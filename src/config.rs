//! Configuration management.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Global settings instance
static SETTINGS: OnceLock<Settings> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub serve_ui: bool,
    pub project_name: String,
    pub project_path: String,
    pub default_working_dir: PathBuf,
    pub session_dir: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            host: "0.0.0.0".to_string(),
            port: 8035,
            serve_ui: false,
            project_name: "project".to_string(),
            project_path: String::new(),
            default_working_dir: home.clone(),
            session_dir: home.join(".basil").join("sessions"),
        }
    }
}

/// Initialize global settings
pub fn init_settings(settings: Settings) {
    // Ensure session directory exists
    std::fs::create_dir_all(&settings.session_dir).ok();
    SETTINGS.set(settings).ok();
}

/// Get global settings
pub fn get_settings() -> &'static Settings {
    SETTINGS.get().expect("Settings not initialized")
}
