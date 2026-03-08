//! HTTP API routes.

mod chat;
mod health;
pub mod mcp;
mod session;

use axum::Router;
use crate::mcp::McpState;
use crate::services::SessionManager;
use std::sync::Arc;

pub use chat::simple_chat_route;

/// Build the API router
pub fn api_router(sessions: Arc<SessionManager>, mcp_state: Arc<McpState>) -> Router {
    Router::new()
        .merge(health::routes())
        .merge(mcp::routes(mcp_state))
        .nest("/api",
            Router::new()
                .merge(session::routes(sessions.clone()))
                .merge(chat::routes(sessions))
        )
}
