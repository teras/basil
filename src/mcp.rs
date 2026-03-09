// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! MCP (Model Context Protocol) server implementation.
//!
//! Provides JSON-RPC 2.0 over HTTP for Claude to interact with Basil.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::config::get_settings;
use crate::docker::{self, MountConfig};
use crate::services::SessionManager;

/// MCP protocol version
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Pending mount request waiting for user approval
pub struct PendingMountRequest {
    pub id: String,
    pub host_path: String,
    pub target_path: String,
    pub readonly: bool,
    pub reason: String,
    /// Sends `true` for approved, `false` for rejected
    pub response_tx: oneshot::Sender<bool>,
    /// Extra senders for dedup'd requests (same path requested multiple times)
    pub extra_txs: Vec<oneshot::Sender<bool>>,
}

/// Pending install request waiting for user approval
pub struct PendingInstallRequest {
    pub id: String,
    pub dockerfile_commands: String,
    pub response_tx: oneshot::Sender<bool>,
    /// Extra senders for dedup'd requests (same commands requested multiple times)
    pub extra_txs: Vec<oneshot::Sender<bool>>,
}

/// MCP session state
pub struct McpState {
    sessions: RwLock<HashSet<String>>,
    pending_mounts: RwLock<HashMap<String, PendingMountRequest>>,
    pending_installs: RwLock<HashMap<String, PendingInstallRequest>>,
    /// Serializes config.json reads and writes to prevent race conditions
    config_lock: Mutex<()>,
    /// Init state for tracking rebuild progress
    init_state: Arc<crate::init::InitState>,
    /// Tracks which approval IDs have been sent to the UI via chat/next
    sent_approval_ids: RwLock<HashSet<String>>,
    /// Notifies when new approvals are added
    approval_notify: tokio::sync::Notify,
    /// Serializes container restarts to prevent concurrent stop/start races
    restart_lock: Mutex<()>,
    /// True while a container restart is in progress — blocks approvals from
    /// having side effects (config write + restart trigger).
    restarting: AtomicBool,
    /// Chat session manager — used to auto-resume sessions after container restart
    chat_sessions: Option<Arc<SessionManager>>,
}

