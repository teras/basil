//! Session management with persistence.

use crate::config::get_settings;
use crate::error::{AppError, Result};
use crate::models::{Message, ResponseBlock, SessionData, SessionListItem};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

/// Runtime state for an active session
pub struct SessionRuntime {
    pub data: SessionData,
    pub response_tx: Option<mpsc::Sender<ResponseBlock>>,
    pub response_rx: Arc<Mutex<Option<mpsc::Receiver<ResponseBlock>>>>,
    pub is_processing: bool,
    pub current_block_id: u64,
    pub cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SessionRuntime {
    pub fn new(data: SessionData) -> Self {
        Self {
            data,
            response_tx: None,
            response_rx: Arc::new(Mutex::new(None)),
            is_processing: false,
            current_block_id: 0,
            cancel_tx: None,
        }
    }

    pub fn next_block_id(&mut self) -> u64 {
        self.current_block_id += 1;
        self.current_block_id
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        self.data.messages.push(Message::new(role, content));
    }

    /// Create a new response channel for this session
    pub async fn create_channel(&mut self) {
        let (tx, rx) = mpsc::channel(100);
        self.response_tx = Some(tx);
        *self.response_rx.lock().await = Some(rx);
    }
}

/// Manages chat sessions
pub struct SessionManager {
    sessions: RwLock<HashMap<String, SessionRuntime>>,
}

impl SessionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Create a new session
    pub async fn create_session(&self, working_dir: Option<String>) -> Result<SessionData> {
        let settings = get_settings();
        let session_id = uuid::Uuid::new_v4().to_string()[..12].to_string();

        let working_dir = working_dir.unwrap_or_else(|| {
            settings.default_working_dir.to_string_lossy().to_string()
        });

        // Generate default name
        let sessions_list = self.list_sessions().await?;
        let count = sessions_list.len() + 1;
        let default_name = format!("{} #{}", settings.project_name, count);

        let mut data = SessionData::new(session_id.clone(), working_dir);
        data.name = Some(default_name);

        // Save to disk
        self.save_session_data(&data)?;

        // Store in memory
        let runtime = SessionRuntime::new(data.clone());
        self.sessions.write().await.insert(session_id, runtime);

        Ok(data)
    }

    /// Get a session by ID
    pub async fn get_session(&self, session_id: &str) -> Result<SessionData> {
        // Check memory first
        if let Some(runtime) = self.sessions.read().await.get(session_id) {
            return Ok(runtime.data.clone());
        }

        // Try to load from disk
        if let Some(data) = self.load_session_data(session_id)? {
            let runtime = SessionRuntime::new(data.clone());
            self.sessions.write().await.insert(session_id.to_string(), runtime);
            return Ok(data);
        }

        Err(AppError::SessionNotFound(session_id.to_string()))
    }

    /// Get session runtime (for processing)
    pub async fn get_runtime(&self, session_id: &str) -> Result<()> {
        // Ensure session is loaded
        self.get_session(session_id).await?;
        Ok(())
    }

    /// Check if session is processing
    pub async fn is_processing(&self, session_id: &str) -> bool {
        self.sessions.read().await
            .get(session_id)
            .map(|r| r.is_processing)
            .unwrap_or(false)
    }

