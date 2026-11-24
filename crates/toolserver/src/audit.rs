//! Audit log (ADM-2): one event per tool call and per approval decision.
//!
//! Events carry metadata only: who, which skill and run, which tool, what was read (paths
//! and ids, and for Jira, Confluence, docs and context items a short title), a hash of the
//! arguments, and a reference to what was written. They never carry message bodies, page
//! bodies or snippets, and nothing from Slack but channel and message ids: no Slack title or
//! text is ever recorded, because Slack text may be kept only 24 hours (SLK-8, D6).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::{StoreError, StoreResult};

/// The longest source title recorded, in characters.
pub const MAX_SOURCE_TITLE: usize = 120;

/// The systems whose read items have their title recorded: an issue summary, a page title, a
/// docs page title, a context file's title. Never Slack: its titles are built from message
/// text, which may be kept only 24 hours.
pub const TITLED_SYSTEMS: [&str; 4] = ["jira", "confluence", "docs", "context"];

/// `raw` as a source title fit for the audit log: control characters (line breaks included)
/// become spaces, runs of white space one space, at most [`MAX_SOURCE_TITLE`] characters.
/// `None` when nothing is left.
pub fn source_title(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title: String = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_SOURCE_TITLE)
        .collect();
    let title = title.trim_end().to_string();
    (!title.is_empty()).then_some(title)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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

impl Outcome {
    pub const ALL: [Outcome; 10] = [
        Outcome::Ok,
        Outcome::Denied,
        Outcome::Pending,
        Outcome::Approved,
        Outcome::Rejected,
        Outcome::Granted,
        Outcome::Started,
        Outcome::Error,
        Outcome::Failed,
        Outcome::Expired,
    ];

    /// The serialized name, as the database and the JSON-lines log store it.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Denied => "denied",
            Outcome::Pending => "pending",
            Outcome::Approved => "approved",
            Outcome::Rejected => "rejected",
            Outcome::Granted => "granted",
            Outcome::Started => "started",
            Outcome::Error => "error",
            Outcome::Failed => "failed",
            Outcome::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }
}

/// Who made the call: a sandbox over MCP, or an internal service with its own token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    /// An agent sandbox, over `/mcp` with a session token.
    #[default]
    Mcp,
    /// The orchestrator or a platform-owner tool, with `PMOS_SERVICE_TOKEN`.
    Orchestrator,
    /// The Slack bot, with `PMOS_BOT_TOKEN`.
    Bot,
    /// `pmos-web`, with `PMOS_WEB_TOKEN`: the approvals routes and the read-only admin routes.
    Web,
    /// The web console, with `PMOS_CONSOLE_TS_TOKEN`: reads approvals only.
    Console,
    /// The tool server itself, with no caller: the audit retention purge records its runs
    /// as this (`docs/audit-retention.md`). Never a token's client.
    System,
    /// A host command, `pmos-admin users …` (`docs/user-database-spec.md` §5): the event's
    /// detail names the OS user who ran it. Never a token's client.
    Host,
    /// The `pmos-sandbox-audit` collector, with `PMOS_SANDBOX_AUDIT_TOKEN`: OpenShell's own
    /// sandbox decisions (`sandbox.*`, `crate::openshell::sandbox`), and the tool server's own
    /// gap and down events about the collector. Its token appends those and nothing else.
    SandboxAudit,
}