impl McpState {
    pub fn new(init_state: Arc<crate::init::InitState>, chat_sessions: Arc<SessionManager>) -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashSet::new()),
            pending_mounts: RwLock::new(HashMap::new()),
            pending_installs: RwLock::new(HashMap::new()),
            config_lock: Mutex::new(()),
            init_state,
            sent_approval_ids: RwLock::new(HashSet::new()),
            approval_notify: tokio::sync::Notify::new(),
            restart_lock: Mutex::new(()),
            restarting: AtomicBool::new(false),
            chat_sessions: Some(chat_sessions),
        })
    }

    pub async fn create_session(&self) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.write().await.insert(session_id.clone());
        session_id
    }

    pub async fn is_valid_session(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains(session_id)
    }

    /// Add a pending mount request
    pub async fn add_pending_mount(&self, request: PendingMountRequest) {
        self.pending_mounts.write().await.insert(request.id.clone(), request);
        self.approval_notify.notify_waiters();
    }

    /// If the same host path is already pending, piggyback on that request.
    pub async fn try_piggyback_mount(&self, host_path: &str) -> Option<oneshot::Receiver<bool>> {
        let mut mounts = self.pending_mounts.write().await;
        for req in mounts.values_mut() {
            if req.host_path == host_path {
                let (tx, rx) = oneshot::channel();
                req.extra_txs.push(tx);
                return Some(rx);
            }
        }
        None
    }

    /// Respond to a pending mount request
    pub async fn respond_to_mount(&self, request_id: &str, approved: bool) -> bool {
        if self.restarting.load(Ordering::Acquire) {
            tracing::debug!("respond_to_mount [{}] BLOCKED (restart in progress)", request_id);
            return false;
        }
        if let Some(request) = self.pending_mounts.write().await.remove(request_id) {
            // Set restarting BEFORE sending on tx — this atomically blocks
            // any other approval from triggering side effects.
            if approved {
                self.restarting.store(true, Ordering::Release);
            }
            tracing::debug!("respond_to_mount [{}] approved={} path={} (+ {} piggyback)", request_id, approved, request.host_path, request.extra_txs.len());
            let _ = request.response_tx.send(approved);
            for tx in request.extra_txs {
                let _ = tx.send(approved);
            }
            self.clear_sent_approval(request_id).await;
            true
        } else {
            tracing::debug!("respond_to_mount [{}] NOT FOUND (already cancelled or responded)", request_id);
            false
        }
    }

    /// Remove a pending mount request (e.g. on timeout)
    pub async fn remove_pending_mount(&self, request_id: &str) {
        self.pending_mounts.write().await.remove(request_id);
    }

    /// Add a pending install request
    pub async fn add_pending_install(&self, request: PendingInstallRequest) {
        self.pending_installs.write().await.insert(request.id.clone(), request);
        self.approval_notify.notify_waiters();
    }

    /// If the same dockerfile commands are already pending, piggyback on that
    /// request instead of creating a duplicate. Returns a receiver if dedup'd.
    pub async fn try_piggyback_install(&self, commands: &str) -> Option<oneshot::Receiver<bool>> {
        let trimmed = commands.trim();
        let mut installs = self.pending_installs.write().await;
        for req in installs.values_mut() {
            if req.dockerfile_commands.trim() == trimmed {
                let (tx, rx) = oneshot::channel();
                req.extra_txs.push(tx);
                return Some(rx);
            }
        }
        None
    }

    /// Respond to a pending install request
    pub async fn respond_to_install(&self, request_id: &str, approved: bool) -> bool {
        if self.restarting.load(Ordering::Acquire) {
            tracing::debug!("respond_to_install [{}] BLOCKED (restart in progress)", request_id);
            return false;
        }
        if let Some(request) = self.pending_installs.write().await.remove(request_id) {
            if approved {
                self.restarting.store(true, Ordering::Release);
            }
            tracing::debug!("respond_to_install [{}] approved={} cmds={} (+ {} piggyback)", request_id, approved, request.dockerfile_commands.lines().next().unwrap_or("?"), request.extra_txs.len());
            let _ = request.response_tx.send(approved);
            // Also notify any dedup'd waiters
            for tx in request.extra_txs {
                let _ = tx.send(approved);
            }
            self.clear_sent_approval(request_id).await;
            true
        } else {
            tracing::debug!("respond_to_install [{}] NOT FOUND (already cancelled or responded)", request_id);
            false
        }
    }

    /// Remove a pending install request (e.g. on timeout)
    pub async fn remove_pending_install(&self, request_id: &str) {
        self.pending_installs.write().await.remove(request_id);
    }

    /// Get pending approvals that haven't been sent to the UI yet.
    /// Returns ResponseBlocks for unsent approvals and marks them as sent.
    pub async fn get_unsent_approvals(&self) -> Vec<crate::models::ResponseBlock> {
        let mut blocks = Vec::new();
        let mut sent = self.sent_approval_ids.write().await;

        for (id, req) in self.pending_mounts.read().await.iter() {
            if sent.insert(id.clone()) {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("approval_type".to_string(), serde_json::json!("mount"));
                metadata.insert("approval_id".to_string(), serde_json::json!(id));
                metadata.insert("host_path".to_string(), serde_json::json!(req.host_path));
                metadata.insert("target_path".to_string(), serde_json::json!(req.target_path));
                metadata.insert("readonly".to_string(), serde_json::json!(req.readonly));
                metadata.insert("reason".to_string(), serde_json::json!(req.reason));
                blocks.push(crate::models::ResponseBlock {
                    block_id: 0,
                    content: String::new(),
                    block_type: "approval".to_string(),
                    more: false,
                    metadata,
                });
            }
        }

        for (id, req) in self.pending_installs.read().await.iter() {
            if sent.insert(id.clone()) {
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("approval_type".to_string(), serde_json::json!("install"));
                metadata.insert("approval_id".to_string(), serde_json::json!(id));
                metadata.insert("dockerfile_commands".to_string(), serde_json::json!(req.dockerfile_commands));
                blocks.push(crate::models::ResponseBlock {
                    block_id: 0,
                    content: String::new(),
                    block_type: "approval".to_string(),
                    more: false,
                    metadata,
                });
            }
        }

        blocks
    }

    /// Clean up sent tracking when an approval is responded to
    async fn clear_sent_approval(&self, request_id: &str) {
        self.sent_approval_ids.write().await.remove(request_id);
    }

    /// Cancel all pending requests (used during container restart).
    /// The Claude CLI process that made these requests is about to be killed,
    /// so their responses can never be delivered. Dropping the senders causes
    /// receivers to get `Err`, which the handlers treat as "cancelled".
    pub async fn cancel_all_pending(&self) {
        let mut mounts = self.pending_mounts.write().await;
        let mut installs = self.pending_installs.write().await;
        let mount_ids: Vec<String> = mounts.keys().cloned().collect();
        let install_ids: Vec<String> = installs.keys().cloned().collect();
        if !mount_ids.is_empty() || !install_ids.is_empty() {
            tracing::info!("cancel_all_pending: mounts={:?} installs={:?}", mount_ids, install_ids);
        } else {
            tracing::debug!("cancel_all_pending: nothing to cancel");
        }
        mounts.clear();
        installs.clear();
        drop(mounts);
        drop(installs);
        self.sent_approval_ids.write().await.clear();
    }

    /// Get a reference to the approval notify
    pub fn approval_notifier(&self) -> &tokio::sync::Notify {
        &self.approval_notify
    }
}

