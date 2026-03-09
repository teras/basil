// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! Embedded web UI serving.

use axum::{
    http::header,
    response::{Html, IntoResponse},
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

/// Create UI routes
pub fn ui_route() -> Router {
    Router::new()
        .route("/", get(serve_ui))
        .route("/assets/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], STYLE_CSS) }))
        .route("/assets/app.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript")], APP_JS) }))
        .route("/assets/vendor/marked.min.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript")], MARKED_JS) }))
        .route("/assets/vendor/highlight.min.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript")], HIGHLIGHT_JS) }))
        .route("/assets/vendor/github-dark.min.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], HIGHLIGHT_CSS) }))
}
