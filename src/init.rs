// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Panayotis Katsaloulis

//! Initialization state tracking for background Docker setup.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitPhase {
    Starting,
    InitProject,
    BuildingBaseImage,
    BuildingProjectImage,
    StartingContainer,
    Ready,
    Failed,
}

impl InitPhase {
    pub fn label(&self) -> &'static str {
        match self {
            InitPhase::Starting => "Starting up...",
            InitPhase::InitProject => "Setting up project...",
            InitPhase::BuildingBaseImage => "Installing tools & dependencies...",
            InitPhase::BuildingProjectImage => "Applying project customizations...",
            InitPhase::StartingContainer => "Launching environment...",
            InitPhase::Ready => "Ready",
            InitPhase::Failed => "Initialization failed",
        }
    }
}

pub struct InitState {
    phase: RwLock<InitPhase>,
    message: RwLock<String>,
    ready: AtomicBool,
    error: RwLock<Option<String>>,
    container_name: RwLock<Option<String>>,
    logs: RwLock<Vec<String>>,
    progress: AtomicU8, // 0-100
}

#[derive(Serialize)]
pub struct InitStatusResponse {
    pub phase: InitPhase,
    pub message: String,
    pub ready: bool,
    pub error: Option<String>,
    pub logs: Vec<String>,
    pub progress: u8,
}

impl InitState {
    pub fn new() -> Self {
        Self {
            phase: RwLock::new(InitPhase::Starting),
            message: RwLock::new("Starting up...".to_string()),
            ready: AtomicBool::new(false),
            error: RwLock::new(None),
            container_name: RwLock::new(None),
            logs: RwLock::new(Vec::new()),
            progress: AtomicU8::new(0),
        }
    }

    pub async fn set_phase(&self, phase: InitPhase) {
        let label = phase.label().to_string();
        *self.phase.write().await = phase;
        *self.message.write().await = label;
    }

    pub async fn add_log(&self, msg: impl Into<String>) {
        self.logs.write().await.push(msg.into());
    }

    pub fn set_progress(&self, pct: u8) {
        self.progress.store(pct.min(100), Ordering::SeqCst);
    }

    /// Mark the system as not ready (e.g. before a container restart).
    /// Resets progress atomically. Call `clear_for_rebuild` afterward to reset logs/error.
    pub fn set_not_ready_sync(&self) {
        self.ready.store(false, Ordering::SeqCst);
        self.progress.store(0, Ordering::SeqCst);
    }

    /// Clear logs and error state for a fresh rebuild cycle.
    pub async fn clear_for_rebuild(&self) {
        self.logs.write().await.clear();
        *self.error.write().await = None;
    }

    pub async fn set_ready(&self, container_name: String) {
        *self.container_name.write().await = Some(container_name);
        *self.phase.write().await = InitPhase::Ready;
        *self.message.write().await = "Ready".to_string();
        self.progress.store(100, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
    }

    pub async fn set_failed(&self, error: String) {
        *self.phase.write().await = InitPhase::Failed;
        *self.message.write().await = "Initialization failed".to_string();
        *self.error.write().await = Some(error);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub async fn get_container_name(&self) -> Option<String> {
        self.container_name.read().await.clone()
    }

    pub async fn status(&self) -> InitStatusResponse {
        InitStatusResponse {
            phase: *self.phase.read().await,
            message: self.message.read().await.clone(),
            ready: self.is_ready(),
            error: self.error.read().await.clone(),
            logs: self.logs.read().await.clone(),
            progress: self.progress.load(Ordering::SeqCst),
        }
    }
}
