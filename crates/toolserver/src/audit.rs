//! Audit log (ADM-2): one event per tool call and per approval decision.
//!
//! Events carry metadata only: who, which skill and run, which tool, what was read (paths
//! and ids), a hash of the arguments, and a reference to what was written. They never carry
//! message bodies, so nothing from Slack is retained here (SLK-8).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The call ran and returned.
    Ok,
    /// Refused: undeclared tool, above the sensitivity ceiling, or not the owner.
    Denied,
    /// A write was queued for approval.
    Pending,
    /// A person approved a write (possibly edited) and it ran.
    Approved,
    /// A person rejected a write.
    Rejected,
    /// A grant was recorded for a write the PM approved in the harness.
    Granted,
    /// The call failed in a connector or on bad input.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEvent {
    pub at: DateTime<Utc>,
    pub run_id: String,
    pub user: String,
    pub skill: String,
    /// Registry name, such as `jira.create_issue`.
    pub tool: String,
    pub outcome: Outcome,
    /// Context paths and system ids read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_hash: Option<String>,
    /// What was written, such as a Jira key or page URL, or an approval id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub trait AuditSink: Send + Sync {
    fn record(&self, event: AuditEvent);
}

/// Keeps events in memory. Used in tests and when no log file is configured.
#[derive(Default)]
pub struct MemoryAudit {
    events: Mutex<Vec<AuditEvent>>,
}

impl MemoryAudit {
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("audit lock").clone()
    }
}

impl AuditSink for MemoryAudit {
    fn record(&self, event: AuditEvent) {
        self.events.lock().expect("audit lock").push(event);
    }
}

/// Appends one JSON object per line to a file.
pub struct JsonlAudit {
    file: Mutex<File>,
}

impl JsonlAudit {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl AuditSink for JsonlAudit {
    fn record(&self, event: AuditEvent) {
        let line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "cannot serialize audit event");
                return;
            }
        };
        let mut file = self.file.lock().expect("audit lock");
        if let Err(e) = writeln!(file, "{line}").and_then(|_| file.flush()) {
            // An audit write that fails must be loud: the run log is a control.
            tracing::error!(error = %e, "cannot write audit event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> AuditEvent {
        AuditEvent {
            at: "2026-11-14T00:00:00Z".parse().unwrap(),
            run_id: "run-1".into(),
            user: "ana".into(),
            skill: "ask".into(),
            tool: "context.search".into(),
            outcome: Outcome::Ok,
            sources: vec!["context/products/ccm.md".into()],
            args_hash: None,
            output_ref: None,
            detail: None,
        }
    }

    #[test]
    fn jsonl_appends_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let sink = JsonlAudit::open(&path).unwrap();
        sink.record(event());
        sink.record(event());
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["outcome"], "ok");
        assert!(parsed.get("args_hash").is_none());
    }
}
