//! OpenShell's sandbox decisions in the audit log (`docs/openshell-prover-and-sandbox-audit-
//! spec.md` Part B, slice B1).
//!
//! The `pmos-sandbox-audit` collector receives OpenShell's own OCSF records over OTLP on
//! loopback, keeps an allowlist of metadata, and posts batches here with its own token
//! (`PMOS_SANDBOX_AUDIT_TOKEN`), which reaches `POST /internal/sandbox-audit/batch` and
//! `/heartbeat` and nothing else: it can't read, edit or delete audit events. This module checks
//! every batch again ([`check_batch`]: the shape, sizes, printable ASCII, a fixed reason set),
//! attributes each event to a bot and, when it can tell, a PM and task ([`attribute`]), and plans
//! what to store ([`plan`]): the events in sequence order, a `sandbox.collector_gap` for every
//! hole in a run's sequence numbers, a `sandbox.collector_down` when a new run starts or a silent
//! run is heard from again. [`plan_watchdog`] opens a `collector_down` after five silent
//! minutes. Every decision is an `audit_events` row (client `sandbox_audit`), so the one 90-day
//! purge is the only thing that deletes them.
//!
//! **Verify on a real host (slice B6):** everything about what OpenShell sends (field names,
//! classes, the gap record, the sandbox name and id); here it is from the documentation only.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::audit::{AuditEvent, Client, Outcome};

/// Who sandbox events are recorded as when no PM can be told.
pub const ACTOR: &str = "pmos-sandbox-audit";

/// The run id of an event with no task.
pub const RUN: &str = "sandbox";

/// The most events in one batch (spec §B.2).
pub const MAX_EVENTS: usize = 500;

/// The largest batch body, in bytes (spec §B.2).
pub const MAX_BODY: usize = 256 * 1024;

/// No heartbeat or batch for this long opens a `sandbox.collector_down` (spec §B.6).
pub fn silence() -> Duration {
    Duration::minutes(5)
}

/// How far ahead of the tool server's clock an event's time may be.
pub fn clock_skew() -> Duration {
    Duration::minutes(5)
}

/// The fixed reasons (OpenShell's `status_detail`, mapped by the collector; spec §B.2).
pub const REASONS: [&str; 7] = [
    "no_matching_policy",
    "always_blocked",
    "not_in_allowed_ips",
    "l7_deny",
    "cross_sandbox",
    "finding",
    "other",
];

/// Every action a sandbox row can have (spec §B.3).
pub const TOOLS: [&str; 9] = [
    "sandbox.deny",
    "sandbox.deny_summary",
    "sandbox.allow_summary",
    "sandbox.finding",
    "sandbox.policy_loaded",
    "sandbox.source_gap",
    "sandbox.collector_gap",
    "sandbox.collector_down",
    "sandbox.unparsed",
];

/// The kinds of event the collector may send. The two the tool server detects itself
/// (`collector_gap`, `collector_down`) are never accepted from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Deny,
    DenySummary,
    AllowSummary,
    Finding,
    PolicyLoaded,
    SourceGap,
    Unparsed,
}

impl Kind {
    pub fn tool(self) -> &'static str {
        match self {
            Kind::Deny => "sandbox.deny",
            Kind::DenySummary => "sandbox.deny_summary",
            Kind::AllowSummary => "sandbox.allow_summary",
            Kind::Finding => "sandbox.finding",
            Kind::PolicyLoaded => "sandbox.policy_loaded",
            Kind::SourceGap => "sandbox.source_gap",
            Kind::Unparsed => "sandbox.unparsed",
        }
    }

    /// `denied` for a deny or finding, `ok` for an allow summary or policy load, `error` for a
    /// gap or unparsed records (spec §B.3).
    pub fn outcome(self) -> Outcome {
        match self {
            Kind::Deny | Kind::DenySummary | Kind::Finding => Outcome::Denied,
            Kind::AllowSummary | Kind::PolicyLoaded => Outcome::Ok,
            Kind::SourceGap | Kind::Unparsed => Outcome::Error,
        }
    }

    pub fn decision(self) -> Option<&'static str> {
        match self {
            Kind::Deny | Kind::DenySummary => Some("deny"),
            Kind::AllowSummary => Some("allow"),
            Kind::Finding => Some("finding"),
            _ => None,
        }
    }

    /// Events about one sandbox, attributed to its bot.
    fn about_a_sandbox(self) -> bool {
        !matches!(self, Kind::SourceGap | Kind::Unparsed)
    }
}

/// A note the collector adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Note {
    /// A `metadata.product.version` the format wasn't verified on (spec §B.8).
    FormatUnverified,
}

/// One event as the collector sends it. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WireEvent {
    pub seq: u64,
    pub kind: Option<Kind>,
    pub at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_event_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_correlation_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<String>,
    /// A summary's count, unparsed records, or a source gap's dropped records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// A source gap's start (OpenShell's `openshell.ocsf.gap.since_unix_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// A deny summary's distinct rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
}

