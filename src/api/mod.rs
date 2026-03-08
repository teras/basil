//! HTTP API routes.

mod chat;
mod health;
pub mod mcp;
mod session;

use axum::Router;
use crate::init::InitState;
use crate::mcp::McpState;
use crate::services::SessionManager;
use std::sync::Arc;

/// Build the API router
pub fn api_router(sessions: Arc<SessionManager>, mcp_state: Arc<McpState>, init_state: Arc<InitState>) -> Router {
    Router::new()
        .merge(health::routes())
        .merge(mcp::routes(mcp_state.clone()))
        .nest("/api",
            Router::new()
                .merge(session::routes(sessions.clone()))
                .merge(chat::routes(sessions.clone(), init_state.clone(), mcp_state))
                .merge(health::status_route(init_state))
        )
}

pub fn simple_chat_route(sessions: Arc<SessionManager>, init_state: Arc<InitState>, mcp_state: Arc<McpState>) -> Router {
    chat::simple_chat_route(sessions, init_state, mcp_state)
}