// ============================================================================
// JSON-RPC Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}

// JSON-RPC error codes
const METHOD_NOT_FOUND: i32 = -32601;

// ============================================================================
// Tool Definitions
// ============================================================================

fn get_tool_definitions() -> Value {
    json!([
        {
            "name": "request_mount",
            "description": "Request access to a directory on the USER'S MACHINE (host). You cannot access paths outside /workspace directly - use this tool first. The 'path' parameter is the path on the user's machine (e.g., /home/user/data, ~/datasets). After user approval, the container auto-restarts and the directory becomes accessible at /workspace/.mounts/<name>.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path on the USER'S machine (host system). This is NOT a path you can see - it's where the files are on the user's computer."
                    },
                    "target": {
                        "type": "string",
                        "description": "Where to mount inside the container. If omitted, defaults to /workspace/.mounts/<basename>"
                    },
                    "readonly": {
                        "type": "boolean",
                        "description": "Mount as read-only for safety (default: true)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Explain to the user why you need access to this directory"
                    }
                },
                "required": ["path", "reason"]
            }
        },
        {
            "name": "install_package",
            "description": "Add Dockerfile commands for persistent package installation. Saved to project config and applied automatically after user approval (container auto-restarts). Use standard Dockerfile syntax (RUN, ENV, COPY, etc.). Works for ANY package manager: apt, pip, cargo, npm, rustup, or custom install scripts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dockerfile_commands": {
                        "type": "string",
                        "description": "Raw Dockerfile commands to append. Example: 'RUN apt-get update && apt-get install -y htop' or 'RUN curl -sSf https://sh.rustup.rs | sh -s -- -y\\nENV PATH=/root/.cargo/bin:$PATH'"
                    }
                },
                "required": ["dockerfile_commands"]
            }
        },
        {
            "name": "list_config",
            "description": "Show the project's Basil configuration: approved mounts and installed packages.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false
            }
        }
    ])
}

// ============================================================================
// Request Handlers
// ============================================================================

/// Handle incoming JSON-RPC request
pub async fn handle_request(
    state: Arc<McpState>,
    request: JsonRpcRequest,
    _session_id: Option<String>,
) -> (JsonRpcResponse, Option<String>) {
    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => handle_initialize(state, id).await,
        "tools/list" => (handle_tools_list(id), None),
        "tools/call" => (handle_tools_call(state, id, request.params).await, None),
        _ => (
            JsonRpcResponse::error(id, METHOD_NOT_FOUND, &format!("Unknown method: {}", request.method)),
            None,
        ),
    }
}

async fn handle_initialize(
    state: Arc<McpState>,
    id: Option<Value>,
) -> (JsonRpcResponse, Option<String>) {
    let session_id = state.create_session().await;

    let result = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "serverInfo": {
            "name": "basil",
            "version": env!("CARGO_PKG_VERSION")
        },
        "capabilities": {
            "tools": {}
        }
    });

    (JsonRpcResponse::success(id, result), Some(session_id))
}

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    let result = json!({
        "tools": get_tool_definitions()
    });
    JsonRpcResponse::success(id, result)
}