/// `POST /internal/sandbox-audit/batch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub collector_run: String,
    pub events: Vec<WireEvent>,
}

/// `POST /internal/sandbox-audit/heartbeat`: the run is alive, and the last `seq` the tool
/// server acknowledged to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub collector_run: String,
    pub acked_seq: u64,
}

/// The sandbox columns of an `audit_events` row (migration 0031).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Fields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_event_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_correlation_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_seq: Option<i64>,
}

/// One row to store: the event, its sandbox columns, and the collector's `seq` (`None` for the
/// tool server's own gap and down events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub seq: Option<u64>,
    pub event: AuditEvent,
    pub fields: Fields,
}

/// One stored sandbox row, for reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRow {
    pub id: i64,
    pub event: AuditEvent,
    pub fields: Fields,
}

/// What one batch or heartbeat did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Every row stored, gap and down events included, to copy to the other audit sinks.
    pub stored: Vec<(AuditEvent, Fields)>,
    /// Denies and findings already stored (the same `os_event_uid`).
    pub duplicates: usize,
    /// Events with a `seq` already stored (a batch replayed from the collector's spool).
    pub replayed: usize,
    /// Holes in the sequence, and down periods, recorded.
    pub gaps: usize,
}

/// What the silence check did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchdogReport {
    pub stored: Vec<(AuditEvent, Fields)>,
}

/// A search of the sandbox rows, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxQuery {
    pub bot: Option<String>,
    /// At or after.
    pub since: Option<DateTime<Utc>>,
    /// Only these actions (empty: every sandbox action).
    pub tools: Vec<String>,
    pub limit: usize,
}

impl SandboxQuery {
    pub fn matches(&self, row: &SandboxRow) -> bool {
        self.bot
            .as_ref()
            .is_none_or(|b| row.event.bot.as_deref() == Some(b.as_str()))
            && self.since.is_none_or(|s| row.event.at >= s)
            && (self.tools.is_empty() || self.tools.contains(&row.event.tool))
            && row.event.tool.starts_with("sandbox.")
    }

    pub fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, 1000)
    }
}

/// The collector as the overview shows it (spec §B.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CollectorState {
    /// Heard from within the last five minutes.
    pub receiving: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    /// An open `collector_down`: silent since then.
    pub silent_since: Option<DateTime<Utc>>,
    /// The newest gap or down event (collector or OpenShell).
    pub last_gap_at: Option<DateTime<Utc>>,
    pub runs: usize,
}

/// A collector run, as stored (`sandbox_collector_runs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run: String,
    pub first_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_seq: u64,
    pub down_open_since: Option<DateTime<Utc>>,
    pub superseded: bool,
}

/// What to store for one batch or heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub inserts: Vec<Prepared>,
    /// The run as it is afterwards.
    pub run: RunRow,
    /// A previous run this one replaces.
    pub supersede: Option<String>,
    pub replayed: usize,
    pub gaps: usize,
}

// Checking a batch.

/// Printable ASCII, 1 to `max` bytes.
fn ok_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn ok_opt(v: &Option<String>, max: usize, what: &str) -> Result<(), String> {
    match v {
        Some(s) if !ok_text(s, max) => Err(format!(
            "`{what}` must be printable ASCII of at most {max} bytes"
        )),
        _ => Ok(()),
    }
}

