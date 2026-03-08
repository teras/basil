//! MCP (Model Context Protocol) server implementation.
//!
//! Provides JSON-RPC 2.0 over HTTP for Claude to interact with Basil.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::get_settings;
use crate::docker::{self, BasilConfig, MountConfig};

/// MCP protocol version
const PROTOCOL_VERSION: &str = "2025-06-18";

/// MCP session state
#[derive(Default)]
pub struct McpState {
    sessions: RwLock<HashMap<String, McpSession>>,
}

#[derive(Clone)]
struct McpSession {
    initialized: bool,
}

impl McpState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn create_session(&self) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.write().await.insert(
            session_id.clone(),
            McpSession { initialized: true },
        );
        session_id
    }

    pub async fn is_valid_session(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }
}

// ============================================================================
// JSON-RPC Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
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
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;

// ============================================================================
// Tool Definitions
// ============================================================================

fn get_tool_definitions() -> Value {
    json!([
        {
            "name": "request_mount",
            "description": "Request access to a directory on the USER'S MACHINE (host). You cannot access paths outside /workspace directly - use this tool first. The 'path' parameter is the path on the user's machine (e.g., /home/user/data, ~/datasets). After user approval and container restart, the directory will be accessible inside your environment.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path on the USER'S machine (host system). This is NOT a path you can see - it's where the files are on the user's computer."
                    },
                    "target": {
                        "type": "string",
                        "description": "Where to mount inside the container (e.g., /data). If omitted, defaults to /mnt/<basename>"
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
            "description": "Install system packages (apt) or Python packages (pip) PERSISTENTLY. Unlike running apt-get directly, packages installed via this tool survive container restarts. Use this when you need tools that aren't available in the base container.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "apt": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "System packages to install via apt-get (e.g., ['htop', 'jq', 'ffmpeg'])"
                    },
                    "pip": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Python packages to install via pip (e.g., ['pandas', 'numpy', 'requests'])"
                    }
                }
            }
        },
        {
            "name": "list_mounts",
            "description": "Show all configured mounts for this project. Displays both approved (active) and pending (awaiting user approval) mounts.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "restart_container",
            "description": "Restart the container to apply pending changes. Call this AFTER: 1) User approves a mount request, or 2) You add packages via install_package. The container will restart with new mounts and packages available.",
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
    session_id: Option<String>,
) -> (JsonRpcResponse, Option<String>) {
    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => handle_initialize(state, id).await,
        "tools/list" => (handle_tools_list(id), None),
        "tools/call" => (handle_tools_call(id, request.params).await, None),
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

async fn handle_tools_call(id: Option<Value>, params: Value) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "request_mount" => call_request_mount(arguments).await,
        "install_package" => call_install_package(arguments).await,
        "list_mounts" => call_list_mounts().await,
        "restart_container" => call_restart_container().await,
        _ => Err(format!("Unknown tool: {}", name)),
    };

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

async fn call_request_mount(args: Value) -> Result<String, String> {
    let req: MountRequest = serde_json::from_value(args)
        .map_err(|e| format!("Invalid arguments: {}", e))?;

    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
    let config_path = claude_dir.join("config.json");

    // Load existing config or create new
    let mut config: BasilConfig = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config: {}", e))?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        BasilConfig::default()
    };

    // Check if already mounted
    if config.mounts.iter().any(|m| m.host == req.path && m.approved) {
        return Ok(format!("Mount already approved: {} → {}", req.path,
            req.target.as_deref().unwrap_or(&req.path)));
    }

    // Add pending mount request
    let target = req.target.unwrap_or_else(|| req.path.clone());
    let mount = MountConfig {
        host: req.path.clone(),
        target: target.clone(),
        readonly: req.readonly.unwrap_or(true),
        approved: false,
        reason: Some(req.reason.clone()),
    };

    // Check if already pending
    if !config.mounts.iter().any(|m| m.host == req.path) {
        config.mounts.push(mount);

        // Save config
        let content = serde_json::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        std::fs::write(&config_path, content)
            .map_err(|e| format!("Failed to write config: {}", e))?;
    }

    Ok(format!(
        "Mount request submitted for approval:\n  {} → {} ({})\n  Reason: {}\n\nThe user will be prompted to approve this request in the Basil UI.",
        req.path,
        target,
        if req.readonly.unwrap_or(true) { "read-only" } else { "read-write" },
        req.reason
    ))
}

#[derive(Debug, Deserialize)]
struct InstallPackageRequest {
    apt: Option<Vec<String>>,
    pip: Option<Vec<String>>,
}

async fn call_install_package(args: Value) -> Result<String, String> {
    let req: InstallPackageRequest = serde_json::from_value(args)
        .map_err(|e| format!("Invalid arguments: {}", e))?;

    if req.apt.is_none() && req.pip.is_none() {
        return Err("No packages specified. Use 'apt' and/or 'pip' arrays.".to_string());
    }

    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
    let extras_path = claude_dir.join("Dockerfile.extras");

    // Read existing content or start fresh
    let mut content = if extras_path.exists() {
        std::fs::read_to_string(&extras_path)
            .map_err(|e| format!("Failed to read Dockerfile.extras: {}", e))?
    } else {
        String::new()
    };

    let mut added = Vec::new();

    // Add apt packages
    if let Some(apt_packages) = &req.apt {
        if !apt_packages.is_empty() {
            let apt_line = format!(
                "\nRUN apt-get update && apt-get install -y {}\n",
                apt_packages.join(" ")
            );
            content.push_str(&apt_line);
            added.push(format!("apt: {}", apt_packages.join(", ")));
        }
    }

    // Add pip packages
    if let Some(pip_packages) = &req.pip {
        if !pip_packages.is_empty() {
            let pip_line = format!("\nRUN pip3 install {}\n", pip_packages.join(" "));
            content.push_str(&pip_line);
            added.push(format!("pip: {}", pip_packages.join(", ")));
        }
    }

    // Write updated content
    std::fs::write(&extras_path, content)
        .map_err(|e| format!("Failed to write Dockerfile.extras: {}", e))?;

    Ok(format!(
        "Packages added to Dockerfile.extras:\n  {}\n\nRun 'restart_container' tool to rebuild and apply changes.",
        added.join("\n  ")
    ))
}

async fn call_list_mounts() -> Result<String, String> {
    let settings = get_settings();
    let claude_dir = docker::get_claude_dir(&settings.default_working_dir);
    let config_path = claude_dir.join("config.json");

    if !config_path.exists() {
        return Ok("No mounts configured.".to_string());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config: {}", e))?;
    let config: BasilConfig = serde_json::from_str(&content).unwrap_or_default();

    if config.mounts.is_empty() {
        return Ok("No mounts configured.".to_string());
    }

    let mut output = String::from("Configured mounts:\n");
    for mount in &config.mounts {
        let status = if mount.approved { "✓" } else { "⏳" };
        let mode = if mount.readonly { "ro" } else { "rw" };
        output.push_str(&format!(
            "  {} {} → {} ({})\n",
            status, mount.host, mount.target, mode
        ));
    }

    Ok(output)
}

async fn call_restart_container() -> Result<String, String> {
    let settings = get_settings();
    let container_name = &settings.container_name;
    let project_dir = &settings.default_working_dir;

    // Stop current container
    docker::stop_container(container_name).await;

    // Start new container (will pick up new config)
    match docker::start_container(project_dir).await {
        Ok(_) => Ok("Container restarted successfully. New mounts and packages are now available.".to_string()),
        Err(e) => Err(format!("Failed to restart container: {}", e)),
    }
}

// Config types are imported from docker module
