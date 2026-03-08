//! Docker container management - start/stop warm container for Claude CLI execution.

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::BuildImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const BASE_IMAGE: &str = "claude-code-runner";
const CONTAINER_PREFIX: &str = "claude";

// ============================================================================
// Basil Config Types
// ============================================================================

/// Basil project configuration stored in .basil/config.json
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BasilConfig {
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

/// Mount configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
    pub host: String,
    pub target: String,
    #[serde(default = "default_true")]
    pub readonly: bool,
    #[serde(default)]
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Load Basil configuration from .basil/config.json
pub fn load_basil_config(claude_dir: &Path) -> Result<BasilConfig, Box<dyn std::error::Error>> {
    let config_path = claude_dir.join("config.json");
    if !config_path.exists() {
        return Ok(BasilConfig::default());
    }
    let content = std::fs::read_to_string(&config_path)?;
    let config: BasilConfig = serde_json::from_str(&content)?;
    Ok(config)
}

/// Get container name from project path
pub fn get_container_name(project_dir: &Path) -> String {
    let basename = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    format!("{}-{}", CONTAINER_PREFIX, basename)
}

/// Check if a port is free
pub async fn is_port_free(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok()
}

/// Get port for project (hash-based, range 8100-8199)
pub async fn get_project_port(project_dir: &Path) -> u16 {
    let basename = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");

    let hash: u32 = basename.bytes().map(|b| b as u32).sum();
    let base_port = 8100 + (hash % 100) as u16;

    let mut port = base_port;
    for _ in 0..100 {
        if is_port_free(port).await {
            return port;
        }
        port += 1;
        if port >= 8200 {
            port = 8100;
        }
    }
    base_port
}

/// Get claude dir (.basil in project, or fallback to ~/.local/basil/)
pub fn get_claude_dir(project_dir: &Path) -> PathBuf {
    if is_writable(project_dir) {
        project_dir.join(".basil")
    } else {
        let encoded = project_dir
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "_");
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("basil")
            .join(encoded)
    }
}

fn is_writable(path: &Path) -> bool {
    use std::fs::OpenOptions;
    let test_file = path.join(".basil_write_test");
    if OpenOptions::new()
        .create(true)
        .write(true)
        .open(&test_file)
        .is_ok()
    {
        let _ = std::fs::remove_file(&test_file);
        true
    } else {
        false
    }
}

/// Initialize project - copy credentials from ~/.claude to .basil/
pub fn init_project(project_dir: &Path, port: u16) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let source_claude = home.join(".claude");
    let source_credentials = source_claude.join(".credentials.json");

    if !source_credentials.exists() {
        return Err("~/.claude/.credentials.json not found. Please run: claude login".into());
    }

    let claude_dir = get_claude_dir(project_dir);

    // If already initialized, just refresh credentials and MCP config
    if claude_dir.exists() {
        tracing::debug!("Refreshing credentials");
        std::fs::copy(&source_credentials, claude_dir.join(".credentials.json"))?;

        // Also refresh .claude.json if it exists
        let source_claude_json = home.join(".claude.json");
        if source_claude_json.exists() {
            std::fs::copy(&source_claude_json, claude_dir.join(".claude.json"))?;
        }

        // Always update MCP config (port might have changed)
        inject_mcp_config(&claude_dir, port)?;

        return Ok(claude_dir);
    }

    tracing::debug!("Initializing credentials");

    if let Some(parent) = claude_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if claude_dir.exists() {
        std::fs::remove_dir_all(&claude_dir)?;
    }

    copy_dir_recursive(&source_claude, &claude_dir)?;

    let source_claude_json = home.join(".claude.json");
    if source_claude_json.exists() {
        std::fs::copy(&source_claude_json, claude_dir.join(".claude.json"))?;
    }

    strip_hooks_from_settings(&claude_dir.join("settings.json"));
    strip_hooks_from_settings(&claude_dir.join("settings.local.json"));

    // Inject MCP server configuration
    inject_mcp_config(&claude_dir, port)?;

    // Create CLAUDE.md with environment context
    create_claude_md(&claude_dir)?;

    if is_writable(project_dir) {
        update_gitignore(project_dir);
    }

    tracing::debug!("State dir: {}", claude_dir.display());
    Ok(claude_dir)
}

