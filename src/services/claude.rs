//! Claude CLI runner with streaming JSON parsing via docker exec.

use crate::config::get_settings;
use crate::init::InitState;
use crate::models::ResponseBlock;
use crate::services::SessionManager;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// System prompt context for Basil execution environment
fn get_basil_system_prompt() -> &'static str {
    r#"
# Basil Execution Environment

You are running inside an isolated Docker container managed by Basil.

## Your filesystem view:
- /workspace → The user's project directory
- /workspace/.mounts/ → Approved mounts from request_mount tool (e.g. /workspace/.mounts/Desktop)
- That's it. You cannot see anything else on the user's machine.

## What you CANNOT access directly:
- The user's home directory (~/.bashrc, ~/.zshrc, ~/.config, etc.)
- System tools not installed in this container
- Other projects or files outside /workspace
- The user's environment variables

## IMPORTANT: Installing packages

This container can restart at any time. Anything installed directly (e.g. `apt install`, `pip install`, `npm install -g`) is LOST on restart.

**You MUST use the `install_package` MCP tool to install packages persistently.** This tool writes Dockerfile commands that survive restarts. It works for any package manager: apt, pip, cargo, npm, gem, or custom install scripts.

Example: to install ripgrep, call install_package with commands: `RUN apt-get update && apt-get install -y ripgrep`

Do NOT run install commands directly — always use install_package.

## IMPORTANT: Accessing host paths

When the user mentions a path (e.g., "~/data", "/home/user/datasets"), they mean a path on THEIR machine, not inside your container. You cannot access these paths directly.

**You MUST use the `request_mount` MCP tool to request access to host directories.** After user approval, the container auto-restarts and the directory becomes available at `/workspace/.mounts/<name>`.

Do NOT attempt to read/write paths outside /workspace without first requesting a mount.

## All Basil MCP tools:
- `request_mount` — Request access to a host directory. Requires user approval; container auto-restarts.
- `install_package` — Persistently install packages via Dockerfile commands. Requires user approval; container auto-restarts.
- `list_config` — Show project configuration: approved mounts and installed packages.

## Best practices:
- ALWAYS use install_package for any package installation — direct installs are not persistent
- ALWAYS use request_mount for any path outside /workspace — explain why you need it
- Be explicit when asking the user about paths — clarify you need the full path on their machine
- After requesting a mount or install, wait for the container restart to complete before proceeding
"#
}

/// Run Claude CLI via docker exec and stream response blocks
pub async fn run_claude(
    sessions: Arc<SessionManager>,
    session_id: String,
    message: String,
    plan_mode: bool,
    init_state: Arc<InitState>,
) {
    let sender = match sessions.get_sender(&session_id).await {
        Some(s) => s,
        None => {
            tracing::error!("No sender for session {}", session_id);
            return;
        }
    };

    // Container name must exist — callers check is_ready() before spawning us.
    // Fall back to computing it from settings if init_state hasn't stored it yet.
    let container_name = match init_state.get_container_name().await {
        Some(name) => name,
        None => {
            let settings = get_settings();
            crate::docker::get_container_name(&settings.default_working_dir)
        }
    };

    let settings = get_settings();

    // Get working dir and map host path to container path
    let host_working_dir = sessions
        .get_working_dir(&session_id)
        .await
        .unwrap_or_else(|| settings.project_path.clone());

    // Convert host path to container path: /host/project/subdir -> /workspace/subdir
    let working_dir = if host_working_dir.starts_with(&settings.project_path) {
        let relative = host_working_dir
            .strip_prefix(&settings.project_path)
            .unwrap_or("");
        if relative.is_empty() || relative == "/" {
            "/workspace".to_string()
        } else {
            format!("/workspace{}", relative)
        }
    } else {
        "/workspace".to_string()
    };

    let claude_session_id = sessions.get_claude_session_id(&session_id).await;

    // Build claude args
    let mut claude_args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];

    // Append Basil environment context to system prompt
    claude_args.extend([
        "--append-system-prompt".to_string(),
        get_basil_system_prompt().to_string(),
    ]);

    if plan_mode {
        claude_args.extend(["--permission-mode".to_string(), "plan".to_string()]);

        // Allow Basil MCP tools in plan mode — they have their own approval flow via the UI.
        // Mounts go under /workspace/.mounts/ so Claude can read them without extra permissions.
        claude_args.extend([
            "--allowedTools".to_string(),
            "mcp__basil__install_package,mcp__basil__request_mount,mcp__basil__list_config".to_string(),
        ]);
    } else {
        claude_args.push("--dangerously-skip-permissions".to_string());
    }

    if let Some(ref claude_id) = claude_session_id {
        claude_args.extend(["--resume".to_string(), claude_id.clone()]);
    }

    // Build docker exec command
    let mut cmd = Command::new("docker");
    cmd.arg("exec")
        .arg("-i")
        .arg("-w")
        .arg(&working_dir)
        .arg(&container_name)
        .arg("claude")
        .args(&claude_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Create cancel channel
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    sessions.set_cancel_tx(&session_id, cancel_tx).await;

    let result = run_process(
        cmd,
        message,
        sender.clone(),
        sessions.clone(),
        session_id.clone(),
        &mut cancel_rx,
        init_state.clone(),
    )
    .await;

    if let Err(e) = result {
        // Don't send errors during container rebuild — the init overlay handles it
        if init_state.is_ready() {
            let block_id = sessions.next_block_id(&session_id).await;
            let _ = sender
                .send(ResponseBlock::error(block_id, format!("Error: {}", e)))
                .await;
        } else {
            tracing::debug!("Claude process error during rebuild: {}", e);
        }
    }

    sessions.set_processing(&session_id, false).await;
    sessions.update_session(&session_id).await.ok();
}

