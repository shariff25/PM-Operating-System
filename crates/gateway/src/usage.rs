//! One usage event per request: who asked, which role and model, how long it queued and ran,
//! and the tokens used. Never prompts or answers. Upstreams are local by configuration, so
//! the "restricted data sent off the Spark" figure is zero by construction; each event still
//! names the upstream host so that can be checked.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::scheduler::Priority;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageEvent {
    pub at: DateTime<Utc>,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub endpoint: &'static str,
    pub role: String,
    pub model: String,
    pub upstream: String,
    pub priority: Priority,
    pub stream: bool,
    pub stub: bool,
    pub status: u16,
    pub queue_ms: u64,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workarounds: Vec<&'static str>,
}

/// Recent events in memory, plus an optional JSON-lines file for shipping off-box.
pub struct UsageLog {
    file: Option<Mutex<File>>,
    recent: Mutex<Vec<UsageEvent>>,
}

const KEEP: usize = 1000;

impl UsageLog {
    pub fn new(path: Option<&Path>) -> std::io::Result<Self> {
        let file = match path {
            Some(p) => Some(Mutex::new(
                OpenOptions::new().create(true).append(true).open(p)?,
            )),
            None => None,
        };
        Ok(Self {
            file,
            recent: Mutex::new(Vec::new()),
        })
    }

    pub fn record(&self, event: UsageEvent) {
        tracing::info!(
            client = %event.client,
            user = event.user.as_deref().unwrap_or("-"),
            role = %event.role,
            status = event.status,
            queue_ms = event.queue_ms,
            latency_ms = event.latency_ms,
            "model call"
        );
        if let Some(file) = &self.file {
            let line = serde_json::to_string(&event).expect("usage event serializes");
            let mut f = file.lock().expect("usage log lock");
            if let Err(e) = writeln!(f, "{line}") {
                tracing::error!(error = %e, "usage log write failed");
            }
        }
        let mut recent = self.recent.lock().expect("usage lock");
        if recent.len() >= KEEP {
            recent.remove(0);
        }
        recent.push(event);
    }

    pub fn recent(&self) -> Vec<UsageEvent> {
        self.recent.lock().expect("usage lock").clone()
    }
}
