//! HTTP API routes.

mod health;
mod session;
mod chat;

use axum::Router;
use crate::services::SessionManager;
use std::sync::Arc;

pub use chat::simple_chat_route;

/// Build the API router
pub fn api_router(sessions: Arc<SessionManager>) -> Router {
    Router::new()
        .merge(health::routes())
        .nest("/api",
            Router::new()
                .merge(session::routes(sessions.clone()))
                .merge(chat::routes(sessions))
        )
}
