//! Health check and project info endpoints.

use axum::{routing::get, Json, Router};
use crate::config::get_settings;
use serde::Serialize;

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

pub fn routes() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/project", get(project_info))
}
