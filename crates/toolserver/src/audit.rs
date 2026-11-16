//! Audit log (ADM-2): one event per tool call and per approval decision.
//!
//! Events carry metadata only: who, which skill and run, which tool, what was read (paths
//! and ids), a hash of the arguments, and a reference to what was written. They never carry
//! message bodies, so nothing from Slack is retained here (SLK-8).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audit_chain::{
    unsupported, AuditEnvelope, ChainRecord, MemoryChain, PruneReport, Recorded, VerifyReport,
};
use crate::store::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// An approved write is about to run. Recorded first: if this can't be recorded, the
    /// write doesn't run.
    Started,
    /// The call failed in a connector or on bad input.
    Error,
    /// An approved member could not be committed to the content repo.
    Failed,
    /// A Slack draft nobody decided in time expired, and its text was removed.
    Expired,
}

/// Who made the call: a sandbox over MCP, or an internal service with its own token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    /// An agent sandbox, over `/mcp` with a session token.
    #[default]
    Mcp,
    /// The orchestrator or a platform-owner tool, with `PMOS_SERVICE_TOKEN`.
    Orchestrator,
    /// The Slack bot, with `PMOS_BOT_TOKEN`.
    Bot,
}

impl Client {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Orchestrator => "orchestrator",
            Self::Bot => "bot",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mcp" => Some(Self::Mcp),
            "orchestrator" => Some(Self::Orchestrator),
            "bot" => Some(Self::Bot),
            _ => None,
        }
    }
}

impl Outcome {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

/// The serde form of an outcome, so the database and the JSON-lines log say the same thing.
pub fn outcome_str(outcome: Outcome) -> String {
    serde_json::to_value(outcome)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEvent {
    pub at: DateTime<Utc>,
    pub client: Client,
    pub run_id: String,
    pub user: String,
    /// The session's skill; `None` for a bot session.
    pub skill: Option<String>,
    /// The session's bot, for a bot session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
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

#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Store one event. An error means the event was not recorded; callers log it loudly.
    async fn record(&self, event: AuditEvent) -> StoreResult<()>;

    // The hash-chained log (`audit_chain`). Sinks that keep no chain, such as the JSON-lines
    // file, use these defaults; `chained` says which sinks do.

    /// Does this sink keep the hash chain?
    fn chained(&self) -> bool {
        false
    }

    /// Store one envelope in the chain and return its `seq` and hash. An error means it was
    /// not recorded; callers that must fail closed refuse the action.
    async fn record_envelope(&self, _envelope: AuditEnvelope) -> StoreResult<Recorded> {
        Err(unsupported())
    }

    /// Walk the chain and report the first break.
    async fn verify_chain(&self) -> StoreResult<VerifyReport> {
        Err(unsupported())
    }

    /// Delete the oldest records, at most `batch`, that are all older than `cutoff`, after
    /// writing a checkpoint. See `audit_chain::prune_retained`.
    async fn prune_before(
        &self,
        _cutoff: DateTime<Utc>,
        _batch: usize,
    ) -> StoreResult<PruneReport> {
        Err(unsupported())
    }

    /// Records with `seq` above `after_seq`, oldest first (keyset pagination).
    async fn read_chain(&self, _after_seq: i64, _limit: usize) -> StoreResult<Vec<ChainRecord>> {
        Err(unsupported())
    }
}

/// Keeps events in memory. Used in tests and when no log file is configured.
#[derive(Default)]
pub struct MemoryAudit {
    events: Mutex<Vec<AuditEvent>>,
    chain: MemoryChain,
}

impl MemoryAudit {
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("audit lock").clone()
    }

    /// The hash chain behind `events()` (and any envelopes recorded directly).
    pub fn chain(&self) -> &MemoryChain {
        &self.chain
    }
}

#[async_trait]
impl AuditSink for MemoryAudit {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        self.chain.append(AuditEnvelope::from(&event));
        self.events.lock().expect("audit lock").push(event);
        Ok(())
    }

    fn chained(&self) -> bool {
        true
    }

    async fn record_envelope(&self, envelope: AuditEnvelope) -> StoreResult<Recorded> {
        Ok(self.chain.append(envelope))
    }

    async fn verify_chain(&self) -> StoreResult<VerifyReport> {
        Ok(self.chain.verify())
    }

