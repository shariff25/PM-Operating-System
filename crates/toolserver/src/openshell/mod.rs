//! What the tool server keeps about NVIDIA OpenShell (`docs/openshell-prover-and-sandbox-audit-
//! spec.md`): the host's policy proofs (Part A, [`proofs`]) and the sandboxes' own allow and deny
//! decisions (Part B, [`sandbox`]). The tool server runs no prover and reads nothing from
//! OpenShell itself: proofs arrive from a host command, decisions from the `pmos-sandbox-audit`
//! collector through its own append-only token ([`ingest`]).
//!
//! One store seam, [`OpenShellStore`], with an in-memory implementation (tests and
//! `PMOS_DEV=1`) and a Postgres one (`pg.rs`), where proofs are `policy_proofs` rows and
//! decisions are `audit_events` rows, under the one 90-day purge.

pub mod pg;
pub mod proofs;
pub mod sandbox;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::audit::AuditSink;
use crate::service::ToolService;
use crate::store::{StoreError, StoreResult};
use proofs::ProofRow;
use sandbox::{
    Batch, CollectorState, Fields, Heartbeat, Ingested, KnownBot, Prepared, SandboxQuery,
    SandboxRow, WatchdogReport,
};

#[async_trait]
pub trait OpenShellStore: Send + Sync {
    /// Record `rows` and one `policy.prove` audit event each, all or nothing.
    async fn record_proofs(&self, rows: &[ProofRow]) -> StoreResult<()>;

    /// The newest proof of each bot, by bot.
    async fn latest_proofs(&self) -> StoreResult<Vec<ProofRow>>;

    /// Store one checked, attributed batch of run `run` (`events` in `seq` order): the
    /// sequence check, deduplication and gap events, in one transaction (`sandbox::plan`).
    async fn ingest(
        &self,
        run: &str,
        events: Vec<Prepared>,
        now: DateTime<Utc>,
    ) -> StoreResult<Ingested>;

    /// A heartbeat: the run is alive. A new run, or one heard from after an open
    /// `collector_down`, records the down period it ends; an `acked_seq` past what was stored
    /// is a gap.
    async fn heartbeat(&self, hb: &Heartbeat, now: DateTime<Utc>) -> StoreResult<Ingested>;

    /// The silence check: no heartbeat or batch for [`sandbox::silence`] opens a
    /// `collector_down` with an open end, once.
    async fn watchdog(&self, now: DateTime<Utc>) -> StoreResult<WatchdogReport>;

    /// Sandbox rows matching `q`, newest first.
    async fn sandbox_rows(&self, q: &SandboxQuery) -> StoreResult<Vec<SandboxRow>>;

    /// The sandbox columns of the audit events `ids`, for the admin audit list. In memory: none
    /// (its rows have their own ids).
    async fn sandbox_fields(&self, ids: &[i64]) -> StoreResult<Vec<(i64, Fields)>>;

    /// The collector's state for the overview.
    async fn collector_state(&self, now: DateTime<Utc>) -> StoreResult<CollectorState>;
}

/// In memory, for tests and `PMOS_DEV=1`.
pub struct MemoryOpenShell {
    audit: Option<Arc<dyn AuditSink>>,
    proofs: Mutex<Vec<ProofRow>>,
    sandbox: Mutex<sandbox::MemoryState>,
}

impl Default for MemoryOpenShell {
    fn default() -> Self {
        Self::new(None)
    }
}

impl MemoryOpenShell {
    /// With `audit`, each recorded proof's `policy.prove` event goes there.
    pub fn new(audit: Option<Arc<dyn AuditSink>>) -> Self {
        Self {
            audit,
            proofs: Mutex::new(Vec::new()),
            sandbox: Mutex::new(sandbox::MemoryState::default()),
        }
    }
}

#[async_trait]
impl OpenShellStore for MemoryOpenShell {
    async fn record_proofs(&self, rows: &[ProofRow]) -> StoreResult<()> {
        if let Some(audit) = &self.audit {
            for r in rows {
                audit.record(proofs::audit_event(r)).await?;
            }
        }
        self.proofs
            .lock()
            .expect("proofs lock")
            .extend(rows.iter().cloned());
        Ok(())
    }

    async fn latest_proofs(&self) -> StoreResult<Vec<ProofRow>> {
        Ok(proofs::newest_per_bot(
            &self.proofs.lock().expect("proofs lock"),
        ))
    }