async fn run_process(
    mut cmd: Command,
    message: String,
    sender: mpsc::Sender<ResponseBlock>,
    sessions: Arc<SessionManager>,
    session_id: String,
    cancel_rx: &mut tokio::sync::oneshot::Receiver<()>,
    init_state: Arc<InitState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut child = cmd.spawn()?;

    // Write message to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(message.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let stdout = child.stdout.take().ok_or("No stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let mut last_text = String::new();

    loop {
        tokio::select! {
            _ = &mut *cancel_rx => {
                // Cancelled - kill process
                child.kill().await.ok();
                let block_id = sessions.next_block_id(&session_id).await;
                sender.send(ResponseBlock::system(block_id, "Stopped by user")).await.ok();
                return Ok(());
            }
            line = reader.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if line.is_empty() {
                            continue;
                        }

                        let data: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(d) => d,
                            Err(_) => continue,
                        };

                        let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

                        match msg_type {
                            "system" => {
                                if data.get("subtype").and_then(|v| v.as_str()) == Some("init") {
                                    if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
                                        sessions.set_claude_session_id(&session_id, sid.to_string()).await;
                                    }
                                }
                            }
                            "assistant" => {
                                if let Some(content_list) = data.get("message")
                                    .and_then(|m| m.get("content"))
                                    .and_then(|c| c.as_array())
                                {
                                    for content in content_list {
                                        let content_type = content.get("type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");

                                        if content_type == "text" {
                                            if let Some(text) = content.get("text").and_then(|v| v.as_str()) {
                                                if !text.is_empty() && text != last_text {
                                                    last_text = text.to_string();
                                                    let block_id = sessions.next_block_id(&session_id).await;
                                                    sender.send(ResponseBlock::text(block_id, text, true)).await.ok();
                                                    sessions.add_message(&session_id, "assistant", text).await;
                                                }
                                            }
                                        } else if content_type == "tool_use" {
                                            let tool_name = content.get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            let tool_input = content.get("input")
                                                .cloned()
                                                .unwrap_or(serde_json::json!({}));
                                            let tool_use_id = content.get("id")
                                                .and_then(|v| v.as_str());

                                            // Interactive tools signal end of processing (user must respond)
                                            let is_interactive = tool_name == "AskUserQuestion" || tool_name == "ExitPlanMode";

                                            let block_id = sessions.next_block_id(&session_id).await;
                                            sender.send(ResponseBlock::tool(block_id, tool_name, tool_input.clone(), tool_use_id, !is_interactive)).await.ok();

                                            // Save tool message
                                            let tool_msg = serde_json::json!({
                                                "tool": tool_name,
                                                "input": tool_input,
                                                "tool_use_id": tool_use_id
                                            });
                                            sessions.add_message(&session_id, "tool", &tool_msg.to_string()).await;

                                            // Stop processing for interactive tools
                                            if is_interactive {
                                                child.kill().await.ok();
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                            "result" => {
                                if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
                                    sessions.set_claude_session_id(&session_id, sid.to_string()).await;
                                }

                                let result_text = data.get("result")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");

                                if !result_text.is_empty() && result_text != last_text {
                                    let block_id = sessions.next_block_id(&session_id).await;
                                    sender.send(ResponseBlock::text(block_id, result_text, false)).await.ok();
                                    sessions.add_message(&session_id, "assistant", result_text).await;
                                } else {
                                    let block_id = sessions.next_block_id(&session_id).await;
                                    sender.send(ResponseBlock::done(block_id)).await.ok();
                                }
                                return Ok(());
                            }
                            "error" => {
                                let error_msg = data.get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown error");

                                let block_id = sessions.next_block_id(&session_id).await;
                                sender.send(ResponseBlock::error(block_id, format!("Error: {}", error_msg))).await.ok();
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => {
                        // EOF - process finished
                        break;
                    }
                    Err(e) => {
                        let block_id = sessions.next_block_id(&session_id).await;
                        sender.send(ResponseBlock::error(block_id, format!("Read error: {}", e))).await.ok();
                        break;
                    }
                }
            }
        }
    }

    // Wait for process with timeout (container restart can kill it)
    let wait_result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        child.wait_with_output(),
    ).await;

    match wait_result {
        Ok(Ok(output)) if !output.status.success() && !output.stderr.is_empty() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't report errors during container rebuild — the init overlay handles it
            if !init_state.is_ready() {
                tracing::debug!("Claude process exited during container rebuild");
            } else {
                let block_id = sessions.next_block_id(&session_id).await;
                sender
                    .send(ResponseBlock::error(
                        block_id,
                        format!("Claude error: {}", &stderr[..stderr.len().min(500)]),
                    ))
                    .await
                    .ok();
            }
        }
        Ok(Err(e)) => {
            if init_state.is_ready() {
                let block_id = sessions.next_block_id(&session_id).await;
                sender.send(ResponseBlock::error(block_id, format!("Process error: {}", e))).await.ok();
            }
        }
        Err(_) => {
            tracing::warn!("Timed out waiting for Claude process to exit");
        }
        _ => {}
    }

    Ok(())
}

/// Stop a running Claude process
pub async fn stop_claude(sessions: Arc<SessionManager>, session_id: &str) -> bool {
    sessions.cancel(session_id).await
}