/// Create CLAUDE.md with Basil environment context
fn create_claude_md(claude_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let claude_md_path = claude_dir.join("CLAUDE.md");

    // Don't overwrite if it exists (user might have customized)
    if claude_md_path.exists() {
        return Ok(());
    }

    let content = r#"# Basil Environment

This project is running inside Basil, an isolated Docker container for secure Claude Code execution.

## Filesystem Access

- `/workspace` → The project directory
- Additional directories can be requested via MCP tools

You do not have access to the user's home directory, system configs, or paths outside /workspace unless explicitly mounted.

## MCP Tools Available

- `request_mount` - Request access to a host directory
- `install_package` - Install packages (apt/pip) persistently
- `list_mounts` - Show configured mounts
- `restart_container` - Apply pending changes

## Path Convention

When the user mentions paths like `~/data` or `/home/user/files`, these are **host paths** on their machine. Use `request_mount` to access them.
"#;

    std::fs::write(&claude_md_path, content)?;
    tracing::debug!("Created CLAUDE.md");
    Ok(())
}

/// Inject MCP server configuration into .claude.json
fn inject_mcp_config(claude_dir: &Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let claude_json_path = claude_dir.join(".claude.json");

    // Load existing config or create new
    let mut config: serde_json::Value = if claude_json_path.exists() {
        let content = std::fs::read_to_string(&claude_json_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Ensure mcpServers exists
    if !config.get("mcpServers").is_some() {
        config["mcpServers"] = serde_json::json!({});
    }

    // Add/update basil MCP server
    config["mcpServers"]["basil"] = serde_json::json!({
        "type": "http",
        "url": format!("http://host.docker.internal:{}/mcp", port)
    });

    // Write back
    let content = serde_json::to_string_pretty(&config)?;
    std::fs::write(&claude_json_path, content)?;

    tracing::debug!("Injected MCP config for port {}", port);
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            let real_path = std::fs::canonicalize(&src_path).unwrap_or(src_path.clone());
            std::fs::copy(&real_path, &dst_path)?;
        }
    }
    Ok(())
}

fn strip_hooks_from_settings(path: &Path) {
    if !path.exists() {
        return;
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                obj.remove("hooks");
                if let Ok(new_content) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(path, new_content);
                }
            }
        }
    }
}

/// Calculate stable hash for project path
fn calculate_path_hash(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    hasher.finish()
}

/// Get custom image name based on project path hash
fn get_custom_image_name(project_path: &Path) -> String {
    let hash = calculate_path_hash(project_path);
    format!("basil-{:x}", hash & 0xFFFFFFFF) // Use lower 32 bits for shorter name
}

