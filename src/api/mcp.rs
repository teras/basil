// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! MCP HTTP endpoint handler.

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::mcp::{handle_request, JsonRpcRequest, JsonRpcResponse, McpState};

/// MCP routes
pub fn routes(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(mcp_handler))
        .route("/api/mounts/{id}/respond", patch(respond_to_mount))
        .route("/api/installs/{id}/respond", patch(respond_to_install))
        .with_state(state)
}

/// Handle MCP JSON-RPC requests (private network only — loopback + Docker bridge)
async fn mcp_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    // Only allow private IPs (loopback for local, Docker bridge for containers)
    if !is_private_ip(addr.ip()) {
        return (StatusCode::FORBIDDEN).into_response();
    }

    // Extract session ID from headers (optional for initialize)
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // For non-initialize requests, require a valid session
    if request.method != "initialize" {
        let valid = match &session_id {
            Some(sid) => state.is_valid_session(sid).await,
            None => false,
        };
        if !valid {
            return (
                StatusCode::UNAUTHORIZED,
                Json(JsonRpcResponse::error(
                    request.id,
                    -32000,
                    "Invalid or missing session",
                )),
            )
                .into_response();
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
// Approval Endpoints
// ============================================================================

#[derive(Deserialize)]
struct ApprovalResponseBody {
    approved: bool,
}

#[derive(Serialize)]
struct ApprovalResponseResult {
    ok: bool,
    message: String,
}

/// Respond to a mount request (approve or reject, localhost only)
async fn respond_to_mount(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<McpState>>,
    Path(id): Path<String>,
    Json(body): Json<ApprovalResponseBody>,
) -> Result<Json<ApprovalResponseResult>, StatusCode> {
    require_loopback(addr)?;
    let ok = state.respond_to_mount(&id, body.approved).await;
    Ok(Json(approval_result(ok, "Mount", body.approved)))
}

/// Respond to an install request (approve or reject, localhost only)
async fn respond_to_install(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<McpState>>,
    Path(id): Path<String>,
    Json(body): Json<ApprovalResponseBody>,
) -> Result<Json<ApprovalResponseResult>, StatusCode> {
    require_loopback(addr)?;
    let ok = state.respond_to_install(&id, body.approved).await;
    Ok(Json(approval_result(ok, "Install", body.approved)))
}

fn require_loopback(addr: SocketAddr) -> Result<(), StatusCode> {
    if addr.ip().is_loopback() { Ok(()) } else { Err(StatusCode::FORBIDDEN) }
}

fn approval_result(ok: bool, label: &str, approved: bool) -> ApprovalResponseResult {
    let action = if approved { "approved" } else { "rejected" };
    let message = if ok {
        format!("{} request {}", label, action)
    } else {
        format!("{} request not found or already {}", label, action)
    };
    ApprovalResponseResult { ok, message }
}

/// Check if an IP is private (loopback, link-local, or RFC1918/Docker bridge)
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()       // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()     // 169.254.0.0/16
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()                                    // ::1
                || (v6.segments()[0] & 0xfe00) == 0xfc00        // ULA fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80        // link-local fe80::/10
        }
    }
}