async fn handle_tools_call(state: Arc<McpState>, id: Option<Value>, params: Value) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    tracing::debug!("MCP tools/call: {} args={}", name, arguments);

    let result = match name {
        "request_mount" => call_request_mount(state.clone(), arguments).await,
        "install_package" => call_install_package(state.clone(), arguments).await,
        "list_config" => call_list_config().await,
        _ => Err(format!("Unknown tool: {}", name)),
    };

    match &result {
        Ok(text) => tracing::debug!("MCP tools/call {} → OK: {}", name, &text[..text.len().min(120)]),
        Err(e) => tracing::debug!("MCP tools/call {} → ERR: {}", name, &e[..e.len().min(120)]),
    }

    match result {
        Ok(text) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{"type": "text", "text": text}],
                "isError": false
            }),
        ),
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{"type": "text", "text": e}],
                "isError": true
            }),
        ),
    }
}

// ============================================================================
// Tool Implementations
// ============================================================================

#[derive(Debug, Deserialize)]
struct MountRequest {
    path: String,
    target: Option<String>,
    readonly: Option<bool>,
    reason: String,
}

async fn call_request_mount(state: Arc<McpState>, args: Value) -> Result<String, String> {
    let mut req: MountRequest = serde_json::from_value(args)
        .map_err(|e| format!("Invalid arguments: {}", e))?;

    // Expand ~ to absolute path (Docker requires absolute mount paths)
    req.path = docker::expand_tilde(&req.path);

    if !req.path.starts_with('/') {
        return Err(format!("Mount path must be absolute, got: {}", req.path));
    }

    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);

    let target = req.target.clone().unwrap_or_else(|| {
        let basename = std::path::Path::new(&req.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("mount");
        format!("/workspace/.mounts/{}", basename)
    });
    let readonly = req.readonly.unwrap_or(true);

    // Quick check if already mounted (the write path has its own lock + double-check)
    {
        let claude_dir2 = claude_dir.clone();
        let path = req.path.clone();
        let config = tokio::task::spawn_blocking(move || {
            docker::load_basil_config(&claude_dir2)
                .map_err(|e| format!("Failed to load config: {}", e))
        }).await.map_err(|e| format!("Task join error: {}", e))??;
        if config.mounts.iter().any(|m| m.host == path && m.approved) {
            tracing::debug!("Mount already in config, skipping: {}", req.path);
            return Ok(format!("Mount already approved: {} → {}", req.path, target));
        }
    }

    // Dedup: if the same path is already pending approval, piggyback
    if let Some(rx) = state.try_piggyback_mount(&req.path).await {
        tracing::info!("Dedup'd mount request for {}, waiting on existing approval", req.path);
        return match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(true)) => Ok(format!(
                "✓ Mount approved: {} → {} ({}). Container is restarting...",
                req.path, target,
                if readonly { "read-only" } else { "read-write" },
            )),
            Ok(Ok(false)) => Err(format!("✗ Mount request rejected by user: {} → {}", req.path, target)),
            _ => Err("Mount request was cancelled".to_string()),
        };
    }

    // Create oneshot channel for response
    let (tx, rx) = oneshot::channel();
    let request_id = uuid::Uuid::new_v4().to_string();

    // Add pending request
    let pending = PendingMountRequest {
        id: request_id.clone(),
        host_path: req.path.clone(),
        target_path: target.clone(),
        readonly,
        reason: req.reason.clone(),
        response_tx: tx,
        extra_txs: Vec::new(),
    };
    state.add_pending_mount(pending).await;

    tracing::info!("Mount request pending approval: {} → {}", req.path, target);

    // Wait for user response (with timeout)
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        rx
    ).await;

    match response {
        Ok(Ok(approved)) => {
            if approved {
                // Hold lock to prevent concurrent config writes
                let _lock = state.config_lock.lock().await;

                let mount_host = req.path.clone();
                let mount_target = target.clone();
                let mount_reason = req.reason.clone();
                let claude_dir_inner = claude_dir.clone();
                tokio::task::spawn_blocking(move || {
                    let mut config = docker::load_basil_config(&claude_dir_inner)
                        .map_err(|e| format!("Failed to load config: {}", e))?;

                    if !config.mounts.iter().any(|m| m.host == mount_host && m.approved) {
                        config.mounts.push(MountConfig {
                            host: mount_host,
                            target: mount_target,
                            readonly,
                            approved: true,
                            reason: Some(mount_reason),
                        });

                        let config_path = claude_dir_inner.join("config.json");
                        tracing::info!("Writing mount config to: {}", config_path.display());
                        let content = serde_json::to_string_pretty(&config)
                            .map_err(|e| format!("Failed to serialize config: {}", e))?;
                        std::fs::write(&config_path, &content)
                            .map_err(|e| format!("Failed to write config: {}", e))?;
                        tracing::info!("Mount config written OK: {}", content);
                    }
                    Ok::<(), String>(())
                }).await.map_err(|e| format!("Task join error: {}", e))??;

                tracing::info!("Auto-restarting container to apply mount...");
                spawn_restart(&state, false);
                Ok(format!(
                    "✓ Mount approved: {} → {} ({}). Container is restarting...",
                    req.path, target,
                    if readonly { "read-only" } else { "read-write" },
                ))
            } else {
                Err(format!("✗ Mount request rejected by user: {} → {}", req.path, target))
            }
        }
        Ok(Err(_)) => {
            Err("Mount request was cancelled".to_string())
        }
        Err(_) => {
            // Timeout - clean up pending request (no-op if already responded)
            state.remove_pending_mount(&request_id).await;
            Err("Mount request timed out (5 minutes). Please try again.".to_string())
        }
    }
}

