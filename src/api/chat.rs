// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! Chat endpoints for Claude interaction.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{AppError, Result};
use crate::init::InitState;
use crate::mcp::McpState;
use crate::models::ResponseBlock;
use crate::services::{run_claude, stop_claude, SessionManager};

#[derive(Clone)]
struct ChatState {
    sessions: Arc<SessionManager>,
    init_state: Arc<InitState>,
    mcp_state: Arc<McpState>,
}

#[derive(Deserialize)]
pub struct ChatRequest {
    text: String,
    #[serde(default)]
    plan_mode: bool,
}

#[derive(Serialize)]
struct ChatStartResponse {
    status: &'static str,
    session_id: String,
}

#[derive(Deserialize)]
pub struct NextBlockQuery {
    #[serde(default = "default_timeout")]
    timeout: u64,
}

fn default_timeout() -> u64 {
    30
}

#[derive(Serialize)]
struct StopResponse {
    stopped: bool,
}

/// Simple synchronous chat request
#[derive(Deserialize)]
pub struct SimpleRequest {
    prompt: String,
    #[serde(default = "default_working_dir")]
    working_dir: String,
}

fn default_working_dir() -> String {
    "/workspace".to_string()
}

#[derive(Serialize)]
struct SimpleResponse {
    response: String,
    tools_used: Vec<ToolUsed>,
    session_id: String,
}

#[derive(Serialize)]
struct ToolUsed {
    tool: Option<String>,
    input: Option<serde_json::Value>,
}

fn get_session_header(headers: &HeaderMap) -> Result<String> {
    headers
        .get("x-session")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Internal("Missing X-Session header".to_string()))
}

async fn send_message(
    State(state): State<ChatState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatStartResponse>> {
    let sessions = &state.sessions;
    let session_id = get_session_header(&headers)?;

    // Check if system is ready
    if !state.init_state.is_ready() {
        return Err(AppError::Internal("System is still initializing. Please wait.".to_string()));
    }

    // Ensure session exists
    sessions.get_runtime(&session_id).await?;

    if sessions.is_processing(&session_id).await {
        return Err(AppError::SessionBusy(session_id));
    }

    // Create response channel
    sessions.create_channel(&session_id).await;

    // Save user message
    sessions.add_message(&session_id, "user", &req.text).await;
    sessions.set_processing(&session_id, true).await;

    // Spawn Claude processing task
    let sessions_clone = sessions.clone();
    let session_id_clone = session_id.clone();
    let message = req.text.clone();
    let plan_mode = req.plan_mode;
    let init_state = state.init_state.clone();

    tokio::spawn(async move {
        run_claude(sessions_clone, session_id_clone, message, plan_mode, init_state).await;
    });

    Ok(Json(ChatStartResponse {
        status: "processing",
        session_id,
    }))
}

async fn get_next_block(
    State(state): State<ChatState>,
    headers: HeaderMap,
    Query(query): Query<NextBlockQuery>,
) -> Result<Json<ResponseBlock>> {
    let sessions = &state.sessions;
    let session_id = get_session_header(&headers)?;

    // Check for unsent approvals immediately
    let approvals = state.mcp_state.get_unsent_approvals().await;
    if let Some(block) = approvals.into_iter().next() {
        return Ok(Json(block));
    }

    // Get the receiver Arc
    let rx_arc = match sessions.get_receiver(&session_id).await {
        Some(rx) => rx,
        None => {
            return Ok(Json(ResponseBlock::timeout(
                sessions.is_processing(&session_id).await,
            )));
        }
    };

    let timeout_duration = Duration::from_secs(query.timeout.min(60));
    let deadline = tokio::time::Instant::now() + timeout_duration;

    // Loop: wait for either a chat block or an approval notification
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(Json(ResponseBlock::timeout(
                sessions.is_processing(&session_id).await,
            )));
        }

        let notified = state.mcp_state.approval_notifier().notified();
        tokio::pin!(notified);

        // Try to receive from channel with select
        let result = {
            let mut rx_guard = rx_arc.lock().await;
            if let Some(rx) = rx_guard.as_mut() {
                tokio::select! {
                    block = rx.recv() => Some(block),
                    _ = &mut notified => None,
                    _ = tokio::time::sleep(remaining) => {
                        return Ok(Json(ResponseBlock::timeout(
                            sessions.is_processing(&session_id).await,
                        )));
                    }
                }
            } else {
                return Ok(Json(ResponseBlock::timeout(
                    sessions.is_processing(&session_id).await,
                )));
            }
        };

        match result {
            // Got a chat block
            Some(Some(block)) => return Ok(Json(block)),
            // Channel closed
            Some(None) => {
                // During rebuild, signal clean end so UI exits polling gracefully
                if !state.init_state.is_ready() {
                    return Ok(Json(ResponseBlock::done(0)));
                }
                return Ok(Json(ResponseBlock::timeout(
                    sessions.is_processing(&session_id).await,
                )));
            }
            // Approval notification — check for unsent approvals
            None => {
                let approvals = state.mcp_state.get_unsent_approvals().await;
                if let Some(block) = approvals.into_iter().next() {
                    return Ok(Json(block));
                }
                // Spurious wake or already sent, loop back
            }
        }
    }
}