/// A collector run id: 8 to 64 of `A-Za-z0-9_-`.
pub fn valid_run(s: &str) -> bool {
    (8..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Check one batch: sizes, `seq` strictly increasing from 1, no event from the future, every
/// field the shape the collector writes. The collector cleans everything already; this is the
/// tool server's own check of an untrusted client. `Err` refuses the whole batch (`400`).
pub fn check_batch(batch: &Batch, now: DateTime<Utc>) -> Result<(), String> {
    if !valid_run(&batch.collector_run) {
        return Err("`collector_run` must be 8 to 64 of A-Za-z0-9_-".into());
    }
    if batch.events.is_empty() || batch.events.len() > MAX_EVENTS {
        return Err(format!("a batch holds 1 to {MAX_EVENTS} events"));
    }
    let mut last = 0u64;
    for e in &batch.events {
        if e.seq == 0 || e.seq <= last {
            return Err("`seq` must start at 1 and increase within a batch".into());
        }
        last = e.seq;
        let Some(kind) = e.kind else {
            return Err("every event needs a `kind`".into());
        };
        let Some(at) = e.at else {
            return Err("every event needs an `at`".into());
        };
        if at > now + clock_skew() {
            return Err("an event's `at` is in the future".into());
        }
        ok_opt(&e.sandbox, 64, "sandbox")?;
        ok_opt(&e.sandbox_uid, 128, "sandbox_uid")?;
        ok_opt(&e.rule, 128, "rule")?;
        ok_opt(&e.dest_host, 253, "dest_host")?;
        ok_opt(&e.dest_ip, 45, "dest_ip")?;
        ok_opt(&e.process, 128, "process")?;
        ok_opt(&e.os_event_uid, 128, "os_event_uid")?;
        ok_opt(&e.os_correlation_uid, 128, "os_correlation_uid")?;
        ok_opt(&e.os_version, 64, "os_version")?;
        ok_opt(&e.policy_hash, 128, "policy_hash")?;
        if let Some(r) = &e.reason {
            if !REASONS.contains(&r.as_str()) {
                return Err("`reason` isn't one of the fixed reasons".into());
            }
        }
        if let Some(m) = &e.http_method {
            if m.is_empty() || m.len() > 16 || !m.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err("`http_method` must be 1 to 16 uppercase letters".into());
            }
        }
        if let Some(p) = &e.http_path {
            if !ok_text(p, 256) || !p.starts_with('/') || p.contains('?') || p.contains('#') {
                return Err("`http_path` must be a path only: no query or fragment".into());
            }
        }
        if e.dest_port.is_some_and(|p| p > 65535) {
            return Err("`dest_port` must be 0 to 65535".into());
        }
        if e.count.is_some_and(|c| c > 1_000_000_000_000) {
            return Err("`count` is too large".into());
        }
        if e.rules.len() > 20 || e.rules.iter().any(|r| !ok_text(r, 128)) {
            return Err("`rules`: at most 20, each printable ASCII of at most 128 bytes".into());
        }
        let summary = matches!(kind, Kind::DenySummary | Kind::AllowSummary);
        if summary && e.count.is_none() {
            return Err("a summary needs a `count`".into());
        }
        if kind == Kind::AllowSummary && e.http_path.is_some() {
            return Err("an allow summary carries no path".into());
        }
    }
    Ok(())
}

/// A heartbeat's own check.
pub fn check_heartbeat(hb: &Heartbeat) -> Result<(), String> {
    if !valid_run(&hb.collector_run) {
        return Err("`collector_run` must be 8 to 64 of A-Za-z0-9_-".into());
    }
    Ok(())
}

// Attribution (spec §B.3).

/// Who an event is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub bot: Option<String>,
    pub user: String,
    pub run_id: String,
    /// `attributed`, `ambiguous`, `no task` or `unknown sandbox`.
    pub note: &'static str,
}

/// A bot as the roster knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnownBot {
    Personal { owner: String },
    Team,
}

/// The bot a sandbox name belongs to (`pmos-<bot>`, a valid name), if it is one.
pub fn bot_of_sandbox(sandbox: &str) -> Option<&str> {
    sandbox
        .strip_prefix("pmos-")
        .filter(|b| pmos_contract::bots::valid_name(b))
}

/// Attribute an event of `sandbox` at the time it happened: `known` is the roster's view of the
/// bot (`None`: not a bot PM OS runs), `sessions` the bot sessions open at that time as
/// `(user, run_id)`. A personal bot's PM is its owner, always; a team bot's only when exactly
/// one session was open. Verify on a real host: the sandbox id `pmos-admin sandbox` recorded at
/// create is not compared yet (slice B6).
pub fn attribute(
    sandbox: Option<&str>,
    known: Option<&KnownBot>,
    sessions: &[(String, String)],
) -> Attribution {
    let bot = sandbox.and_then(bot_of_sandbox);
    let (Some(bot), Some(known)) = (bot, known) else {
        return Attribution {
            bot: None,
            user: ACTOR.into(),
            run_id: RUN.into(),
            note: "unknown sandbox",
        };
    };
    let one = (sessions.len() == 1).then(|| &sessions[0]);
    let note = match sessions.len() {
        0 => "no task",
        1 => "attributed",
        _ => "ambiguous",
    };
    let (user, run_id) = match (known, one) {
        (KnownBot::Personal { owner }, Some((_, run))) => (owner.clone(), run.clone()),
        (KnownBot::Personal { owner }, None) => (owner.clone(), RUN.to_string()),
        (KnownBot::Team, Some((user, run))) => (user.clone(), run.clone()),
        (KnownBot::Team, None) => (ACTOR.to_string(), RUN.to_string()),
    };
    Attribution {
        bot: Some(bot.to_string()),
        user,
        run_id,
        note,
    }
}