#[derive(Debug, Deserialize)]
struct InstallPackageRequest {
    dockerfile_commands: String,
}

async fn call_install_package(state: Arc<McpState>, args: Value) -> Result<String, String> {
    let req: InstallPackageRequest = serde_json::from_value(args)
        .map_err(|e| format!("Invalid arguments: {}", e))?;

    if req.dockerfile_commands.trim().is_empty() {
        return Err("No dockerfile_commands provided.".to_string());
    }

    // Check if the same commands are already installed in config
    {
        let settings = get_settings();
        let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
        let commands_trimmed = req.dockerfile_commands.trim().to_string();
        let config = tokio::task::spawn_blocking(move || {
            docker::load_basil_config(&claude_dir)
                .map_err(|e| format!("Failed to load config: {}", e))
        }).await.map_err(|e| format!("Task join error: {}", e))??;
        if config.packages.iter().any(|p| p.approved && p.commands.trim() == commands_trimmed) {
            tracing::debug!("Install already in config, skipping: {}", commands_trimmed.lines().next().unwrap_or("?"));
            return Ok(format!("Already installed: {}", req.dockerfile_commands.lines().next().unwrap_or("(empty)")));
        }
    }

    // Dedup: if the same commands are already pending approval, piggyback
    // on that request instead of showing a second popup to the user.
    if let Some(rx) = state.try_piggyback_install(&req.dockerfile_commands).await {
        tracing::info!("Install dedup: piggybacking on existing pending request for: {}", req.dockerfile_commands.lines().next().unwrap_or("?"));
        return match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(true)) => Ok(format!(
                "✓ Install approved:\n```dockerfile\n{}\n```\nContainer is restarting...",
                req.dockerfile_commands.trim(),
            )),
            Ok(Ok(false)) => Err("✗ Install request rejected by user.".to_string()),
            _ => Err("Install request was cancelled".to_string()),
        };
    }

    // Create oneshot channel for approval
    let (tx, rx) = oneshot::channel();
    let request_id = uuid::Uuid::new_v4().to_string();

    let pending = PendingInstallRequest {
        id: request_id.clone(),
        dockerfile_commands: req.dockerfile_commands.clone(),
        response_tx: tx,
        extra_txs: Vec::new(),
    };
    state.add_pending_install(pending).await;

    tracing::info!("Install request pending approval [{}]: {}", request_id, req.dockerfile_commands.lines().next().unwrap_or("(empty)"));

    // Wait for user response (with timeout)
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        rx
    ).await;

    tracing::debug!("Install request [{}] got response: {:?}", request_id, response.as_ref().map(|r| r.as_ref().map(|v| *v)));

    match response {
        Ok(Ok(true)) => {
            let settings = get_settings();
            let claude_dir = docker::get_claude_dir(&settings.default_working_dir);

            // Hold lock to prevent concurrent read-modify-write
            let _lock = state.config_lock.lock().await;

            let commands = req.dockerfile_commands.clone();
            tokio::task::spawn_blocking(move || {
                let mut config = docker::load_basil_config(&claude_dir)
                    .map_err(|e| format!("Failed to load config: {}", e))?;

                config.packages.push(docker::PackageConfig {
                    commands,
                    approved: true,
                });

                let config_path = claude_dir.join("config.json");
                tracing::info!("Writing install config to: {}", config_path.display());
                let content = serde_json::to_string_pretty(&config)
                    .map_err(|e| format!("Failed to serialize config: {}", e))?;
                std::fs::write(&config_path, &content)
                    .map_err(|e| format!("Failed to write config: {}", e))?;
                tracing::info!("Install config written OK");
                Ok::<(), String>(())
            }).await.map_err(|e| format!("Task join error: {}", e))??;

            tracing::info!("Auto-restarting container to apply install changes...");
            spawn_restart(&state, true);
            Ok(format!(
                "✓ Install approved:\n```dockerfile\n{}\n```\nContainer is restarting...",
                req.dockerfile_commands.trim(),
            ))
        }
        Ok(Ok(false)) => {
            Err("✗ Install request rejected by user.".to_string())
        }
        Ok(Err(_)) => {
            Err("Install request was cancelled".to_string())
        }
        Err(_) => {
            state.remove_pending_install(&request_id).await;
            Err("Install request timed out (5 minutes). Please try again.".to_string())
        }
    }
}

