//! Docker container management - start/stop warm container for Claude CLI execution.

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use std::path::{Path, PathBuf};

const IMAGE_NAME: &str = "claude-code-runner";
const CONTAINER_PREFIX: &str = "claude";

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
pub fn init_project(project_dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let source_claude = home.join(".claude");
    let source_credentials = source_claude.join(".credentials.json");

    if !source_credentials.exists() {
        return Err("~/.claude/.credentials.json not found. Please run: claude login".into());
    }

    let claude_dir = get_claude_dir(project_dir);

    // If already initialized, just refresh credentials file
    if claude_dir.exists() {
        tracing::debug!("Refreshing credentials");
        std::fs::copy(&source_credentials, claude_dir.join(".credentials.json"))?;

        // Also refresh .claude.json if it exists
        let source_claude_json = home.join(".claude.json");
        if source_claude_json.exists() {
            std::fs::copy(&source_claude_json, claude_dir.join(".claude.json"))?;
        }

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

    if is_writable(project_dir) {
        update_gitignore(project_dir);
    }

    tracing::debug!("State dir: {}", claude_dir.display());
    Ok(claude_dir)
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

    // Build mounts
    let mounts = vec![
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
        image: Some(IMAGE_NAME.to_string()),
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

    tracing::debug!("Container started: {}", container_name);
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
