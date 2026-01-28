//! Session and message data models.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String, // "user", "assistant", "tool"
    pub content: String,
    pub timestamp: String,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

/// A block of response from Claude
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseBlock {
    pub block_id: u64,
    pub content: String,
    #[serde(rename = "type")]
    pub block_type: String, // "text", "tool", "error", "done", "system", "timeout"
    pub more: bool,
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ResponseBlock {
    pub fn text(block_id: u64, content: impl Into<String>, more: bool) -> Self {
        Self {
            block_id,
            content: content.into(),
            block_type: "text".to_string(),
            more,
            metadata: HashMap::new(),
        }
    }

    pub fn tool(block_id: u64, tool_name: &str, tool_input: serde_json::Value, more: bool) -> Self {
        let mut metadata = HashMap::new();
        metadata.insert("tool".to_string(), serde_json::Value::String(tool_name.to_string()));
        metadata.insert("input".to_string(), tool_input);
        Self {
            block_id,
            content: format!("Using tool: {}", tool_name),
            block_type: "tool".to_string(),
            more,
            metadata,
        }
    }

    pub fn error(block_id: u64, content: impl Into<String>) -> Self {
        Self {
            block_id,
            content: content.into(),
            block_type: "error".to_string(),
            more: false,
            metadata: HashMap::new(),
        }
    }

    pub fn done(block_id: u64) -> Self {
        Self {
            block_id,
            content: String::new(),
            block_type: "done".to_string(),
            more: false,
            metadata: HashMap::new(),
        }
    }

    pub fn system(block_id: u64, content: impl Into<String>) -> Self {
        Self {
            block_id,
            content: content.into(),
            block_type: "system".to_string(),
            more: false,
            metadata: HashMap::new(),
        }
    }

    pub fn timeout(more: bool) -> Self {
        Self {
            block_id: 0,
            content: "No response yet, try again".to_string(),
            block_type: "timeout".to_string(),
            more,
            metadata: HashMap::new(),
        }
    }
}

/// Session data for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub session_id: String,
    pub working_dir: String,
    pub created_at: String,
    pub claude_session_id: Option<String>,
    pub name: Option<String>,
    pub messages: Vec<Message>,
    pub plan_mode: bool,
}

impl SessionData {
    pub fn new(session_id: String, working_dir: String) -> Self {
        Self {
            session_id,
            working_dir,
            created_at: Utc::now().to_rfc3339(),
            claude_session_id: None,
            name: None,
            messages: Vec::new(),
            plan_mode: true, // Default: plan mode (read-only)
        }
    }
}

/// Session list item (summary for listing)
#[derive(Debug, Clone, Serialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub working_dir: String,
    pub created_at: String,
    pub name: Option<String>,
    pub is_processing: bool,
}