async fn call_list_config() -> Result<String, String> {
    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);

    let config = tokio::task::spawn_blocking(move || {
        docker::load_basil_config(&claude_dir)
            .map_err(|e| format!("Failed to load config: {}", e))
    }).await.map_err(|e| format!("Task join error: {}", e))??;

    if config.mounts.is_empty() && config.packages.is_empty() {
        return Ok("No mounts or packages configured.".to_string());
    }

    let mut output = String::new();

    if !config.mounts.is_empty() {
        output.push_str("Mounts:\n");
        for mount in &config.mounts {
            let status = if mount.approved { "✓" } else { "⏳" };
            let mode = if mount.readonly { "ro" } else { "rw" };
            output.push_str(&format!(
                "  {} {} → {} ({})\n",
                status, mount.host, mount.target, mode
            ));
        }
    }

    if !config.packages.is_empty() {
        if !output.is_empty() { output.push('\n'); }
        output.push_str("Packages:\n");
        for pkg in &config.packages {
            let status = if pkg.approved { "✓" } else { "⏳" };
            let summary = pkg.commands.lines().next().unwrap_or("(empty)");
            output.push_str(&format!("  {} {}\n", status, summary));
        }
    }

    Ok(output)
}

/// Build a continuation message that includes the full current state
/// (installed packages and mounts) so Claude knows exactly what's available.
fn build_continuation_message(settings: &crate::config::Settings) -> String {
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
    let config = docker::load_basil_config(&claude_dir).unwrap_or_default();

    let mut msg = String::from("Container restarted successfully. Current environment state:\n");

    if config.packages.iter().any(|p| p.approved) {
        msg.push_str("\nInstalled packages:\n");
        for pkg in config.packages.iter().filter(|p| p.approved) {
            msg.push_str(&format!("  - {}\n", pkg.commands.trim()));
        }
    }

    if config.mounts.iter().any(|m| m.approved) {
        msg.push_str("\nMounted directories:\n");
        for m in config.mounts.iter().filter(|m| m.approved) {
            let mode = if m.readonly { "read-only" } else { "read-write" };
            msg.push_str(&format!("  - {} → {} ({})\n", m.host, m.target, mode));
        }
    }

    msg.push_str("\nAll the above are already applied. Do NOT re-request them. Continue with any remaining work.");
    msg
}

