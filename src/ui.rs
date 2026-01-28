//! Embedded web UI serving.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};

/// Embedded UI HTML (loaded at compile time)
const UI_HTML: &str = include_str!("../assets/ui.html");

/// Embedded assets (loaded at compile time)
const STYLE_CSS: &str = include_str!("../assets/style.css");
const APP_JS: &str = include_str!("../assets/app.js");
const MARKED_JS: &str = include_str!("../assets/vendor/marked.min.js");
const HIGHLIGHT_JS: &str = include_str!("../assets/vendor/highlight.min.js");
const HIGHLIGHT_CSS: &str = include_str!("../assets/vendor/github-dark.min.css");

async fn serve_ui() -> impl IntoResponse {
    Html(UI_HTML)
}

async fn serve_asset(Path(path): Path<String>) -> Response {
    match path.as_str() {
        "style.css" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/css")],
            STYLE_CSS,
        )
            .into_response(),
        "vendor/marked.min.js" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/javascript")],
            MARKED_JS,
        )
            .into_response(),
        "vendor/highlight.min.js" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/javascript")],
            HIGHLIGHT_JS,
        )
            .into_response(),
        "vendor/github-dark.min.css" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/css")],
            HIGHLIGHT_CSS,
        )
            .into_response(),
        "app.js" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/javascript")],
            APP_JS,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// Create UI routes (GET / and GET /assets/*)
pub fn ui_route() -> Router {
    Router::new()
        .route("/", get(serve_ui))
        .route("/assets/{*path}", get(serve_asset))
}