    async fn prune_before(&self, cutoff: DateTime<Utc>, batch: usize) -> StoreResult<PruneReport> {
        Ok(self.chain.prune(cutoff, batch, Utc::now()))
    }

    async fn read_chain(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<ChainRecord>> {
        Ok(self.chain.list(after_seq, limit))
    }
}

/// Records every event in several sinks, such as Postgres plus a JSON-lines file that is
/// shipped off-box. Fails if any sink fails, after trying them all.
pub struct TeeAudit(pub Vec<std::sync::Arc<dyn AuditSink>>);

#[async_trait]
impl AuditSink for TeeAudit {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        let mut first_error = None;
        for sink in &self.0 {
            if let Err(e) = sink.record(event.clone()).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn chained(&self) -> bool {
        self.0.iter().any(|s| s.chained())
    }

    /// Every sink gets the envelope; the answer is the chained sink's.
    async fn record_envelope(&self, envelope: AuditEnvelope) -> StoreResult<Recorded> {
        let mut recorded = None;
        let mut first_error = None;
        for sink in &self.0 {
            match sink.record_envelope(envelope.clone()).await {
                Ok(r) if sink.chained() => recorded = Some(r),
                Ok(_) => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match (first_error, recorded) {
            (Some(e), _) => Err(e),
            (None, Some(r)) => Ok(r),
            (None, None) => Err(unsupported()),
        }
    }

    async fn verify_chain(&self) -> StoreResult<VerifyReport> {
        match self.0.iter().find(|s| s.chained()) {
            Some(s) => s.verify_chain().await,
            None => Err(unsupported()),
        }
    }

    async fn prune_before(&self, cutoff: DateTime<Utc>, batch: usize) -> StoreResult<PruneReport> {
        match self.0.iter().find(|s| s.chained()) {
            Some(s) => s.prune_before(cutoff, batch).await,
            None => Err(unsupported()),
        }
    }

    async fn read_chain(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<ChainRecord>> {
        match self.0.iter().find(|s| s.chained()) {
            Some(s) => s.read_chain(after_seq, limit).await,
            None => Err(unsupported()),
        }
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

#[async_trait]
impl AuditSink for JsonlAudit {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        let line = serde_json::to_string(&event).map_err(|e| StoreError(e.to_string()))?;
        let mut file = self.file.lock().expect("audit lock");
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|e| StoreError(e.to_string()))
    }

    /// The envelope as one line. The file is a shipping spool: the chain lives in Postgres.
    async fn record_envelope(&self, envelope: AuditEnvelope) -> StoreResult<Recorded> {
        let line = serde_json::to_string(&envelope).map_err(|e| StoreError(e.to_string()))?;
        let mut file = self.file.lock().expect("audit lock");
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|e| StoreError(e.to_string()))?;
        Ok(Recorded {
            seq: 0,
            hash: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> AuditEvent {
        AuditEvent {
            client: Client::Mcp,
            at: "2026-11-14T00:00:00Z".parse().unwrap(),
            run_id: "run-1".into(),
            user: "ana".into(),
            skill: Some("ask".into()),
            bot: None,
            tool: "context.search".into(),
            outcome: Outcome::Ok,
            sources: vec!["context/products/cloud.md".into()],
            args_hash: None,
            output_ref: None,
            detail: None,
        }
    }

    struct Failing;

    #[async_trait]
    impl AuditSink for Failing {
        async fn record(&self, _event: AuditEvent) -> StoreResult<()> {
            Err(StoreError("disk full".into()))
        }
    }

    #[tokio::test]
    async fn tee_writes_every_sink_and_reports_failure() {
        let a = std::sync::Arc::new(MemoryAudit::default());
        let b = std::sync::Arc::new(MemoryAudit::default());
        let tee = TeeAudit(vec![a.clone(), std::sync::Arc::new(Failing), b.clone()]);
        let err = tee.record(event()).await.unwrap_err();
        assert_eq!(err, StoreError("disk full".into()));
        assert_eq!(a.events().len(), 1);
        assert_eq!(
            b.events().len(),
            1,
            "a failing sink doesn't stop the others"
        );
    }

    #[tokio::test]
    async fn jsonl_appends_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let sink = JsonlAudit::open(&path).unwrap();
        sink.record(event()).await.unwrap();
        sink.record(event()).await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["outcome"], "ok");
        assert!(parsed.get("args_hash").is_none());
    }
}
