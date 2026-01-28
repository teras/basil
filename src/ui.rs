//! Embedded web UI serving.

use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};

/// Embedded UI HTML (loaded at compile time)
const UI_HTML: &str = include_str!("../assets/ui.html");

async fn serve_ui() -> impl IntoResponse {
    Html(UI_HTML)
}

/// Create UI route (GET /)
pub fn ui_route() -> Router {
    Router::new().route("/", get(serve_ui))
}