impl Client {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Orchestrator => "orchestrator",
            Self::Bot => "bot",
            Self::Web => "web",
            Self::Console => "console",
            Self::System => "system",
            Self::Host => "host",
            Self::SandboxAudit => "sandbox_audit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Mcp,
            Self::Orchestrator,
            Self::Bot,
            Self::Web,
            Self::Console,
            Self::System,
            Self::Host,
            Self::SandboxAudit,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
    }
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
    /// The persona (marketplace bot, `docs/bot-marketplace-spec.md` §7) the session ran as, or
    /// the drafting persona of the item a decision was about. `None` is Nudge, and every event
    /// that isn't about a bot's call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Registry name, such as `jira.create_issue`.
    pub tool: String,
    pub outcome: Outcome,
    /// Context paths and system ids read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// A short title for some of `sources`, keyed by the entry (`jira:PAY-212` → its
    /// summary): only for [`TITLED_SYSTEMS`], cleaned by [`source_title`]. Never for Slack.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub source_titles: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_hash: Option<String>,
    /// What was written, such as a Jira key or page URL, or an approval id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A tool call that returned what it read, as the audit log has it: when, which tool, the
/// `sources` it recorded (context paths, `system:id`) and their titles. Metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadEvent {
    pub at: DateTime<Utc>,
    pub tool: String,
    pub sources: Vec<String>,
    /// The titles the event recorded, by `sources` entry; empty for events recorded before
    /// titles were.
    pub titles: BTreeMap<String, String>,
}

impl ReadEvent {
    fn of(event: &AuditEvent) -> Self {
        Self {
            at: event.at,
            tool: event.tool.clone(),
            sources: event.sources.clone(),
            titles: event.source_titles.clone(),
        }
    }
}

/// The largest page `GET /internal/admin/audit` serves.
pub const MAX_PAGE: usize = 1000;

/// The most events one [`AuditSink::search`] returns: a full page plus one, which tells the
/// caller whether more follow.
pub const MAX_SEARCH: usize = MAX_PAGE + 1;

/// Where a page of [`AuditSink::search`] ends: the last event's time and store id. Events are
/// ordered newest first by `(at, id)`, so the next page is everything strictly before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub at: DateTime<Utc>,
    pub id: i64,
}

impl Cursor {
    /// An opaque token: `c1.` and base64url of `<unix nanos>:<id>`. Callers must not parse it.
    pub fn encode(&self) -> String {
        use base64::Engine;
        let nanos = self.at.timestamp_nanos_opt().unwrap_or_default();
        format!(
            "c1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{nanos}:{}", self.id))
        )
    }

    /// The cursor `token` encodes, or `None` if it isn't one.
    pub fn decode(token: &str) -> Option<Self> {
        use base64::Engine;
        let raw = token.strip_prefix("c1.")?;
        if raw.len() > 64 {
            return None;
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        let (nanos, id) = text.split_once(':')?;
        Some(Self {
            at: DateTime::from_timestamp_nanos(nanos.parse().ok()?),
            id: id.parse().ok()?,
        })
    }
}

/// A search of the whole audit log (`GET /internal/admin/audit`), newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    /// Only events strictly before this position.
    pub before: Option<Cursor>,
    /// At most this many, from 1 to [`MAX_SEARCH`].
    pub limit: usize,
    /// Only this `user`.
    pub actor: Option<String>,
    /// Only this `tool` (the registry name, such as `jira.create_issue`).
    pub action: Option<String>,
    pub outcome: Option<Outcome>,
    /// At or after this time.
    pub from: Option<DateTime<Utc>>,
    /// Strictly before this time.
    pub to: Option<DateTime<Utc>>,
    /// Only events of this marketplace bot (`persona`, a key; slice A1). Nudge's events carry
    /// no persona, so they are never matched by a key.
    pub persona: Option<String>,
}

impl AuditQuery {
    /// Does `event`, stored as `id`, match? The in-memory sink filters with this; Postgres
    /// filters in SQL to the same rules.
    pub fn matches(&self, id: i64, event: &AuditEvent) -> bool {
        self.before.is_none_or(|c| (event.at, id) < (c.at, c.id))
            && self.actor.as_ref().is_none_or(|a| *a == event.user)
            && self.action.as_ref().is_none_or(|a| *a == event.tool)
            && self.outcome.is_none_or(|o| o == event.outcome)
            && self.from.is_none_or(|f| event.at >= f)
            && self.to.is_none_or(|t| event.at < t)
            && self
                .persona
                .as_ref()
                .is_none_or(|p| event.persona.as_deref() == Some(p.as_str()))
    }

