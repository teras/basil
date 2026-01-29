//! Basil - HTTP API bridge for Claude Code CLI
//!
//! Single binary distribution with embedded UI. Docker used for isolated CLI execution.

mod api;
mod config;
mod docker;
mod error;
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
use std::{net::SocketAddr, path::PathBuf, time::Instant};
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{fmt::time::LocalTime, layer::SubscriberExt, util::SubscriberInitExt};

use config::{get_settings, init_settings, Settings};
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
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.debug { "basil=debug" } else { "basil=info" };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_timer(LocalTime::new(time::macros::format_description!(
                    "[year]-[month]-[day] [hour]:[minute]:[second]"
                ))),
        )
        .init();

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

    // Init project (copy credentials)
    if let Err(e) = docker::init_project(&project_dir) {
        tracing::error!("{}", e);
        std::process::exit(1);
    }

    // Start container
    let container_name = match docker::start_container(&project_dir).await {
        Ok(name) => name,
        Err(e) => {
            tracing::error!("Failed to start container: {}", e);
            std::process::exit(1);
        }
    };

    // Ctrl+C handler - stop container on exit
    let cn = container_name.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        docker::stop_container(&cn).await;
        std::process::exit(0);
    });

    // Run server
    run_server(&project_dir, port, !args.no_ui, container_name).await;
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

    if path == "/health" || path == "/api/health" {
        tracing::debug!("{msg}");
    } else if status.is_success() {
        tracing::info!("{msg}");
    } else if status.is_client_error() {
        tracing::warn!("{msg}");
    } else {
        tracing::error!("{msg}");
    }

    response
}

async fn run_server(project_dir: &PathBuf, port: u16, serve_ui: bool, container_name: String) {
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
        container_name: container_name.clone(),
    };

    init_settings(settings);
    let settings = get_settings();

    // Create session manager
    let sessions = SessionManager::new();

    // Build router
    let mut app = Router::new()
        .merge(api::api_router(sessions.clone()))
        .merge(api::simple_chat_route(sessions.clone()));

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