    async fn ingest(
        &self,
        run: &str,
        events: Vec<Prepared>,
        now: DateTime<Utc>,
    ) -> StoreResult<Ingested> {
        Ok(self
            .sandbox
            .lock()
            .expect("sandbox lock")
            .ingest(run, events, now))
    }

    async fn heartbeat(&self, hb: &Heartbeat, now: DateTime<Utc>) -> StoreResult<Ingested> {
        Ok(self
            .sandbox
            .lock()
            .expect("sandbox lock")
            .heartbeat(hb, now))
    }

    async fn watchdog(&self, now: DateTime<Utc>) -> StoreResult<WatchdogReport> {
        Ok(self.sandbox.lock().expect("sandbox lock").watchdog(now))
    }

    async fn sandbox_rows(&self, q: &SandboxQuery) -> StoreResult<Vec<SandboxRow>> {
        Ok(self.sandbox.lock().expect("sandbox lock").rows(q))
    }

    async fn sandbox_fields(&self, _ids: &[i64]) -> StoreResult<Vec<(i64, Fields)>> {
        Ok(Vec::new())
    }

    async fn collector_state(&self, now: DateTime<Utc>) -> StoreResult<CollectorState> {
        Ok(self.sandbox.lock().expect("sandbox lock").state(now))
    }
}

/// Why a batch was refused.
#[derive(Debug)]
pub enum IngestError {
    /// The batch isn't the shape the collector writes (`400`).
    Invalid(String),
    /// Storage is down (`503`): the collector keeps the batch in its spool and retries.
    Store(StoreError),
}

impl From<StoreError> for IngestError {
    fn from(e: StoreError) -> Self {
        IngestError::Store(e)
    }
}

/// Copy what was stored to every other audit sink (the JSON-lines file). A copy that fails is
/// logged loudly; the row is in the database already.
async fn mirror(svc: &ToolService, stored: &[(crate::audit::AuditEvent, Fields)]) {
    for (event, fields) in stored {
        if let Err(e) = svc
            .stores()
            .audit
            .mirror_sandbox(event.clone(), sandbox::fields_json(fields))
            .await
        {
            tracing::error!(error = %e, "a sandbox event couldn't be copied to every audit sink");
        }
    }
}

/// `POST /internal/sandbox-audit/batch`: check the batch, attribute each event to its bot and,
/// when it can tell, its PM and task (from the roster and the bot sessions open at the event's
/// time), store it, and copy what was stored to the other sinks.
pub async fn ingest(
    svc: &ToolService,
    batch: Batch,
    now: DateTime<Utc>,
) -> Result<Ingested, IngestError> {
    sandbox::check_batch(&batch, now).map_err(IngestError::Invalid)?;
    let roster = svc.roster();
    let mut prepared = Vec::with_capacity(batch.events.len());
    for e in &batch.events {
        let bot = e.sandbox.as_deref().and_then(sandbox::bot_of_sandbox);
        let known = bot.and_then(|b| roster.bot(b)).map(|(found, _)| {
            if found.scope == pmos_contract::bots::BotScope::Personal {
                KnownBot::Personal {
                    owner: found.owner.clone(),
                }
            } else {
                KnownBot::Team
            }
        });
        let sessions = match (bot, &known, e.at) {
            (Some(b), Some(_), Some(at)) => svc.stores().sessions.open_bot_sessions(b, at).await?,
            _ => Vec::new(),
        };
        let who = sandbox::attribute(e.sandbox.as_deref(), known.as_ref(), &sessions);
        prepared.push(sandbox::prepare(e, &batch.collector_run, Some(&who)));
    }
    let done = svc
        .openshell()
        .ingest(&batch.collector_run, prepared, now)
        .await?;
    mirror(svc, &done.stored).await;
    Ok(done)
}

/// `POST /internal/sandbox-audit/heartbeat`.
pub async fn heartbeat(
    svc: &ToolService,
    hb: Heartbeat,
    now: DateTime<Utc>,
) -> Result<Ingested, IngestError> {
    sandbox::check_heartbeat(&hb).map_err(IngestError::Invalid)?;
    let done = svc.openshell().heartbeat(&hb, now).await?;
    mirror(svc, &done.stored).await;
    Ok(done)
}

/// One silence check, with what it recorded copied to the other sinks.
pub async fn watchdog(svc: &ToolService, now: DateTime<Utc>) -> StoreResult<WatchdogReport> {
    let done = svc.openshell().watchdog(now).await?;
    mirror(svc, &done.stored).await;
    Ok(done)
}