/// Signal not-ready and spawn the restart as an independent task.
/// Must be called before returning the MCP response, because the MCP handler
/// will be cancelled when stop_container kills the Claude CLI process.
///
/// Always does a full rebuild via `start_container_fresh` — `build_project_image`
/// is smart enough to skip the image build when there are no packages, and Docker
/// layer caching makes rebuilds fast when nothing changed. This avoids bugs where
/// the caller's intent (mount-only vs install) is stale by the time the restart
/// actually runs (e.g. an install was approved in the meantime).
fn spawn_restart(state: &Arc<McpState>, _rebuild_image: bool) {
    // Mark not-ready synchronously so the UI sees it immediately,
    // before the spawned task starts or the MCP response is sent.
    state.init_state.set_not_ready_sync();

    let state = state.clone();
    tokio::spawn(async move {
        use crate::init::InitPhase;
        use crate::services::run_claude;

        tracing::debug!("spawn_restart: waiting for restart_lock...");
        let _restart_guard = state.restart_lock.lock().await;
        tracing::debug!("spawn_restart: acquired restart_lock");

        // restarting flag is already set by respond_to_mount/respond_to_install
        // before sending on the channel. Re-assert it here as a safety net.
        state.restarting.store(true, Ordering::Release);

        // Capture which sessions are currently processing BEFORE stopping the
        // container — once the container dies the Claude processes exit and
        // set is_processing=false, so we'd lose this information.
        let interrupted_sessions = if let Some(ref sessions) = state.chat_sessions {
            sessions.get_processing_sessions().await
        } else {
            Vec::new()
        };
        tracing::debug!("spawn_restart: interrupted_sessions={:?}", interrupted_sessions.iter().map(|(id, mode)| (id.as_str(), *mode)).collect::<Vec<_>>());

        let init_state = &state.init_state;
        init_state.set_not_ready_sync();
        init_state.clear_for_rebuild().await;

        let settings = get_settings();
        let project_dir = &settings.default_working_dir;
        let container_name = docker::get_container_name(project_dir);

        init_state.set_phase(InitPhase::BuildingProjectImage).await;

        // Stop current container (with timeout to avoid hanging forever)
        let stop_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            async { docker::stop_container(&container_name).await },
        ).await;
        if stop_result.is_err() {
            tracing::error!("Timed out stopping container {}", container_name);
        }

        // Cancel all pending requests — covers both pre-existing requests AND
        // late arrivals from the dying Claude CLI. At this point the container
        // is stopped so no new MCP requests can arrive.
        state.cancel_all_pending().await;

        // Always do a fresh start — build_project_image skips the image build
        // when there are no packages, and Docker cache makes it fast otherwise.
        // This ensures any config changes (mounts OR packages) approved between
        // the spawn_restart call and now are always picked up.
        let result = docker::start_container_fresh(project_dir, Some(init_state.clone())).await;

        match result {
            Ok(name) => {
                // Prepare auto-resume BEFORE marking ready, so the UI sees
                // is_processing=true as soon as it starts polling after rebuild.
                let mut continuations = Vec::new();
                if let Some(ref sessions) = state.chat_sessions {
                    for (session_id, plan_mode) in &interrupted_sessions {
                        // Wait for the old run_claude to finish cleaning up
                        // (it sets is_processing=false after the process exits)
                        for _ in 0..50 {
                            if !sessions.is_processing(session_id).await {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }

                        // Build a detailed continuation that tells Claude exactly
                        // what's currently installed and mounted, so it doesn't
                        // re-request things that are already done.
                        let continuation = build_continuation_message(&settings);
                        tracing::debug!("spawn_restart: continuation message for {}: {:?}", session_id, &continuation[..continuation.len().min(200)]);

                        sessions.create_channel(session_id).await;
                        sessions.add_message(session_id, "user", &continuation).await;
                        sessions.set_processing(session_id, true).await;

                        continuations.push((session_id.clone(), plan_mode, continuation));
                    }
                }

                // Allow approvals again before marking ready — new Claude
                // process will make fresh MCP requests that need to work.
                state.restarting.store(false, Ordering::Release);

                init_state.set_ready(name).await;
                tracing::info!("Container restarted successfully");

                // Now spawn the actual Claude processes
                if let Some(ref sessions) = state.chat_sessions {
                    for (session_id, plan_mode, continuation) in continuations {
                        tracing::info!("Auto-resuming session {} after container restart", session_id);
                        let sessions_clone = sessions.clone();
                        let init_clone = init_state.clone();
                        let plan = *plan_mode;
                        tokio::spawn(async move {
                            run_claude(
                                sessions_clone,
                                session_id,
                                continuation,
                                plan,
                                init_clone,
                            ).await;
                        });
                    }
                }
            }
            Err(e) => {
                state.restarting.store(false, Ordering::Release);
                let msg = format!("Failed to restart container: {}", e);
                tracing::error!("{}", msg);
                init_state.set_failed(msg).await;
            }
        }
    });
}
