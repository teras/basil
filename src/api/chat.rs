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
use tokio::time::timeout;

use crate::error::{AppError, Result};
use crate::models::ResponseBlock;
use crate::services::{run_claude, stop_claude, SessionManager};

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
    State(sessions): State<Arc<SessionManager>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatStartResponse>> {
    let session_id = get_session_header(&headers)?;

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

    tokio::spawn(async move {
        run_claude(sessions_clone, session_id_clone, message, plan_mode).await;
    });

    Ok(Json(ChatStartResponse {
        status: "processing",
        session_id,
    }))
}

async fn get_next_block(
    State(sessions): State<Arc<SessionManager>>,
    headers: HeaderMap,
    Query(query): Query<NextBlockQuery>,
) -> Result<Json<ResponseBlock>> {
    let session_id = get_session_header(&headers)?;

    // Get the receiver Arc
    let rx_arc = match sessions.get_receiver(&session_id).await {
        Some(rx) => rx,
        None => {
            return Ok(Json(ResponseBlock::timeout(
                sessions.is_processing(&session_id).await,
            )));
        }
    };

    // Lock the receiver and wait for a message
    let timeout_duration = Duration::from_secs(query.timeout.min(60));

    let result = {
        let mut rx_guard = rx_arc.lock().await;
        if let Some(rx) = rx_guard.as_mut() {
            timeout(timeout_duration, rx.recv()).await
        } else {
            return Ok(Json(ResponseBlock::timeout(
                sessions.is_processing(&session_id).await,
            )));
        }
    };

    match result {
        Ok(Some(block)) => Ok(Json(block)),
        Ok(None) => Ok(Json(ResponseBlock::timeout(
            sessions.is_processing(&session_id).await,
        ))),
        Err(_) => Ok(Json(ResponseBlock::timeout(
            sessions.is_processing(&session_id).await,
        ))),
    }
}

async fn stop_processing(
    State(sessions): State<Arc<SessionManager>>,
    headers: HeaderMap,
) -> Result<Json<StopResponse>> {
    let session_id = get_session_header(&headers)?;
    let stopped = stop_claude(sessions.clone(), &session_id).await;
    sessions.set_processing(&session_id, false).await;
    Ok(Json(StopResponse { stopped }))
}

/// Simple synchronous chat - creates temp session, runs Claude, returns full response
async fn simple_chat(
    State(sessions): State<Arc<SessionManager>>,
    Json(req): Json<SimpleRequest>,
) -> Result<Json<SimpleResponse>> {
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

    tokio::spawn(async move {
        run_claude(sessions_clone, session_id_clone, prompt, false).await;
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

pub fn routes(sessions: Arc<SessionManager>) -> Router {
    Router::new()
        .route("/chat", post(send_message))
        .route("/chat/next", get(get_next_block))
        .route("/chat/stop", post(stop_processing))
        .with_state(sessions)
}

/// Simple chat route (mounted at root /)
pub fn simple_chat_route(sessions: Arc<SessionManager>) -> Router {
    Router::new()
        .route("/", post(simple_chat))
        .with_state(sessions)
}