/// The tripwire (spec §B.5, SBA-14): `bot`'s denials and findings at or after `since`, newest
/// first (at most 1,000). The phase 2 halt (IL2-16) reads this; any row halts.
pub async fn denials_since(
    svc: &ToolService,
    bot: &str,
    since: DateTime<Utc>,
) -> StoreResult<Vec<SandboxRow>> {
    svc.openshell()
        .sandbox_rows(&SandboxQuery {
            bot: Some(bot.to_string()),
            since: Some(since),
            tools: vec![
                "sandbox.deny".into(),
                "sandbox.deny_summary".into(),
                "sandbox.finding".into(),
            ],
            limit: 1000,
        })
        .await
}

/// The tripwire's other half: every gap, down period and unparsed record at or after `since`.
/// No record of a denial is not proof of none, so any row here halts too.
pub async fn sandbox_gaps_since(
    svc: &ToolService,
    since: DateTime<Utc>,
) -> StoreResult<Vec<SandboxRow>> {
    svc.openshell()
        .sandbox_rows(&SandboxQuery {
            bot: None,
            since: Some(since),
            tools: vec![
                "sandbox.collector_gap".into(),
                "sandbox.collector_down".into(),
                "sandbox.source_gap".into(),
                "sandbox.unparsed".into(),
            ],
            limit: 1000,
        })
        .await
}

/// The deny count a row stands for: one, or a summary's held count.
fn denies_in(row: &SandboxRow) -> u64 {
    if row.event.tool == "sandbox.deny_summary" {
        row.event
            .detail
            .as_deref()
            .and_then(|d| d.strip_prefix("held "))
            .and_then(|d| d.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    } else {
        1
    }
}

/// The admin overview's sandbox block (spec §B.5): whether the collector is configured, its
/// state (`receiving`, silent since, the last gap), and each bot's denials in the last 24 hours
/// (a summary counts its held denies). Counts only: no destination.
pub async fn overview(svc: &ToolService, now: DateTime<Utc>) -> StoreResult<serde_json::Value> {
    let configured = svc.sandbox_audit_token().is_some();
    let state = svc.openshell().collector_state(now).await?;
    let rows = svc
        .openshell()
        .sandbox_rows(&SandboxQuery {
            since: Some(now - chrono::Duration::hours(24)),
            tools: vec![
                "sandbox.deny".into(),
                "sandbox.deny_summary".into(),
                "sandbox.finding".into(),
            ],
            limit: 1000,
            ..SandboxQuery::default()
        })
        .await?;
    let mut by_bot: std::collections::BTreeMap<String, u64> = Default::default();
    for r in &rows {
        let key = r
            .event
            .bot
            .clone()
            .unwrap_or_else(|| "(unknown sandbox)".into());
        *by_bot.entry(key).or_insert(0) += denies_in(r);
    }
    Ok(serde_json::json!({
        "configured": configured,
        "collector": state,
        "denials_24h": by_bot
            .into_iter()
            .map(|(bot, count)| serde_json::json!({"bot": bot, "count": count}))
            .collect::<Vec<_>>(),
        // At most 1,000 rows are read: past that the counts are a floor.
        "denials_24h_capped": rows.len() >= 1000,
    }))
}

/// A sandbox row as the Admin's and Auditor's views show it: the event's metadata and the
/// sandbox columns, an Auditor's without host, address, path or process (spec §B.7).
pub fn row_json(r: &SandboxRow, admin: bool) -> serde_json::Value {
    let mut v = serde_json::json!({
        "id": format!("ev_{}", r.id),
        "at": r.event.at,
        "action": r.event.tool,
        "outcome": r.event.outcome.as_str(),
        "bot": r.event.bot,
        "actor": r.event.user,
        "task_id": r.event.run_id,
        "detail": r.event.detail.as_deref().filter(|d| sandbox::is_safe_detail(d)),
    });
    v["sandbox"] = if admin {
        sandbox::fields_json(&r.fields)
    } else {
        sandbox::auditor_fields_json(&r.fields)
    };
    v
}

/// Run the silence check every minute while the collector is configured.
pub fn spawn_watchdog(
    svc: Arc<ToolService>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {}
            }
            match watchdog(&svc, Utc::now()).await {
                Ok(r) if !r.stored.is_empty() => {
                    tracing::warn!("the sandbox-audit collector has been silent for 5 minutes: collector_down recorded")
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "the sandbox-audit silence check failed"),
            }
        }
    })
}
