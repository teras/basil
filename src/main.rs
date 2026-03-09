// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! Basil - HTTP API bridge for Claude Code CLI
//!
//! Single binary distribution with embedded UI. Docker used for isolated CLI execution.

mod api;
mod config;
mod docker;
mod error;
mod init;
mod mcp;
mod models;
mod services;
mod ui;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, Uri},
    middleware::{self, Next},
    response::Response,
    Router,
};
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{fmt::time::LocalTime, layer::SubscriberExt, util::SubscriberInitExt, Layer};

use config::{get_settings, init_settings, Settings};
use init::{InitPhase, InitState};
use services::SessionManager;

#[derive(Parser, Debug)]
#[command(name = "basil")]
#[command(about = "HTTP API bridge for Claude Code CLI")]
#[command(version)]
struct Args {
    /// Project directory
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Port to listen on
    #[arg(short, long)]
    port: Option<u16>,

    /// Disable web UI (enabled by default)
    #[arg(long)]
    no_ui: bool,

    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    /// Write logs to file (enables debug level automatically)
    #[arg(long, value_name = "FILE")]
    log_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Initialize logging
    let has_log_file = args.log_file.is_some();
    let log_level = if args.debug || has_log_file { "basil=debug" } else { "basil=info" };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| log_level.into());
    let time_format = LocalTime::new(time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second]"
    ));

    if let Some(ref log_path) = args.log_file {
        let log_file = std::fs::File::create(log_path).unwrap_or_else(|e| {
            eprintln!("Cannot create log file {}: {}", log_path.display(), e);
            std::process::exit(1);
        });
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_timer(time_format.clone())
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(log_file)),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_timer(time_format)
                    .with_filter(tracing_subscriber::EnvFilter::new("basil=info")),
            )
            .init();
        eprintln!("Logging to {}", log_path.display());
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_timer(time_format),
            )
            .init();
    }

    // Canonicalize project path
    let project_dir = match std::fs::canonicalize(&args.path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot access {}: {}", args.path.display(), e);
            std::process::exit(1);
        }
    };

    // Compute port
    let port = match args.port {
        Some(p) => p,
        None => docker::get_project_port(&project_dir).await,
    };

    // Create init state (shared between server and background init)
    let init_state = Arc::new(InitState::new());

    // Start server immediately, then init Docker in background
    let init_state_bg = init_state.clone();
    let project_dir_bg = project_dir.clone();
    tokio::spawn(async move {
        run_init(init_state_bg, &project_dir_bg, port).await;
    });

    // Run server (available immediately with init status)
    run_server(&project_dir, port, !args.no_ui, init_state).await;
}

/// Background initialization - Docker setup
async fn run_init(init_state: Arc<InitState>, project_dir: &PathBuf, port: u16) {
    // Init project (copy credentials, inject MCP config)
    init_state.set_phase(InitPhase::InitProject).await;
    let init_err = docker::init_project(project_dir, port).err().map(|e| e.to_string());
    if let Some(msg) = init_err {
        tracing::error!("{}", msg);
        init_state.set_failed(msg).await;
        return;
    }

    // Start container (includes base image build if needed)
    init_state.set_phase(InitPhase::StartingContainer).await;
    let container_result = docker::start_container(project_dir, Some(init_state.clone())).await
        .map_err(|e| e.to_string());
    match container_result {
        Ok(name) => {
            // Ctrl+C handler - stop container on exit
            let cn = name.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                docker::stop_container(&cn).await;
                std::process::exit(0);
            });

            init_state.set_ready(name).await;
            tracing::info!("Initialization complete - system ready");
        }
        Err(msg) => {
            let msg = format!("Failed to start container: {}", msg);
            tracing::error!("{}", msg);
            init_state.set_failed(msg).await;
        }
    }
}

/// Custom request logging middleware
async fn request_logging(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    request: Request<Body>,
    next: Next,
) -> Response {
    let start = Instant::now();
    let path = uri.path().to_string();
    let ip = addr.ip();

    let response = next.run(request).await;

    let status = response.status();
    let latency = start.elapsed().as_millis();

    let msg = format!("{ip} {method} {path} → {status} ({latency}ms)");

    if path == "/health" || path == "/api/health" || path == "/api/status" {
        tracing::trace!("{msg}");
    } else if status.is_success() {
        tracing::info!("{msg}");
    } else if status.is_client_error() {
        tracing::warn!("{msg}");
    } else {
        tracing::error!("{msg}");
    }

    response
}

async fn run_server(project_dir: &PathBuf, port: u16, serve_ui: bool, init_state: Arc<InitState>) {
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    // Build settings
    let settings = Settings {
        host: "0.0.0.0".to_string(),
        port,
        serve_ui,
        project_name: project_name.clone(),
        project_path: project_dir.to_string_lossy().to_string(),
        default_working_dir: project_dir.clone(),
        session_dir: docker::get_claude_dir(project_dir).join("sessions"),
    };

    init_settings(settings);
    let settings = get_settings();

    // Create session manager and MCP state
    let sessions = SessionManager::new();
    let mcp_state = mcp::McpState::new(init_state.clone(), sessions.clone());

    // Build router
    let mut app = Router::new()
        .merge(api::api_router(sessions.clone(), mcp_state.clone(), init_state.clone()))
        .merge(api::simple_chat_route(sessions.clone(), init_state, mcp_state));

    // Add UI route if enabled
    if settings.serve_ui {
        app = app.merge(ui::ui_route());
    }

    // Add middleware
    app = app
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(middleware::from_fn(request_logging));

    // Start server
    let addr: std::net::SocketAddr = format!("{}:{}", settings.host, settings.port)
        .parse()
        .expect("Invalid address");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!("Listening on http://{}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