async fn stop_processing(
    State(state): State<ChatState>,
    headers: HeaderMap,
) -> Result<Json<StopResponse>> {
    let sessions = &state.sessions;
    let session_id = get_session_header(&headers)?;
    let stopped = stop_claude(sessions.clone(), &session_id).await;
    sessions.set_processing(&session_id, false).await;
    Ok(Json(StopResponse { stopped }))
}

/// Simple synchronous chat - creates temp session, runs Claude, returns full response
async fn simple_chat(
    State(state): State<ChatState>,
    Json(req): Json<SimpleRequest>,
) -> Result<Json<SimpleResponse>> {
    let sessions = &state.sessions;

    if !state.init_state.is_ready() {
        return Err(AppError::Internal("System is still initializing. Please wait.".to_string()));
    }

    // Create temporary session
    let data = sessions.create_session(Some(req.working_dir)).await?;
    let session_id = data.session_id.clone();

    // Create channel
    sessions.create_channel(&session_id).await;

    // Get receiver
    let rx_arc = sessions.get_receiver(&session_id).await
        .ok_or_else(|| AppError::Internal("Failed to create channel".to_string()))?;

    sessions.add_message(&session_id, "user", &req.prompt).await;
    sessions.set_processing(&session_id, true).await;

    // Run Claude in background
    let sessions_clone = sessions.clone();
    let session_id_clone = session_id.clone();
    let prompt = req.prompt.clone();
    let init_state = state.init_state.clone();

    tokio::spawn(async move {
        run_claude(sessions_clone, session_id_clone, prompt, false, init_state).await;
    });

    // Collect all responses
    let mut response_text = Vec::new();
    let mut tools_used = Vec::new();

    loop {
        let block = {
            let mut rx_guard = rx_arc.lock().await;
            if let Some(rx) = rx_guard.as_mut() {
                rx.recv().await
            } else {
                break;
            }
        };

        match block {
            Some(block) => {
                match block.block_type.as_str() {
                    "text" if !block.content.is_empty() => {
                        response_text.push(block.content.clone());
                    }
                    "tool" => {
                        tools_used.push(ToolUsed {
                            tool: block.metadata.get("tool").and_then(|v| v.as_str()).map(String::from),
                            input: block.metadata.get("input").cloned(),
                        });
                    }
                    _ => {}
                }

                if !block.more {
                    break;
                }
            }
            None => break,
        }
    }

    Ok(Json(SimpleResponse {
        response: response_text.join("\n"),
        tools_used,
        session_id,
    }))
}

pub fn routes(sessions: Arc<SessionManager>, init_state: Arc<InitState>, mcp_state: Arc<McpState>) -> Router {
    let state = ChatState { sessions, init_state, mcp_state };
    Router::new()
        .route("/chat", post(send_message))
        .route("/chat/next", get(get_next_block))
        .route("/chat/stop", post(stop_processing))
        .with_state(state)
}

/// Simple chat route (mounted at root /)
pub fn simple_chat_route(sessions: Arc<SessionManager>, init_state: Arc<InitState>, mcp_state: Arc<McpState>) -> Router {
    let state = ChatState { sessions, init_state, mcp_state };
    Router::new()
        .route("/", post(simple_chat))
        .with_state(state)
}