    /// The limit, clamped to 1 to [`MAX_SEARCH`].
    pub fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, MAX_SEARCH)
    }
}

/// An event as stored, with its store id (the order of recording).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: i64,
    pub event: AuditEvent,
}

impl StoredEvent {
    pub fn cursor(&self) -> Cursor {
        Cursor {
            at: self.event.at,
            id: self.id,
        }
    }
}

/// Does `event` belong in [`AuditSink::reads`]?
pub fn is_read_of(event: &AuditEvent, user: &str, run_id: &str, until: DateTime<Utc>) -> bool {
    event.user == user
        && event.run_id == run_id
        && event.outcome == Outcome::Ok
        && !event.sources.is_empty()
        && event.at <= until
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Store one event. An error means the event was not recorded; callers log it loudly.
    async fn record(&self, event: AuditEvent) -> StoreResult<()>;

    /// `user`'s calls in run `run_id` at or before `until` that succeeded and recorded
    /// sources, newest first, at most `limit`. `None` when this sink can't be read back
    /// (a JSON-lines file is write-only).
    async fn reads(
        &self,
        _user: &str,
        _run_id: &str,
        _until: DateTime<Utc>,
        _limit: usize,
    ) -> StoreResult<Option<Vec<ReadEvent>>> {
        Ok(None)
    }

    /// Events matching `query`, newest first by `(at, id)`, at most `query.limit` (clamped
    /// to 1 to [`MAX_SEARCH`]). `None` when this sink can't be read back.
    async fn search(&self, _query: &AuditQuery) -> StoreResult<Option<Vec<StoredEvent>>> {
        Ok(None)
    }

    /// How many events at or after `since` each outcome has (outcomes with none are left
    /// out). `None` when this sink can't be read back.
    async fn count_outcomes(
        &self,
        _since: DateTime<Utc>,
    ) -> StoreResult<Option<BTreeMap<Outcome, u64>>> {
        Ok(None)
    }

    /// One purge batch (`docs/audit-retention.md`): delete at most `batch` events (1 to
    /// [`MAX_PURGE_BATCH`]) recorded before the cutoff (`now` less `retention_days`, and
    /// never later than `now` less [`MIN_RETENTION_DAYS`]), oldest first, and record the batch
    /// as a [`PURGE_TOOL`] event. A retention under [`MIN_RETENTION_DAYS`] is an error and
    /// deletes nothing. `None` when this sink can't delete (a JSON-lines file);
    /// [`Purged::Busy`] when another purge is running. Postgres keeps time by its own clock,
    /// not `now`.
    async fn purge(
        &self,
        _retention_days: u32,
        _batch: u32,
        _now: DateTime<Utc>,
    ) -> StoreResult<Option<Purged>> {
        Ok(None)
    }

    /// The oldest event's time and the last purge, for the admin overview. `None` when this
    /// sink can't be read back.
    async fn retention(&self) -> StoreResult<Option<RetentionFacts>> {
        Ok(None)
    }

    /// A sandbox decision the OpenShell store already recorded (`crate::openshell`), copied to
    /// this sink with its sandbox columns as `fields` (JSON): the JSON-lines file gets it as it
    /// gets every event (spec §B.7), the in-memory log keeps the event. Postgres does nothing:
    /// the row is already in `audit_events`.
    async fn mirror_sandbox(
        &self,
        _event: AuditEvent,
        _fields: serde_json::Value,
    ) -> StoreResult<()> {
        Ok(())
    }
}

// Retention.

/// The shortest audit retention there is, in days: the owner's decision (purge at 90 days,
/// no archive). The database has the same floor written into its purge function and its
/// append-only trigger (migration 0013), so no setting can lower it.
pub const MIN_RETENTION_DAYS: u32 = 90;