    /// Set session processing state
    pub async fn set_processing(&self, session_id: &str, processing: bool) {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            runtime.is_processing = processing;
        }
    }

    /// Get next block ID for session
    pub async fn next_block_id(&self, session_id: &str) -> u64 {
        self.sessions.write().await
            .get_mut(session_id)
            .map(|r| r.next_block_id())
            .unwrap_or(0)
    }

    /// Add message to session
    pub async fn add_message(&self, session_id: &str, role: &str, content: &str) {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            runtime.add_message(role, content);
        }
    }

    /// Get/set Claude session ID
    pub async fn get_claude_session_id(&self, session_id: &str) -> Option<String> {
        self.sessions.read().await
            .get(session_id)
            .and_then(|r| r.data.claude_session_id.clone())
    }

    pub async fn set_claude_session_id(&self, session_id: &str, claude_id: String) {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            runtime.data.claude_session_id = Some(claude_id);
        }
    }

    /// Get working directory
    pub async fn get_working_dir(&self, session_id: &str) -> Option<String> {
        self.sessions.read().await
            .get(session_id)
            .map(|r| r.data.working_dir.clone())
    }

    /// Create response channel for session
    pub async fn create_channel(&self, session_id: &str) {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            runtime.create_channel().await;
        }
    }

    /// Get response sender for session
    pub async fn get_sender(&self, session_id: &str) -> Option<mpsc::Sender<ResponseBlock>> {
        self.sessions.read().await
            .get(session_id)
            .and_then(|r| r.response_tx.clone())
    }

    /// Get the receiver Arc for a session (for receiving blocks)
    pub async fn get_receiver(&self, session_id: &str) -> Option<Arc<Mutex<Option<mpsc::Receiver<ResponseBlock>>>>> {
        self.sessions.read().await
            .get(session_id)
            .map(|r| r.response_rx.clone())
    }

    /// Store cancel sender for session
    pub async fn set_cancel_tx(&self, session_id: &str, tx: tokio::sync::oneshot::Sender<()>) {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            runtime.cancel_tx = Some(tx);
        }
    }

    /// Cancel running process
    pub async fn cancel(&self, session_id: &str) -> bool {
        if let Some(runtime) = self.sessions.write().await.get_mut(session_id) {
            if let Some(tx) = runtime.cancel_tx.take() {
                let _ = tx.send(());
                return true;
            }
        }
        false
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<SessionListItem>> {
        let settings = get_settings();
        let mut sessions = Vec::new();

        if !settings.session_dir.exists() {
            return Ok(sessions);
        }

        let entries = std::fs::read_dir(&settings.session_dir)?;
        let memory_sessions = self.sessions.read().await;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<SessionData>(&content) {
                        let is_processing = memory_sessions
                            .get(&data.session_id)
                            .map(|r| r.is_processing)
                            .unwrap_or(false);

                        sessions.push(SessionListItem {
                            session_id: data.session_id,
                            working_dir: data.working_dir,
                            created_at: data.created_at,
                            name: data.name,
                            is_processing,
                        });
                    }
                }
            }
        }

        // Sort by created_at descending
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    /// Update session (save to disk)
    pub async fn update_session(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        if let Some(runtime) = sessions.get(session_id) {
            self.save_session_data(&runtime.data)?;
        }
        Ok(())
    }

    /// Rename session
    pub async fn rename_session(&self, session_id: &str, name: String) -> Result<()> {
        {
            let mut sessions = self.sessions.write().await;
            if let Some(runtime) = sessions.get_mut(session_id) {
                runtime.data.name = Some(name);
            } else {
                return Err(AppError::SessionNotFound(session_id.to_string()));
            }
        }
        self.update_session(session_id).await
    }

    /// Set session mode
    pub async fn set_mode(&self, session_id: &str, plan_mode: bool) -> Result<()> {
        {
            let mut sessions = self.sessions.write().await;
            if let Some(runtime) = sessions.get_mut(session_id) {
                runtime.data.plan_mode = plan_mode;
            } else {
                return Err(AppError::SessionNotFound(session_id.to_string()));
            }
        }
        self.update_session(session_id).await
    }

    /// Delete session
    pub async fn delete_session(&self, session_id: &str) -> Result<bool> {
        let settings = get_settings();
        let path = settings.session_dir.join(format!("{}.json", session_id));

        // Get claude_session_id before deleting
        let claude_session_id = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id)
                .and_then(|r| r.data.claude_session_id.clone())
        };

        // Remove from memory
        self.sessions.write().await.remove(session_id);

        // Delete Claude's session if exists
        if let Some(claude_id) = claude_session_id {
            self.delete_claude_session(&claude_id);
        }

        // Delete file
        if path.exists() {
            std::fs::remove_file(&path)?;
            return Ok(true);
        }

        Ok(false)
    }

    fn delete_claude_session(&self, claude_session_id: &str) {
        if let Some(home) = dirs::home_dir() {
            let projects_dir = home.join(".claude").join("projects");
            if projects_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&projects_dir) {
                    for entry in entries.flatten() {
                        let sessions_dir = entry.path().join(".sessions");
                        let session_file = sessions_dir.join(format!("{}.json", claude_session_id));
                        if session_file.exists() {
                            if let Err(e) = std::fs::remove_file(&session_file) {
                                tracing::warn!("Failed to delete Claude session: {}", e);
                            } else {
                                tracing::info!("Deleted Claude session: {:?}", session_file);
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        get_settings().session_dir.join(format!("{}.json", session_id))
    }

    fn save_session_data(&self, data: &SessionData) -> Result<()> {
        let path = self.session_path(&data.session_id);
        let content = serde_json::to_string_pretty(data)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn load_session_data(&self, session_id: &str) -> Result<Option<SessionData>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let data = serde_json::from_str(&content)?;
        Ok(Some(data))
    }
}
