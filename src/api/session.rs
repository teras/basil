//! Session management endpoints.

use axum::{
    extract::{Path, State},
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{AppError, Result};
use crate::services::SessionManager;

#[derive(Deserialize)]
pub struct NewSessionRequest {
    working_dir: Option<String>,
}

#[derive(Serialize)]
struct NewSessionResponse {
    session_id: String,
    working_dir: String,
    name: Option<String>,
    status: &'static str,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<crate::models::SessionListItem>,
}

#[derive(Serialize)]
struct SessionInfoResponse {
    session_id: String,
    working_dir: String,
    created_at: String,
    name: Option<String>,
    is_processing: bool,
    messages: Vec<crate::models::Message>,
    plan_mode: bool,
}

#[derive(Deserialize)]
pub struct RenameRequest {
    name: String,
}

#[derive(Deserialize)]
pub struct SetModeRequest {
    plan_mode: bool,
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_mode: Option<bool>,
}

#[derive(Serialize)]
struct DeleteResponse {
    deleted: bool,
}

async fn create_session(
    State(sessions): State<Arc<SessionManager>>,
    Json(req): Json<NewSessionRequest>,
) -> Result<Json<NewSessionResponse>> {
    let data = sessions.create_session(req.working_dir).await?;
    Ok(Json(NewSessionResponse {
        session_id: data.session_id,
        working_dir: data.working_dir,
        name: data.name,
        status: "ready",
    }))
}

async fn list_sessions(
    State(sessions): State<Arc<SessionManager>>,
) -> Result<Json<SessionListResponse>> {
    let list = sessions.list_sessions().await?;
    Ok(Json(SessionListResponse { sessions: list }))
}

async fn get_session_info(
    State(sessions): State<Arc<SessionManager>>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionInfoResponse>> {
    let data = sessions.get_session(&session_id).await?;
    let is_processing = sessions.is_processing(&session_id).await;

    Ok(Json(SessionInfoResponse {
        session_id: data.session_id,
        working_dir: data.working_dir,
        created_at: data.created_at,
        name: data.name,
        is_processing,
        messages: data.messages,
        plan_mode: data.plan_mode,
    }))
}

async fn delete_session_handler(
    State(sessions): State<Arc<SessionManager>>,
    Path(session_id): Path<String>,
) -> Result<Json<DeleteResponse>> {
    let deleted = sessions.delete_session(&session_id).await?;
    if deleted {
        Ok(Json(DeleteResponse { deleted: true }))
    } else {
        Err(AppError::SessionNotFound(session_id))
    }
}

async fn rename_session(
    State(sessions): State<Arc<SessionManager>>,
    Path(session_id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> Result<Json<OkResponse>> {
    sessions.rename_session(&session_id, req.name.clone()).await?;
    Ok(Json(OkResponse {
        ok: true,
        name: Some(req.name),
        plan_mode: None,
    }))
}

async fn set_session_mode(
    State(sessions): State<Arc<SessionManager>>,
    Path(session_id): Path<String>,
    Json(req): Json<SetModeRequest>,
) -> Result<Json<OkResponse>> {
    sessions.set_mode(&session_id, req.plan_mode).await?;
    Ok(Json(OkResponse {
        ok: true,
        name: None,
        plan_mode: Some(req.plan_mode),
    }))
}

pub fn routes(sessions: Arc<SessionManager>) -> Router {
    Router::new()
        .route("/session/new", post(create_session))
        .route("/session/list", get(list_sessions))
        .route("/session/{session_id}", get(get_session_info))
        .route("/session/{session_id}", delete(delete_session_handler))
        .route("/session/{session_id}/rename", patch(rename_session))
        .route("/session/{session_id}/mode", patch(set_session_mode))
        .with_state(sessions)
}
