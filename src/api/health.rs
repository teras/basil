//! Health check, project info, and initialization status endpoints.

use axum::{extract::State, routing::get, Json, Router};
use crate::config::get_settings;
use crate::init::{InitState, InitStatusResponse};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

#[derive(Serialize)]
struct ProjectResponse {
    name: String,
    path: String,
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "basil",
    })
}

async fn project_info() -> Json<ProjectResponse> {
    let settings = get_settings();
    Json(ProjectResponse {
        name: settings.project_name.clone(),
        path: settings.project_path.clone(),
    })
}

async fn init_status(State(init_state): State<Arc<InitState>>) -> Json<InitStatusResponse> {
    Json(init_state.status().await)
}

pub fn routes() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/project", get(project_info))
}

pub fn status_route(init_state: Arc<InitState>) -> Router {
    Router::new()
        .route("/status", get(init_status))
        .with_state(init_state)
}
