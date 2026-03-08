//! MCP HTTP endpoint handler and mount management.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config::get_settings;
use crate::docker::{self, BasilConfig, MountConfig};
use crate::mcp::{handle_request, JsonRpcRequest, JsonRpcResponse, McpState};

/// MCP routes
pub fn routes(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(mcp_handler))
        .route("/api/mounts", get(list_mounts))
        .route("/api/mounts/:index/approve", patch(approve_mount))
        .route("/api/mounts/:index/reject", patch(reject_mount))
        .with_state(state)
}

/// Handle MCP JSON-RPC requests
async fn mcp_handler(
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    // Extract session ID from headers (optional for initialize)
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // For non-initialize requests, validate session
    if request.method != "initialize" {
        if let Some(ref sid) = session_id {
            if !state.is_valid_session(sid).await {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(JsonRpcResponse::error(
                        request.id,
                        -32000,
                        "Invalid or expired session",
                    )),
                )
                    .into_response();
            }
        }
    }

    // Handle the request
    let (response, new_session_id) = handle_request(state, request, session_id).await;

    // Build response with optional session ID header
    if let Some(sid) = new_session_id {
        (
            StatusCode::OK,
            [("mcp-session-id", sid)],
            Json(response),
        )
            .into_response()
    } else {
        (StatusCode::OK, Json(response)).into_response()
    }
}

// ============================================================================
// Mount Management Endpoints
// ============================================================================

#[derive(Serialize)]
struct MountsResponse {
    mounts: Vec<MountWithIndex>,
    pending_count: usize,
}

#[derive(Serialize)]
struct MountWithIndex {
    index: usize,
    host: String,
    target: String,
    readonly: bool,
    approved: bool,
    reason: Option<String>,
}

#[derive(Serialize)]
struct MountActionResponse {
    ok: bool,
    message: String,
}

/// List all mounts (pending and approved)
async fn list_mounts(
    State(_state): State<Arc<McpState>>,
) -> Result<Json<MountsResponse>, StatusCode> {
    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);

    let config = docker::load_basil_config(&claude_dir)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mounts: Vec<MountWithIndex> = config
        .mounts
        .iter()
        .enumerate()
        .map(|(i, m)| MountWithIndex {
            index: i,
            host: m.host.clone(),
            target: m.target.clone(),
            readonly: m.readonly,
            approved: m.approved,
            reason: m.reason.clone(),
        })
        .collect();

    let pending_count = mounts.iter().filter(|m| !m.approved).count();

    Ok(Json(MountsResponse {
        mounts,
        pending_count,
    }))
}

/// Approve a mount request
async fn approve_mount(
    State(_state): State<Arc<McpState>>,
    Path(index): Path<usize>,
) -> Result<Json<MountActionResponse>, StatusCode> {
    update_mount_approval(index, true).await
}

/// Reject (remove) a mount request
async fn reject_mount(
    State(_state): State<Arc<McpState>>,
    Path(index): Path<usize>,
) -> Result<Json<MountActionResponse>, StatusCode> {
    update_mount_approval(index, false).await
}

async fn update_mount_approval(
    index: usize,
    approve: bool,
) -> Result<Json<MountActionResponse>, StatusCode> {
    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
    let config_path = claude_dir.join("config.json");

    let mut config = docker::load_basil_config(&claude_dir)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if index >= config.mounts.len() {
        return Ok(Json(MountActionResponse {
            ok: false,
            message: "Mount not found".to_string(),
        }));
    }

    if approve {
        config.mounts[index].approved = true;
        let mount = &config.mounts[index];
        let message = format!("Approved: {} → {}", mount.host, mount.target);

        // Save config
        let content = serde_json::to_string_pretty(&config)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        std::fs::write(&config_path, content)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok(Json(MountActionResponse {
            ok: true,
            message,
        }))
    } else {
        // Remove the mount request
        let mount = config.mounts.remove(index);
        let message = format!("Rejected: {} → {}", mount.host, mount.target);

        // Save config
        let content = serde_json::to_string_pretty(&config)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        std::fs::write(&config_path, content)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        Ok(Json(MountActionResponse {
            ok: true,
            message,
        }))
    }
}