/// Build custom image from Dockerfile.extras if it exists
/// Returns the image name to use (custom or base)
async fn build_custom_image(
    docker: &Docker,
    project_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let claude_dir = get_claude_dir(project_path);
    let extras_path = claude_dir.join("Dockerfile.extras");

    if !extras_path.exists() {
        tracing::debug!("No Dockerfile.extras found, using base image");
        return Ok(BASE_IMAGE.to_string());
    }

    let image_name = get_custom_image_name(project_path);
    tracing::info!(
        "Building custom image {} from {}",
        image_name,
        extras_path.display()
    );

    // Read Dockerfile.extras content
    let extras_content = std::fs::read_to_string(&extras_path)?;

    // Create full Dockerfile with FROM base image
    let dockerfile_content = format!("FROM {}\n{}", BASE_IMAGE, extras_content);

    // Create tar archive with the Dockerfile
    let mut tar_builder = tar::Builder::new(Vec::new());

    let dockerfile_bytes = dockerfile_content.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_path("Dockerfile")?;
    header.set_size(dockerfile_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar_builder.append(&header, dockerfile_bytes)?;

    let tar_data = tar_builder.into_inner()?;

    // Build the image
    let build_options = BuildImageOptions {
        t: image_name.clone(),
        rm: true,
        forcerm: true,
        ..Default::default()
    };

    let mut build_stream = docker.build_image(build_options, None, Some(tar_data.into()));

    // Process build output
    while let Some(result) = build_stream.next().await {
        match result {
            Ok(output) => {
                if let Some(stream) = output.stream {
                    // Log build output (trimmed)
                    let msg = stream.trim();
                    if !msg.is_empty() {
                        tracing::debug!("Build: {}", msg);
                    }
                }
                if let Some(error) = output.error {
                    tracing::error!("Build error: {}", error);
                    return Err(format!("Docker build failed: {}", error).into());
                }
            }
            Err(e) => {
                tracing::error!("Build stream error: {}", e);
                return Err(e.into());
            }
        }
    }

    tracing::info!("Custom image built successfully: {}", image_name);
    Ok(image_name)
}

fn update_gitignore(project_dir: &Path) {
    let gitignore = project_dir.join(".gitignore");
    let entry = ".basil/";

    if gitignore.exists() {
        if let Ok(content) = std::fs::read_to_string(&gitignore) {
            if !content
                .lines()
                .any(|l| l.trim() == ".basil/" || l.trim() == ".basil")
            {
                let _ = std::fs::write(&gitignore, format!("{}\n{}", content.trim_end(), entry));
            }
        }
    } else {
        let _ = std::fs::write(&gitignore, format!("{}\n", entry));
    }
}

/// Start a warm container (sleep infinity) for executing Claude CLI
pub async fn start_container(project_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let docker = Docker::connect_with_local_defaults()?;
    docker.ping().await?;

    let container_name = get_container_name(project_dir);
    let claude_dir = get_claude_dir(project_dir);

    // Check if already running
    if is_container_running(&docker, &container_name).await {
        tracing::debug!("Container already running: {}", container_name);
        return Ok(container_name);
    }

    // Build custom image if Dockerfile.extras exists, otherwise use base
    let image_name = match build_custom_image(&docker, project_dir).await {
        Ok(name) => name,
        Err(e) => {
            tracing::warn!("Failed to build custom image, using base: {}", e);
            BASE_IMAGE.to_string()
        }
    };

    // Remove stopped container with same name
    let _ = docker
        .remove_container(
            &container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    // Build mounts - start with default mounts
    let mut mounts = vec![
        Mount {
            target: Some("/workspace".to_string()),
            source: Some(project_dir.to_string_lossy().to_string()),
            typ: Some(MountTypeEnum::BIND),
            ..Default::default()
        },
        Mount {
            target: Some("/home/claude/.claude".to_string()),
            source: Some(claude_dir.to_string_lossy().to_string()),
            typ: Some(MountTypeEnum::BIND),
            ..Default::default()
        },
    ];

    // Add approved extra mounts from config
    if let Ok(config) = load_basil_config(&claude_dir) {
        for mount_config in config.mounts.iter().filter(|m| m.approved) {
            tracing::info!("Adding extra mount: {} → {}", mount_config.host, mount_config.target);
            mounts.push(Mount {
                target: Some(mount_config.target.clone()),
                source: Some(mount_config.host.clone()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(mount_config.readonly),
                ..Default::default()
            });
        }
    }

    // Get current user id for permissions
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let user_str = format!("{}:{}", uid, gid);

    let host_config = HostConfig {
        mounts: Some(mounts),
        auto_remove: Some(true),
        ..Default::default()
    };

    let config: Config<String> = Config {
        image: Some(image_name.clone()),
        working_dir: Some("/workspace".to_string()),
        user: Some(user_str),
        env: Some(vec![
            "HOME=/home/claude".to_string(),
            "CI=1".to_string(),
            "NO_COLOR=1".to_string(),
        ]),
        host_config: Some(host_config),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        tty: Some(false),
        open_stdin: Some(true),
        ..Default::default()
    };

    // Create container
    let options = CreateContainerOptions {
        name: container_name.clone(),
        ..Default::default()
    };
    docker.create_container(Some(options), config).await?;

    // Start container
    docker
        .start_container(&container_name, None::<StartContainerOptions<String>>)
        .await?;

    tracing::info!("Container started: {} (image: {})", container_name, image_name);
    Ok(container_name)
}

/// Stop and remove container
pub async fn stop_container(container_name: &str) {
    if let Ok(docker) = Docker::connect_with_local_defaults() {
        let _ = docker
            .stop_container(container_name, Some(StopContainerOptions { t: 2 }))
            .await;
        tracing::debug!("Container stopped: {}", container_name);
    }
}

async fn is_container_running(docker: &Docker, container_name: &str) -> bool {
    let options = ListContainersOptions::<String> {
        all: false,
        ..Default::default()
    };

    if let Ok(containers) = docker.list_containers(Some(options)).await {
        containers.iter().any(|c| {
            c.names.as_ref().map_or(false, |names| {
                names
                    .iter()
                    .any(|n| n.trim_start_matches('/') == container_name)
            })
        })
    } else {
        false
    }
}