/// The fixed-format detail of a collector event (no workload text).
fn detail_of(e: &WireEvent, kind: Kind, note: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match kind {
        Kind::DenySummary => parts.push(format!(
            "held {} denies past 100 a minute",
            e.count.unwrap_or(0)
        )),
        Kind::SourceGap => parts.push(match e.since {
            Some(s) => format!("dropped {} since {}", e.count.unwrap_or(0), stamp(s)),
            None => format!("dropped {}", e.count.unwrap_or(0)),
        }),
        Kind::Unparsed => parts.push(match e.note {
            Some(Note::FormatUnverified) => "format unverified".into(),
            None => format!("unparsed {}", e.count.unwrap_or(0)),
        }),
        _ => {}
    }
    if let Some(n) = note {
        parts.push(n.to_string());
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A collector event as a row, attributed as `who` says.
pub fn prepare(e: &WireEvent, run: &str, who: Option<&Attribution>) -> Prepared {
    let kind = e.kind.unwrap_or(Kind::Unparsed);
    let about = kind.about_a_sandbox();
    let who = who.filter(|_| about);
    let rule = if kind == Kind::DenySummary && !e.rules.is_empty() {
        let mut joined = e.rules.join(",");
        joined.truncate(128);
        Some(joined)
    } else {
        e.rule.clone()
    };
    let allow_count = (kind == Kind::AllowSummary)
        .then(|| e.count.map(|c| c as i64))
        .flatten();
    let event = AuditEvent {
        at: e.at.unwrap_or_else(Utc::now),
        client: Client::SandboxAudit,
        run_id: who.map_or(RUN.into(), |w| w.run_id.clone()),
        user: who.map_or(ACTOR.into(), |w| w.user.clone()),
        skill: None,
        bot: who.and_then(|w| w.bot.clone()),
        persona: None,
        tool: kind.tool().into(),
        outcome: kind.outcome(),
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: None,
        detail: detail_of(e, kind, who.map(|w| w.note)),
    };
    let fields = Fields {
        sandbox: e.sandbox.clone(),
        sandbox_uid: e.sandbox_uid.clone(),
        decision: kind.decision().map(String::from),
        rule,
        reason: e.reason.clone(),
        dest_host: e.dest_host.clone(),
        dest_ip: e.dest_ip.clone(),
        dest_port: e.dest_port.map(|p| p as i32),
        http_method: e.http_method.clone(),
        http_path: if kind == Kind::AllowSummary {
            None
        } else {
            e.http_path.clone()
        },
        process: e.process.clone(),
        allow_count,
        os_event_uid: e.os_event_uid.clone(),
        os_correlation_uid: e.os_correlation_uid.clone(),
        os_version: e.os_version.clone(),
        policy_hash: e.policy_hash.clone(),
        collector_run: Some(run.to_string()),
        collector_seq: Some(e.seq as i64),
    };
    Prepared {
        seq: Some(e.seq),
        event,
        fields,
    }
}

/// One of the tool server's own events about the collector.
fn own_event(tool: &str, at: DateTime<Utc>, run: &str, detail: String) -> Prepared {
    Prepared {
        seq: None,
        event: AuditEvent {
            at,
            client: Client::SandboxAudit,
            run_id: RUN.into(),
            user: ACTOR.into(),
            skill: None,
            bot: None,
            persona: None,
            tool: tool.into(),
            outcome: Outcome::Error,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: Some(detail),
        },
        fields: Fields {
            collector_run: Some(run.into()),
            ..Fields::default()
        },
    }
}

fn gap_event(run: &str, from: u64, to: u64, now: DateTime<Utc>) -> Prepared {
    own_event(
        "sandbox.collector_gap",
        now,
        run,
        format!("seq {from} to {to} missing {}", to - from + 1),
    )
}

fn down_event(run: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Prepared {
    own_event(
        "sandbox.collector_down",
        to,
        run,
        format!("down from {} to {}", stamp(from), stamp(to)),
    )
}

/// What a batch (`events`, already checked and prepared, in `seq` order) or a heartbeat
/// (`acked`) does to run `run`: `current` is its stored row, `previous` the newest other run
/// that isn't superseded. Every hole in the sequence is a `collector_gap`; a new run records
/// the down period since the previous run was last heard from; a run heard from after an open
/// `collector_down` records it closed. Events at or below the stored `seq` are a replay and
/// skipped.
pub fn plan(
    run: &str,
    current: Option<RunRow>,
    previous: Option<&RunRow>,
    events: Vec<Prepared>,
    acked: Option<u64>,
    now: DateTime<Utc>,
) -> Plan {
    let mut inserts = Vec::new();
    let mut gaps = 0;
    let mut supersede = None;
    let mut row = match current {
        Some(r) => r,
        None => {
            if let Some(p) = previous {
                let from = p.down_open_since.unwrap_or(p.last_seen_at);
                inserts.push(down_event(&p.run, from, now));
                gaps += 1;
                supersede = Some(p.run.clone());
            }
            RunRow {
                run: run.to_string(),
                first_at: now,
                last_seen_at: now,
                last_seq: 0,
                down_open_since: None,
                superseded: false,
            }
        }
    };
    if let Some(since) = row.down_open_since.take() {
        inserts.push(down_event(run, since, now));
        gaps += 1;
    }
    let mut replayed = 0;
    for p in events {
        let Some(seq) = p.seq else {
            continue;
        };
        if seq <= row.last_seq {
            replayed += 1;
            continue;
        }
        if seq > row.last_seq + 1 {
            inserts.push(gap_event(run, row.last_seq + 1, seq - 1, now));
            gaps += 1;
        }
        row.last_seq = seq;
        inserts.push(p);
    }
    if let Some(acked) = acked {
        if acked > row.last_seq {
            inserts.push(gap_event(run, row.last_seq + 1, acked, now));
            gaps += 1;
            row.last_seq = acked;
        }
    }
    row.last_seen_at = now;
    Plan {
        inserts,
        run: row,
        supersede,
        replayed,
        gaps,
    }
}

/// The silence check on the newest run: silent for [`silence`] with no open `collector_down`
/// opens one (`silent since <t>`), once.
pub fn plan_watchdog(latest: Option<&RunRow>, now: DateTime<Utc>) -> Option<(Prepared, RunRow)> {
    let r = latest?;
    if r.superseded || r.down_open_since.is_some() || now - r.last_seen_at < silence() {
        return None;
    }
    let mut row = r.clone();
    row.down_open_since = Some(r.last_seen_at);
    let event = own_event(
        "sandbox.collector_down",
        now,
        &r.run,
        format!("silent since {}", stamp(r.last_seen_at)),
    );
    Some((event, row))
}

/// Is `detail` one of the fixed sandbox notes (served on the admin audit list)?
pub fn is_safe_detail(detail: &str) -> bool {
    let stamp_ok =
        |s: &str| s.len() == 20 && s.ends_with('Z') && DateTime::parse_from_rfc3339(s).is_ok();
    let num = |s: &str| !s.is_empty() && s.len() <= 13 && s.bytes().all(|b| b.is_ascii_digit());
    let part = |p: &str| -> bool {
        if [
            "attributed",
            "ambiguous",
            "no task",
            "unknown sandbox",
            "format unverified",
        ]
        .contains(&p)
        {
            return true;
        }
        if let Some(n) = p
            .strip_prefix("held ")
            .and_then(|r| r.strip_suffix(" denies past 100 a minute"))
        {
            return num(n);
        }
        if let Some(rest) = p.strip_prefix("dropped ") {
            return match rest.split_once(" since ") {
                Some((n, t)) => num(n) && stamp_ok(t),
                None => num(rest),
            };
        }
        if let Some(n) = p.strip_prefix("unparsed ") {
            return num(n);
        }
        if let Some(rest) = p.strip_prefix("seq ") {
            let Some((from, rest)) = rest.split_once(" to ") else {
                return false;
            };
            let Some((to, n)) = rest.split_once(" missing ") else {
                return false;
            };
            return num(from) && num(to) && num(n);
        }
        if let Some(rest) = p.strip_prefix("down from ") {
            return rest
                .split_once(" to ")
                .is_some_and(|(a, b)| stamp_ok(a) && stamp_ok(b));
        }
        if let Some(t) = p.strip_prefix("silent since ") {
            return stamp_ok(t);
        }
        false
    };
    !detail.is_empty() && detail.split("; ").all(part)
}

/// The destination class an Auditor sees instead of the host, address and path (spec §B.7):
/// `pmos service` (one of the three pinned services), `always blocked` (OpenShell's own
/// always-blocked addresses), `private`, `public`, or `unknown` (no address and not a PM OS
/// name).
pub fn dest_class(f: &Fields) -> &'static str {
    if f.reason.as_deref() == Some("always_blocked") {
        return "always blocked";
    }
    let pmos = pmos_policy::Service::ALL.iter().any(|s| {
        f.dest_host
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(s.host()))
            || f.dest_ip.as_deref() == Some(s.addr().to_string().as_str())
    });
    if pmos {
        return "pmos service";
    }
    match f.dest_ip.as_deref().map(str::parse::<std::net::IpAddr>) {
        Some(Ok(std::net::IpAddr::V4(v4))) => {
            let o = v4.octets();
            let cgnat = o[0] == 100 && (64..128).contains(&o[1]);
            if v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || cgnat
                || v4.is_unspecified()
            {
                "private"
            } else {
                "public"
            }
        }
        Some(Ok(std::net::IpAddr::V6(v6))) => {
            let seg = v6.segments()[0];
            if v6.is_loopback() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80 {
                "private"
            } else {
                "public"
            }
        }
        _ => "unknown",
    }
}

/// A row's sandbox fields as JSON, for the JSON-lines copy and the Admin's view.
pub fn fields_json(f: &Fields) -> Value {
    let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut v {
        map.insert("dest_class".into(), Value::from(dest_class(f)));
    }
    v
}

/// What an Auditor sees of a row's sandbox fields: action, bot, time, outcome (in the event),
/// rule, reason, port and the destination class; never the host, address, path or process.
pub fn auditor_fields_json(f: &Fields) -> Value {
    serde_json::json!({
        "decision": f.decision,
        "rule": f.rule,
        "reason": f.reason,
        "dest_port": f.dest_port,
        "dest_class": dest_class(f),
        "allow_count": f.allow_count,
        "sandbox": f.sandbox,
    })
}

// In memory.

/// The in-memory store's state: runs, rows and the dedup set.
#[derive(Default)]
pub struct MemoryState {
    runs: BTreeMap<String, RunRow>,
    rows: Vec<SandboxRow>,
    uids: HashSet<String>,
    next_id: i64,
}

fn dedup_key(p: &Prepared) -> Option<&str> {
    matches!(p.event.tool.as_str(), "sandbox.deny" | "sandbox.finding")
        .then_some(p.fields.os_event_uid.as_deref())
        .flatten()
}

impl MemoryState {
    fn previous(&self, run: &str) -> Option<RunRow> {
        self.runs
            .values()
            .filter(|r| r.run != run && !r.superseded)
            .max_by_key(|r| r.last_seen_at)
            .cloned()
    }

    fn apply(&mut self, plan: Plan) -> Ingested {
        let mut out = Ingested {
            replayed: plan.replayed,
            gaps: plan.gaps,
            ..Ingested::default()
        };
        if let Some(old) = &plan.supersede {
            if let Some(r) = self.runs.get_mut(old) {
                r.superseded = true;
                r.down_open_since = None;
            }
        }
        self.runs.insert(plan.run.run.clone(), plan.run);
        for p in plan.inserts {
            if let Some(uid) = dedup_key(&p) {
                if !self.uids.insert(uid.to_string()) {
                    out.duplicates += 1;
                    continue;
                }
            }
            self.next_id += 1;
            out.stored.push((p.event.clone(), p.fields.clone()));
            self.rows.push(SandboxRow {
                id: self.next_id,
                event: p.event,
                fields: p.fields,
            });
        }
        out
    }

    pub fn ingest(&mut self, run: &str, events: Vec<Prepared>, now: DateTime<Utc>) -> Ingested {
        let current = self.runs.get(run).cloned();
        let previous = current.is_none().then(|| self.previous(run)).flatten();
        let p = plan(run, current, previous.as_ref(), events, None, now);
        self.apply(p)
    }

    pub fn heartbeat(&mut self, hb: &Heartbeat, now: DateTime<Utc>) -> Ingested {
        let current = self.runs.get(&hb.collector_run).cloned();
        let previous = current
            .is_none()
            .then(|| self.previous(&hb.collector_run))
            .flatten();
        let p = plan(
            &hb.collector_run,
            current,
            previous.as_ref(),
            Vec::new(),
            Some(hb.acked_seq),
            now,
        );
        self.apply(p)
    }

    fn latest(&self) -> Option<&RunRow> {
        self.runs
            .values()
            .filter(|r| !r.superseded)
            .max_by_key(|r| r.last_seen_at)
    }

    pub fn watchdog(&mut self, now: DateTime<Utc>) -> WatchdogReport {
        let Some((event, row)) = plan_watchdog(self.latest(), now) else {
            return WatchdogReport::default();
        };
        self.runs.insert(row.run.clone(), row);
        self.next_id += 1;
        let stored = vec![(event.event.clone(), event.fields.clone())];
        self.rows.push(SandboxRow {
            id: self.next_id,
            event: event.event,
            fields: event.fields,
        });
        WatchdogReport { stored }
    }

    pub fn rows(&self, q: &SandboxQuery) -> Vec<SandboxRow> {
        let mut found: Vec<SandboxRow> =
            self.rows.iter().filter(|r| q.matches(r)).cloned().collect();
        found.sort_by_key(|r| std::cmp::Reverse((r.event.at, r.id)));
        found.truncate(q.bounded_limit());
        found
    }

    pub fn state(&self, now: DateTime<Utc>) -> CollectorState {
        let latest = self.latest();
        let last_gap_at = self
            .rows
            .iter()
            .filter(|r| {
                matches!(
                    r.event.tool.as_str(),
                    "sandbox.collector_gap" | "sandbox.collector_down" | "sandbox.source_gap"
                )
            })
            .map(|r| r.event.at)
            .max();
        state_of(latest, last_gap_at, self.runs.len(), now)
    }
}

/// The overview's view of the collector.
pub fn state_of(
    latest: Option<&RunRow>,
    last_gap_at: Option<DateTime<Utc>>,
    runs: usize,
    now: DateTime<Utc>,
) -> CollectorState {
    CollectorState {
        receiving: latest.is_some_and(|r| now - r.last_seen_at < silence()),
        last_seen_at: latest.map(|r| r.last_seen_at),
        silent_since: latest.and_then(|r| r.down_open_since),
        last_gap_at,
        runs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(min: i64) -> DateTime<Utc> {
        "2026-11-24T10:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(min)
    }

    fn deny(seq: u64) -> WireEvent {
        WireEvent {
            seq,
            kind: Some(Kind::Deny),
            at: Some(t(0)),
            sandbox: Some("pmos-docs".into()),
            rule: Some("-".into()),
            reason: Some("no_matching_policy".into()),
            dest_host: Some("httpbin.org".into()),
            dest_port: Some(443),
            os_event_uid: Some(format!("uid-{seq}")),
            ..WireEvent::default()
        }
    }

    fn batch(events: Vec<WireEvent>) -> Batch {
        Batch {
            collector_run: "run-aaaaaaaa".into(),
            events,
        }
    }

    #[test]
    fn a_batch_is_checked_whole() {
        assert!(check_batch(&batch(vec![deny(1), deny(2)]), t(0)).is_ok());
        let bad: Vec<fn(&mut WireEvent)> = vec![
            |e| e.dest_host = Some("evil\nhost".into()),
            |e| e.dest_host = Some("é".into()),
            |e| e.dest_host = Some("x".repeat(300)),
            |e| e.reason = Some("because".into()),
            |e| e.http_path = Some("/v1/x?token=abc".into()),
            |e| e.http_path = Some("/v1/x#frag".into()),
            |e| e.http_method = Some("get".into()),
            |e| e.dest_port = Some(70000),
            |e| e.at = Some(t(60)),
            |e| e.kind = None,
            |e| e.seq = 0,
        ];
        for f in bad {
            let mut e = deny(1);
            f(&mut e);
            assert!(check_batch(&batch(vec![e]), t(0)).is_err());
        }
        assert!(check_batch(&batch(vec![deny(2), deny(1)]), t(0)).is_err());
        assert!(check_batch(&batch(vec![]), t(0)).is_err());
        assert!(check_batch(&batch((1..=501).map(deny).collect()), t(0)).is_err());
        let mut b = batch(vec![deny(1)]);
        b.collector_run = "short".into();
        assert!(check_batch(&b, t(0)).is_err());
        // Unknown fields are refused by the shape itself.
        let raw = r#"{"collector_run":"run-aaaaaaaa","events":[{"seq":1,"kind":"deny","at":"2026-11-24T10:00:00Z","cmd_line":"curl x"}]}"#;
        assert!(serde_json::from_str::<Batch>(raw).is_err());
        let raw = r#"{"collector_run":"run-aaaaaaaa","events":[{"seq":1,"kind":"collector_gap","at":"2026-11-24T10:00:00Z"}]}"#;
        assert!(
            serde_json::from_str::<Batch>(raw).is_err(),
            "the tool server's own kinds"
        );
    }

    #[test]
    fn attribution_needs_a_known_bot_and_one_session() {
        let personal = KnownBot::Personal {
            owner: "ana".into(),
        };
        let one = vec![("ana".to_string(), "tsk_1".to_string())];
        let two = vec![
            ("ana".to_string(), "tsk_1".to_string()),
            ("ben".to_string(), "tsk_2".to_string()),
        ];
        let a = attribute(Some("pmos-pm-ana"), Some(&personal), &[]);
        assert_eq!((a.user.as_str(), a.note), ("ana", "no task"));
        let a = attribute(Some("pmos-pm-ana"), Some(&personal), &two);
        assert_eq!(
            (a.user.as_str(), a.note, a.run_id.as_str()),
            ("ana", "ambiguous", RUN)
        );
        let a = attribute(Some("pmos-docs"), Some(&KnownBot::Team), &one);
        assert_eq!(
            (a.user.as_str(), a.run_id.as_str(), a.note),
            ("ana", "tsk_1", "attributed")
        );
        let a = attribute(Some("pmos-docs"), Some(&KnownBot::Team), &two);
        assert_eq!((a.user.as_str(), a.note), (ACTOR, "ambiguous"));
        let a = attribute(Some("pmos-docs"), Some(&KnownBot::Team), &[]);
        assert_eq!((a.user.as_str(), a.note), (ACTOR, "no task"));
        let a = attribute(Some("pmos-docs"), None, &one);
        assert_eq!((a.bot, a.note), (None, "unknown sandbox"));
        let a = attribute(Some("other-docs"), Some(&KnownBot::Team), &one);
        assert_eq!(a.note, "unknown sandbox");
        assert_eq!(attribute(None, None, &[]).note, "unknown sandbox");
    }

    fn prepared(seqs: &[u64]) -> Vec<Prepared> {
        seqs.iter()
            .map(|s| prepare(&deny(*s), "run-aaaaaaaa", None))
            .collect()
    }

    #[test]
    fn holes_replays_new_runs_and_silence_are_each_recorded() {
        let mut m = MemoryState::default();
        let r = m.ingest("run-aaaaaaaa", prepared(&[1, 2, 3]), t(0));
        assert_eq!((r.stored.len(), r.gaps), (3, 0));
        // A replay of 2 and 3, then 4: only 4 is new.
        let r = m.ingest("run-aaaaaaaa", prepared(&[2, 3, 4]), t(1));
        assert_eq!((r.stored.len(), r.replayed), (1, 2));
        // 5 to 7 missing.
        let r = m.ingest("run-aaaaaaaa", prepared(&[8]), t(2));
        assert_eq!(r.gaps, 1);
        assert_eq!(r.stored[0].0.tool, "sandbox.collector_gap");
        assert_eq!(
            r.stored[0].0.detail.as_deref(),
            Some("seq 5 to 7 missing 3")
        );
        // Silent for five minutes: one open collector_down, once.
        assert!(m.watchdog(t(6)).stored.is_empty());
        let w = m.watchdog(t(8));
        assert_eq!(w.stored.len(), 1);
        assert_eq!(
            w.stored[0].0.detail.as_deref(),
            Some("silent since 2026-11-24T10:02:00Z")
        );
        assert!(m.watchdog(t(9)).stored.is_empty(), "opened once");
        assert_eq!(m.state(t(9)).silent_since, Some(t(2)));
        // Heard from again: closed.
        let r = m.heartbeat(
            &Heartbeat {
                collector_run: "run-aaaaaaaa".into(),
                acked_seq: 8,
            },
            t(10),
        );
        assert_eq!(r.stored[0].0.tool, "sandbox.collector_down");
        assert_eq!(
            r.stored[0].0.detail.as_deref(),
            Some("down from 2026-11-24T10:02:00Z to 2026-11-24T10:10:00Z")
        );
        assert!(m.state(t(10)).receiving);
        // A new run: down from the old run's last heartbeat to the new run's first.
        let r = m.ingest("run-bbbbbbbb", prepared(&[1]), t(12));
        assert_eq!(r.stored[0].0.tool, "sandbox.collector_down");
        assert_eq!(
            r.stored[0].0.detail.as_deref(),
            Some("down from 2026-11-24T10:10:00Z to 2026-11-24T10:12:00Z")
        );
        // A heartbeat claiming more than was stored: a gap.
        let r = m.heartbeat(
            &Heartbeat {
                collector_run: "run-bbbbbbbb".into(),
                acked_seq: 3,
            },
            t(13),
        );
        assert_eq!(
            r.stored[0].0.detail.as_deref(),
            Some("seq 2 to 3 missing 2")
        );
        for (e, _) in &r.stored {
            assert!(is_safe_detail(e.detail.as_deref().unwrap()));
        }
    }

    #[test]
    fn duplicates_by_os_event_uid_are_stored_once() {
        let mut m = MemoryState::default();
        let mut events = prepared(&[1, 2]);
        events[1].fields.os_event_uid = events[0].fields.os_event_uid.clone();
        let r = m.ingest("run-aaaaaaaa", events, t(0));
        assert_eq!((r.stored.len(), r.duplicates), (1, 1));
    }

    #[test]
    fn details_are_fixed_formats_only() {
        for ok in [
            "attributed",
            "no task",
            "held 900 denies past 100 a minute; no task",
            "dropped 12 since 2026-11-24T10:00:00Z",
            "unparsed 3",
            "format unverified",
            "seq 1 to 2 missing 2",
        ] {
            assert!(is_safe_detail(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "held x denies past 100 a minute",
            "attributed; <script>",
            "",
        ] {
            assert!(!is_safe_detail(bad), "{bad}");
        }
    }

    #[test]
    fn the_auditor_sees_a_destination_class() {
        let f = |host: Option<&str>, ip: Option<&str>, reason: Option<&str>| Fields {
            dest_host: host.map(String::from),
            dest_ip: ip.map(String::from),
            reason: reason.map(String::from),
            http_path: Some("/secret/path".into()),
            ..Fields::default()
        };
        assert_eq!(
            dest_class(&f(Some("gateway.pmos.internal"), None, None)),
            "pmos service"
        );
        assert_eq!(
            dest_class(&f(None, Some("10.201.77.10"), None)),
            "pmos service"
        );
        assert_eq!(
            dest_class(&f(None, Some("169.254.169.254"), Some("always_blocked"))),
            "always blocked"
        );
        assert_eq!(dest_class(&f(None, Some("192.168.1.4"), None)), "private");
        assert_eq!(dest_class(&f(None, Some("100.64.0.1"), None)), "private");
        assert_eq!(dest_class(&f(None, Some("8.8.8.8"), None)), "public");
        assert_eq!(dest_class(&f(Some("httpbin.org"), None, None)), "unknown");
        let a = auditor_fields_json(&f(Some("customer.example"), Some("8.8.8.8"), None));
        let text = a.to_string();
        assert!(
            !text.contains("customer.example")
                && !text.contains("8.8.8.8")
                && !text.contains("/secret")
        );
    }
}