/// The longest retention `PMOS_AUDIT_RETENTION_DAYS` takes: ten years.
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// The most events one purge batch deletes (the database function's own limit).
pub const MAX_PURGE_BATCH: u32 = 100_000;

/// The action a purge batch is recorded as.
pub const PURGE_TOOL: &str = "audit.purge";

/// Who a purge batch is recorded as (client [`Client::System`]).
pub const PURGE_ACTOR: &str = "pmos-retention";

/// The run id purge batches are recorded under.
pub const PURGE_RUN: &str = "retention";

/// What one purge batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeBatch {
    /// Events deleted.
    pub deleted: u64,
    /// Only events recorded before this were eligible.
    pub cutoff: DateTime<Utc>,
    /// Eligible events are left: the batch stopped at its limit.
    pub more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purged {
    Ran(PurgeBatch),
    /// Another purge holds the lock: nothing was deleted or recorded.
    Busy,
}

/// The newest purge batch on record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastPurge {
    pub at: DateTime<Utc>,
    pub deleted: u64,
}

/// What the admin overview says of retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionFacts {
    pub oldest_event_at: Option<DateTime<Utc>>,
    pub last_purge: Option<LastPurge>,
}

/// A purge batch's `detail`: the count and the cutoff (to the second), no content. The
/// database function writes the same words.
pub fn purge_detail(deleted: u64, cutoff: DateTime<Utc>) -> String {
    format!(
        "purged {deleted} events older than {}",
        cutoff.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// The count and cutoff of a [`purge_detail`], or `None` if `detail` isn't one.
pub fn parse_purge_detail(detail: &str) -> Option<(u64, DateTime<Utc>)> {
    let rest = detail.strip_prefix("purged ")?;
    let (n, at) = rest.split_once(" events older than ")?;
    if n.is_empty() || n.len() > 12 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if at.len() != 20 || !at.ends_with('Z') {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc);
    Some((n.parse().ok()?, at))
}

/// The event that records a purge batch.
pub fn purge_event(at: DateTime<Utc>, batch: &PurgeBatch) -> AuditEvent {
    AuditEvent {
        at,
        client: Client::System,
        run_id: PURGE_RUN.to_string(),
        user: PURGE_ACTOR.to_string(),
        skill: None,
        bot: None,
        tool: PURGE_TOOL.to_string(),
        outcome: Outcome::Ok,
        sources: Vec::new(),
        source_titles: BTreeMap::new(),
        args_hash: None,
        output_ref: None,
        detail: Some(purge_detail(batch.deleted, batch.cutoff)),
        persona: None,
    }
}

/// The checks every sink makes before purging: the floor and the batch limit.
pub fn check_purge(retention_days: u32, batch: u32) -> StoreResult<()> {
    if retention_days < MIN_RETENTION_DAYS {
        return Err(StoreError(format!(
            "audit retention must be at least {MIN_RETENTION_DAYS} days"
        )));
    }
    if !(1..=MAX_PURGE_BATCH).contains(&batch) {
        return Err(StoreError(format!(
            "a purge batch must be from 1 to {MAX_PURGE_BATCH}"
        )));
    }
    Ok(())
}

/// Keeps events in memory. Used in tests and when no log file is configured. Append-only
/// like the Postgres table: the one deletion is [`AuditSink::purge`], under the same rules.
#[derive(Default)]
pub struct MemoryAudit {
    /// Each event with its store id, in the order recorded, and the last id given.
    events: Mutex<(Vec<(i64, AuditEvent)>, i64)>,
}

impl MemoryAudit {
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .expect("audit lock")
            .0
            .iter()
            .map(|(_, e)| e.clone())
            .collect()
    }
}

#[async_trait]
impl AuditSink for MemoryAudit {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        let mut guard = self.events.lock().expect("audit lock");
        guard.1 += 1;
        let id = guard.1;
        guard.0.push((id, event));
        Ok(())
    }

    async fn reads(
        &self,
        user: &str,
        run_id: &str,
        until: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Option<Vec<ReadEvent>>> {
        let events = self.events.lock().expect("audit lock");
        let mut found: Vec<ReadEvent> = events
            .0
            .iter()
            .map(|(_, e)| e)
            .filter(|e| is_read_of(e, user, run_id, until))
            .map(ReadEvent::of)
            .collect();
        // Stable: of two events at the same time, the later recorded comes first.
        found.reverse();
        found.sort_by_key(|e| std::cmp::Reverse(e.at));
        found.truncate(limit);
        Ok(Some(found))
    }

    async fn search(&self, query: &AuditQuery) -> StoreResult<Option<Vec<StoredEvent>>> {
        let events = self.events.lock().expect("audit lock");
        let mut found: Vec<StoredEvent> = events
            .0
            .iter()
            .filter(|(id, e)| query.matches(*id, e))
            .map(|(id, e)| StoredEvent {
                id: *id,
                event: e.clone(),
            })
            .collect();
        found.sort_by_key(|s| std::cmp::Reverse((s.event.at, s.id)));
        found.truncate(query.bounded_limit());
        Ok(Some(found))
    }

    async fn count_outcomes(
        &self,
        since: DateTime<Utc>,
    ) -> StoreResult<Option<BTreeMap<Outcome, u64>>> {
        let mut counts = BTreeMap::new();
        for (_, e) in self.events.lock().expect("audit lock").0.iter() {
            if e.at >= since {
                *counts.entry(e.outcome).or_insert(0) += 1;
            }
        }
        Ok(Some(counts))
    }

    async fn purge(
        &self,
        retention_days: u32,
        batch: u32,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Purged>> {
        check_purge(retention_days, batch)?;
        let floor = now - chrono::Duration::days(i64::from(MIN_RETENTION_DAYS));
        let cutoff = (now - chrono::Duration::days(i64::from(retention_days))).min(floor);
        let mut guard = self.events.lock().expect("audit lock");
        let mut doomed: Vec<(DateTime<Utc>, i64)> = guard
            .0
            .iter()
            .filter(|(_, e)| e.at < cutoff)
            .map(|(id, e)| (e.at, *id))
            .collect();
        doomed.sort();
        let more = doomed.len() > batch as usize;
        doomed.truncate(batch as usize);
        let ids: std::collections::HashSet<i64> = doomed.iter().map(|(_, id)| *id).collect();
        // The re-check the database trigger makes, whatever the cutoff: never a newer event.
        guard
            .0
            .retain(|(id, e)| !(ids.contains(id) && e.at < floor));
        let done = PurgeBatch {
            deleted: ids.len() as u64,
            cutoff,
            more,
        };
        guard.1 += 1;
        let id = guard.1;
        guard.0.push((id, purge_event(now, &done)));
        Ok(Some(Purged::Ran(done)))
    }

    async fn retention(&self) -> StoreResult<Option<RetentionFacts>> {
        let guard = self.events.lock().expect("audit lock");
        let oldest_event_at = guard.0.iter().map(|(_, e)| e.at).min();
        let last_purge = guard
            .0
            .iter()
            .filter(|(_, e)| e.tool == PURGE_TOOL && e.client == Client::System)
            .max_by_key(|(id, e)| (e.at, *id))
            .and_then(|(_, e)| {
                let (deleted, _) = parse_purge_detail(e.detail.as_deref()?)?;
                Some(LastPurge { at: e.at, deleted })
            });
        Ok(Some(RetentionFacts {
            oldest_event_at,
            last_purge,
        }))
    }

    async fn mirror_sandbox(
        &self,
        event: AuditEvent,
        _fields: serde_json::Value,
    ) -> StoreResult<()> {
        self.record(event).await
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

    /// From the first sink that can be read back.
    async fn reads(
        &self,
        user: &str,
        run_id: &str,
        until: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Option<Vec<ReadEvent>>> {
        for sink in &self.0 {
            if let Some(found) = sink.reads(user, run_id, until, limit).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// From the first sink that can be read back.
    async fn search(&self, query: &AuditQuery) -> StoreResult<Option<Vec<StoredEvent>>> {
        for sink in &self.0 {
            if let Some(found) = sink.search(query).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// From the first sink that can be read back.
    async fn count_outcomes(
        &self,
        since: DateTime<Utc>,
    ) -> StoreResult<Option<BTreeMap<Outcome, u64>>> {
        for sink in &self.0 {
            if let Some(found) = sink.count_outcomes(since).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// In the first sink that can purge, which records the batch itself; the batch is then
    /// recorded in every other sink too. A JSON-lines file shipped off-box is never purged
    /// by PM OS, but it says when the database was.
    async fn purge(
        &self,
        retention_days: u32,
        batch: u32,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Purged>> {
        for (i, sink) in self.0.iter().enumerate() {
            let Some(purged) = sink.purge(retention_days, batch, now).await? else {
                continue;
            };
            if let Purged::Ran(done) = &purged {
                for (j, other) in self.0.iter().enumerate() {
                    if j == i {
                        continue;
                    }
                    if let Err(e) = other.record(purge_event(now, done)).await {
                        tracing::error!(error = %e, "a purge batch couldn't be recorded in every audit sink");
                    }
                }
            }
            return Ok(Some(purged));
        }
        Ok(None)
    }

    /// From the first sink that can be read back.
    async fn retention(&self) -> StoreResult<Option<RetentionFacts>> {
        for sink in &self.0 {
            if let Some(found) = sink.retention().await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// In every sink, after trying them all.
    async fn mirror_sandbox(
        &self,
        event: AuditEvent,
        fields: serde_json::Value,
    ) -> StoreResult<()> {
        let mut first_error = None;
        for sink in &self.0 {
            if let Err(e) = sink.mirror_sandbox(event.clone(), fields.clone()).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
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

    /// The event with its sandbox columns under `sandbox`.
    async fn mirror_sandbox(
        &self,
        event: AuditEvent,
        fields: serde_json::Value,
    ) -> StoreResult<()> {
        let mut value = serde_json::to_value(&event).map_err(|e| StoreError(e.to_string()))?;
        if let serde_json::Value::Object(map) = &mut value {
            map.insert("sandbox".into(), fields);
        }
        let line = serde_json::to_string(&value).map_err(|e| StoreError(e.to_string()))?;
        let mut file = self.file.lock().expect("audit lock");
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|e| StoreError(e.to_string()))
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
            source_titles: BTreeMap::new(),
            args_hash: None,
            output_ref: None,
            detail: None,
            persona: None,
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
        assert!(parsed.get("source_titles").is_none(), "left out when empty");
    }

    #[tokio::test]
    async fn jsonl_keeps_source_titles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let sink = JsonlAudit::open(&path).unwrap();
        let mut e = event();
        e.source_titles.insert(
            "context/products/cloud.md".into(),
            "Acme Cloud overview".into(),
        );
        sink.record(e).await.unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(
            parsed["source_titles"],
            serde_json::json!({"context/products/cloud.md": "Acme Cloud overview"})
        );
    }

    #[test]
    fn source_titles_are_short_single_lines() {
        assert_eq!(
            source_title("  PRD:\tGrace\n\nperiod \u{7}  ").as_deref(),
            Some("PRD: Grace period")
        );
        assert_eq!(source_title("a\nb\tc\u{0}d").as_deref(), Some("a b c d"));
        assert_eq!(source_title(" \n\t "), None);
        let long = "x".repeat(500);
        assert_eq!(
            source_title(&long).unwrap().chars().count(),
            MAX_SOURCE_TITLE
        );
        assert_eq!(source_title("Café ✓").as_deref(), Some("Café ✓"));
    }
}
