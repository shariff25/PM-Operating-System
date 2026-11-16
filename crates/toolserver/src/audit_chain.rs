//! The unified audit envelope and its hash chain.
//!
//! Every service (tool server, web console, bots, gateway, Slack bot) reports security events
//! in one shape, [`AuditEnvelope`]. The existing [`crate::audit::AuditEvent`] is a subset: it
//! converts into an envelope (`action` `tool.call`), so current writers keep working.
//!
//! The envelope has no free-text field a caller controls: identifiers, hashes and enums only
//! (see `audit_api` for the strict ingestion rules). Message bodies, prompts and replies never
//! belong here; link to them by `run_id` and hash.
//!
//! Integrity: each stored record is a [`ChainRecord`] with a gapless `seq` (from 1) and
//! `hash = SHA-256(canonical_json(record without hashes) || "\n" || prev_hash)`, where
//! `prev_hash` is the previous record's hash (64 zeros for the first). A store assigns `seq`
//! and computes the hash under one serialisation point (a locked head row in Postgres, a
//! mutex in memory), so two writers can never fork the chain.
//!
//! Retention: [`AuditRetention`] (`PMOS_AUDIT_RETENTION_DAYS`, default 90). Pruning deletes a
//! prefix of the chain in one transaction after writing a [`Checkpoint`] naming the last
//! pruned `seq` and its hash, so verification starts from the checkpoint and still passes.

use std::sync::Mutex;

use chrono::{DateTime, Duration, SecondsFormat, SubsecRound, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit::{AuditEvent, Client, Outcome};
use crate::store::{StoreError, StoreResult};

/// `prev_hash` of the first record in a chain.
pub const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Version of the envelope and of the canonical form the hash covers.
pub const SCHEMA_VERSION: i32 = 1;

/// Default audit retention, in days.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// How many records one prune call removes at most.
pub const DEFAULT_PRUNE_BATCH: usize = 5_000;

// ----- envelope --------------------------------------------------------------------------

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

string_enum! {
    /// Which service reported the event.
    Source {
        Console => "console",
        Toolserver => "toolserver",
        Bots => "bots",
        Gateway => "gateway",
        Slack => "slack",
        IdpSync => "idp-sync",
    }
}

string_enum! {
    /// What kind of actor made the call.
    ActorType {
        Human => "human",
        Bot => "bot",
        Service => "service",
        System => "system",
    }
}

string_enum! {
    /// How the actor reached PM OS.
    Channel {
        Web => "web",
        Slack => "slack",
        Api => "api",
        Mcp => "mcp",
        System => "system",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    #[serde(rename = "type")]
    pub kind: ActorType,
    /// The person's PM OS handle (or the bot or service name).
    pub handle: String,
    /// IdP object id and tenant id (Azure AD `oid`, `tid`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idp_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idp_tid: Option<String>,
    /// Web session id, or the MCP or bot session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The transport credential that carried the event.
    pub client: Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

/// Request context: ids and digests only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Keyed digest of the source address (see [`hash_source_addr`]), never the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ip_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent_hash: Option<String>,
}

/// One audit event, before the chain assigns `seq` and hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub at: DateTime<Utc>,
    pub source: Source,
    pub actor: Actor,
    pub channel: Channel,
    /// Dotted verb, such as `auth.sign_in`, `role.grant`, `approval.decide`, `tool.call`.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    pub outcome: Outcome,
    /// Short code, such as `missing_role`. Never prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub request: RequestContext,
    // The fields the original audit event had, kept as a subset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    // Content hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    /// Legacy tool-server detail code. Not accepted from `POST /internal/audit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl From<&AuditEvent> for AuditEnvelope {
    /// The tool server's own events: `action` `tool.call`, the channel from the client.
    fn from(e: &AuditEvent) -> Self {
        Self {
            at: e.at,
            source: Source::Toolserver,
            actor: Actor {
                kind: if e.bot.is_some() {
                    ActorType::Bot
                } else {
                    ActorType::Human
                },
                handle: e.user.clone(),
                idp_oid: None,
                idp_tid: None,
                session_id: None,
                client: e.client,
            },
            channel: match e.client {
                Client::Mcp => Channel::Mcp,
                Client::Orchestrator => Channel::Api,
                Client::Bot => Channel::Slack,
            },
            action: "tool.call".into(),
            target: Some(Target {
                kind: "tool".into(),
                id: e.tool.clone(),
            }),
            outcome: e.outcome,
            reason_code: None,
            request: RequestContext::default(),
            run_id: Some(e.run_id.clone()),
            skill: e.skill.clone(),
            bot: e.bot.clone(),
            tool: Some(e.tool.clone()),
            sources: e.sources.clone(),
            args_hash: e.args_hash.clone(),
            before_hash: None,
            after_hash: None,
            output_ref: e.output_ref.clone(),
            detail: e.detail.clone(),
        }
    }
}

impl AuditEnvelope {
    fn canonical_value(&self, seq: i64, version: i32) -> Value {
        json!({
            "seq": seq,
            "schema_version": version,
            "at": micros(self.at),
            "source": self.source.as_str(),
            "actor": {
                "type": self.actor.kind.as_str(),
                "handle": self.actor.handle,
                "idp_oid": self.actor.idp_oid,
                "idp_tid": self.actor.idp_tid,
                "session_id": self.actor.session_id,
                "client": self.actor.client.as_str(),
            },
            "channel": self.channel.as_str(),
            "action": self.action,
            "target": self.target.as_ref().map(|t| json!({"type": t.kind, "id": t.id})),
            "outcome": crate::audit::outcome_str(self.outcome),
            "reason_code": self.reason_code,
            "request": {
                "request_id": self.request.request_id,
                "source_ip_hash": self.request.source_ip_hash,
                "user_agent_hash": self.request.user_agent_hash,
            },
            "run_id": self.run_id,
            "skill": self.skill,
            "bot": self.bot,
            "tool": self.tool,
            "sources": self.sources,
            "args_hash": self.args_hash,
            "before_hash": self.before_hash,
            "after_hash": self.after_hash,
            "output_ref": self.output_ref,
            "detail": self.detail,
        })
    }
}

fn micros(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// JSON with object keys sorted at every level and no whitespace, so the same record always
/// hashes the same whatever the `serde_json` feature set.
pub fn canonical_json(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    write(&map[k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn sha256_hex(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

/// A keyed digest of a source address or user agent, safe to store (a plain hash of an IPv4
/// address can be reversed by trying every address).
pub fn hash_source_addr(key: &[u8], value: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(value.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

// ----- chain records ---------------------------------------------------------------------

/// A stored event: the envelope plus its place in the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainRecord {
    pub seq: i64,
    pub schema_version: i32,
    #[serde(flatten)]
    pub envelope: AuditEnvelope,
    pub prev_hash: String,
    pub hash: String,
}

impl ChainRecord {
    /// Place `envelope` after a record whose hash is `prev_hash`. `at` is cut to microseconds
    /// (what Postgres keeps) so the hash is the same after a round trip.
    pub fn seal(seq: i64, mut envelope: AuditEnvelope, prev_hash: &str) -> Self {
        envelope.at = envelope.at.trunc_subsecs(6);
        let hash = compute_hash(seq, SCHEMA_VERSION, &envelope, prev_hash);
        Self {
            seq,
            schema_version: SCHEMA_VERSION,
            envelope,
            prev_hash: prev_hash.to_string(),
            hash,
        }
    }

    /// The hash this record should have, from its own content.
    pub fn expected_hash(&self) -> String {
        compute_hash(
            self.seq,
            self.schema_version,
            &self.envelope,
            &self.prev_hash,
        )
    }
}

pub fn compute_hash(seq: i64, version: i32, envelope: &AuditEnvelope, prev_hash: &str) -> String {
    let body = canonical_json(&envelope.canonical_value(seq, version));
    sha256_hex(&format!("{body}\n{prev_hash}"))
}

/// What a store returns for a recorded envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recorded {
    pub seq: i64,
    pub hash: String,
}

/// Written when a prefix of the chain is pruned. Checkpoints form their own chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    /// The last pruned `seq`; the first remaining record is `through_seq + 1`.
    pub through_seq: i64,
    /// Hash of that record: the remaining chain's first `prev_hash`.
    pub through_hash: String,
    pub rows_pruned: i64,
    /// Records older than this were eligible.
    pub cutoff: DateTime<Utc>,
    pub pruned_at: DateTime<Utc>,
    /// Hash of the previous checkpoint, or [`GENESIS_HASH`].
    pub prev_hash: String,
    pub hash: String,
}

impl Checkpoint {
    pub fn seal(
        through_seq: i64,
        through_hash: &str,
        rows_pruned: i64,
        cutoff: DateTime<Utc>,
        pruned_at: DateTime<Utc>,
        prev_hash: &str,
    ) -> Self {
        let cutoff = cutoff.trunc_subsecs(6);
        let pruned_at = pruned_at.trunc_subsecs(6);
        let hash = Self::compute(
            through_seq,
            through_hash,
            rows_pruned,
            cutoff,
            pruned_at,
            prev_hash,
        );
        Self {
            through_seq,
            through_hash: through_hash.to_string(),
            rows_pruned,
            cutoff,
            pruned_at,
            prev_hash: prev_hash.to_string(),
            hash,
        }
    }

    fn compute(
        through_seq: i64,
        through_hash: &str,
        rows_pruned: i64,
        cutoff: DateTime<Utc>,
        pruned_at: DateTime<Utc>,
        prev_hash: &str,
    ) -> String {
        let body = canonical_json(&json!({
            "through_seq": through_seq,
            "through_hash": through_hash,
            "rows_pruned": rows_pruned,
            "cutoff": micros(cutoff),
            "pruned_at": micros(pruned_at),
            "prev_hash": prev_hash,
        }));
        sha256_hex(&format!("checkpoint\n{body}"))
    }

    pub fn expected_hash(&self) -> String {
        Self::compute(
            self.through_seq,
            &self.through_hash,
            self.rows_pruned,
            self.cutoff,
            self.pruned_at,
            &self.prev_hash,
        )
    }
}

// ----- verification ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    /// A record's content no longer matches its hash: edited.
    HashMismatch,
    /// A record doesn't point at the previous record's hash: reordered or spliced.
    PrevHashMismatch,
    /// A `seq` is missing or repeated: deleted or inserted.
    SeqGap,
    /// A checkpoint is forged, or the chain doesn't start where the checkpoint says.
    CheckpointMismatch,
    /// The chain head disagrees with the last record: tail cut off.
    HeadMismatch,
}

/// The first place the chain stops verifying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Break {
    pub seq: i64,
    pub kind: BreakKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    /// Records walked.
    pub checked: u64,
    pub first_seq: Option<i64>,
    pub last_seq: Option<i64>,
    /// Checkpoints from earlier pruning.
    pub checkpoints: usize,
    pub first_break: Option<Break>,
}

/// Walks a chain one record at a time, so a store can stream rows.
pub struct ChainVerifier {
    expected_seq: i64,
    expected_prev: String,
    checkpoints: usize,
    checked: u64,
    first_seq: Option<i64>,
    last: Option<(i64, String)>,
}

impl ChainVerifier {
    /// Start from the checkpoints (oldest first), or from the start of the chain without any.
    pub fn new(checkpoints: &[Checkpoint]) -> Result<Self, Break> {
        let mut prev = GENESIS_HASH.to_string();
        let mut through = 0;
        for c in checkpoints {
            let bad = |message: &str| Break {
                seq: c.through_seq,
                kind: BreakKind::CheckpointMismatch,
                message: message.to_string(),
            };
            if c.hash != c.expected_hash() {
                return Err(bad("checkpoint content doesn't match its hash"));
            }
            if c.prev_hash != prev {
                return Err(bad("checkpoint doesn't follow the previous checkpoint"));
            }
            if c.through_seq <= through {
                return Err(bad("checkpoints don't advance"));
            }
            prev = c.hash.clone();
            through = c.through_seq;
        }
        let (expected_seq, expected_prev) = match checkpoints.last() {
            Some(c) => (c.through_seq + 1, c.through_hash.clone()),
            None => (1, GENESIS_HASH.to_string()),
        };
        Ok(Self {
            expected_seq,
            expected_prev,
            checkpoints: checkpoints.len(),
            checked: 0,
            first_seq: None,
            last: checkpoints
                .last()
                .map(|c| (c.through_seq, c.through_hash.clone())),
        })
    }

    pub fn push(&mut self, rec: &ChainRecord) -> Result<(), Break> {
        let fail = |kind, message: String| Break {
            seq: rec.seq,
            kind,
            message,
        };
        if rec.seq != self.expected_seq {
            return Err(fail(
                BreakKind::SeqGap,
                format!("expected seq {}, found {}", self.expected_seq, rec.seq),
            ));
        }
        if rec.prev_hash != self.expected_prev {
            return Err(fail(
                BreakKind::PrevHashMismatch,
                "prev_hash doesn't match the previous record".into(),
            ));
        }
        if rec.hash != rec.expected_hash() {
            return Err(fail(
                BreakKind::HashMismatch,
                "record content doesn't match its hash".into(),
            ));
        }
        self.first_seq.get_or_insert(rec.seq);
        self.expected_seq = rec.seq + 1;
        self.expected_prev = rec.hash.clone();
        self.last = Some((rec.seq, rec.hash.clone()));
        self.checked += 1;
        Ok(())
    }

    /// Compare the end of the chain with the stored head, if the store keeps one.
    pub fn finish(self, head: Option<(i64, String)>) -> VerifyReport {
        let first_break = head.and_then(|(seq, hash)| {
            let last = self.last.clone().unwrap_or((0, GENESIS_HASH.to_string()));
            (last != (seq, hash)).then(|| Break {
                seq: last.0,
                kind: BreakKind::HeadMismatch,
                message: format!("chain head is at seq {seq}, last record is seq {}", last.0),
            })
        });
        self.report(first_break)
    }

    pub fn report(&self, first_break: Option<Break>) -> VerifyReport {
        VerifyReport {
            ok: first_break.is_none(),
            checked: self.checked,
            first_seq: self.first_seq,
            last_seq: self.last.as_ref().map(|(s, _)| *s),
            checkpoints: self.checkpoints,
            first_break,
        }
    }
}

/// A report for a chain that failed before or during the walk.
pub fn failed_report(checked: u64, checkpoints: usize, b: Break) -> VerifyReport {
    VerifyReport {
        ok: false,
        checked,
        first_seq: None,
        last_seq: None,
        checkpoints,
        first_break: Some(b),
    }
}

// ----- retention and pruning -------------------------------------------------------------

/// How long audit records are kept: `PMOS_AUDIT_RETENTION_DAYS`, default 90. `0` keeps them
/// forever (pruning off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditRetention {
    pub days: i64,
}

impl Default for AuditRetention {
    fn default() -> Self {
        Self {
            days: DEFAULT_RETENTION_DAYS,
        }
    }
}

impl AuditRetention {
    pub const ENV: &'static str = "PMOS_AUDIT_RETENTION_DAYS";

    /// Read the setting through `get` (the real environment, or a test's map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match get(Self::ENV).map(|v| v.trim().to_string()) {
            None => Ok(Self::default()),
            Some(v) if v.is_empty() => Ok(Self::default()),
            Some(v) => match v.parse::<i64>() {
                Ok(days) if (0..=36_500).contains(&days) => Ok(Self { days }),
                _ => Err(format!(
                    "{} must be a whole number of days from 0 (keep forever) to 36500, got `{v}`",
                    Self::ENV
                )),
            },
        }
    }

    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Records before this are due for pruning; `None` when retention is off.
    pub fn cutoff(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (self.days > 0).then(|| now - Duration::days(self.days))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub pruned: u64,
    pub through_seq: Option<i64>,
    pub cutoff: DateTime<Utc>,
}

/// Prune everything older than the retention window, in batches. A job calls this; nothing
/// schedules it yet.
pub async fn prune_retained(
    sink: &dyn crate::audit::AuditSink,
    retention: AuditRetention,
    now: DateTime<Utc>,
    batch: usize,
) -> StoreResult<u64> {
    let Some(cutoff) = retention.cutoff(now) else {
        return Ok(0);
    };
    let mut total = 0;
    loop {
        let report = sink.prune_before(cutoff, batch).await?;
        total += report.pruned;
        if report.pruned < batch as u64 {
            return Ok(total);
        }
    }
}

// ----- the in-memory chain ---------------------------------------------------------------

#[derive(Default)]
struct MemoryChainInner {
    records: Vec<ChainRecord>,
    checkpoints: Vec<Checkpoint>,
    head: Option<(i64, String)>,
}

/// The chain held in memory: what `MemoryAudit` uses, and the reference for the Postgres one.
#[derive(Default)]
pub struct MemoryChain {
    inner: Mutex<MemoryChainInner>,
}

impl MemoryChain {
    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryChainInner> {
        self.inner.lock().expect("audit chain lock")
    }

    pub fn append(&self, envelope: AuditEnvelope) -> Recorded {
        let mut inner = self.lock();
        let (seq, prev) = match &inner.head {
            Some((seq, hash)) => (seq + 1, hash.clone()),
            None => (1, GENESIS_HASH.to_string()),
        };
        let rec = ChainRecord::seal(seq, envelope, &prev);
        let recorded = Recorded {
            seq,
            hash: rec.hash.clone(),
        };
        inner.head = Some((seq, rec.hash.clone()));
        inner.records.push(rec);
        recorded
    }

    pub fn list(&self, after_seq: i64, limit: usize) -> Vec<ChainRecord> {
        self.lock()
            .records
            .iter()
            .filter(|r| r.seq > after_seq)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        self.lock().checkpoints.clone()
    }

    pub fn verify(&self) -> VerifyReport {
        let inner = self.lock();
        let mut v = match ChainVerifier::new(&inner.checkpoints) {
            Ok(v) => v,
            Err(b) => return failed_report(0, inner.checkpoints.len(), b),
        };
        for rec in &inner.records {
            if let Err(b) = v.push(rec) {
                return v.report(Some(b));
            }
        }
        v.finish(inner.head.clone())
    }

    /// Drop the oldest records, at most `batch`, that are all older than `cutoff`, after
    /// recording a checkpoint.
    pub fn prune(&self, cutoff: DateTime<Utc>, batch: usize, now: DateTime<Utc>) -> PruneReport {
        let mut inner = self.lock();
        let n = inner
            .records
            .iter()
            .take_while(|r| r.envelope.at < cutoff)
            .take(batch)
            .count();
        if n == 0 {
            return PruneReport {
                pruned: 0,
                through_seq: None,
                cutoff,
            };
        }
        let last = &inner.records[n - 1];
        let prev = inner
            .checkpoints
            .last()
            .map_or(GENESIS_HASH.to_string(), |c| c.hash.clone());
        let checkpoint =
            Checkpoint::seal(last.seq, &last.hash, n as i64, cutoff, now, &prev);
        let through_seq = checkpoint.through_seq;
        inner.checkpoints.push(checkpoint);
        inner.records.drain(..n);
        PruneReport {
            pruned: n as u64,
            through_seq: Some(through_seq),
            cutoff,
        }
    }

    /// Change stored records, for tests that check tampering is detected.
    #[doc(hidden)]
    pub fn tamper(&self, f: impl FnOnce(&mut Vec<ChainRecord>)) {
        f(&mut self.lock().records);
    }
}

pub fn unsupported() -> StoreError {
    StoreError("this audit sink doesn't keep a hash chain".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(handle: &str, at: &str) -> AuditEnvelope {
        AuditEnvelope {
            at: at.parse().unwrap(),
            source: Source::Console,
            actor: Actor {
                kind: ActorType::Human,
                handle: handle.into(),
                idp_oid: Some("oid-1".into()),
                idp_tid: None,
                session_id: Some("sid-1".into()),
                client: Client::Orchestrator,
            },
            channel: Channel::Web,
            action: "auth.sign_in".into(),
            target: None,
            outcome: Outcome::Ok,
            reason_code: None,
            request: RequestContext::default(),
            run_id: None,
            skill: None,
            bot: None,
            tool: None,
            sources: vec![],
            args_hash: None,
            before_hash: None,
            after_hash: None,
            output_ref: None,
            detail: None,
        }
    }

    fn filled() -> MemoryChain {
        let c = MemoryChain::default();
        for (i, day) in ["2026-01-01", "2026-01-02", "2026-06-01", "2026-06-02"]
            .iter()
            .enumerate()
        {
            c.append(env(&format!("u{i}"), &format!("{day}T10:00:00.123456789Z")));
        }
        c
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let v = json!({"b": 1, "a": {"d": [2, {"z": 1, "y": 2}], "c": null}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":null,"d":[2,{"y":2,"z":1}]},"b":1}"#
        );
    }

    #[test]
    fn chain_links_and_verifies() {
        let c = filled();
        let recs = c.list(0, 100);
        assert_eq!(recs.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(recs[0].prev_hash, GENESIS_HASH);
        assert_eq!(recs[1].prev_hash, recs[0].hash);
        // Time is kept to microseconds, so a database round trip hashes the same.
        assert_eq!(micros(recs[0].envelope.at), "2026-01-01T10:00:00.123456Z");
        let report = c.verify();
        assert!(report.ok, "{report:?}");
        assert_eq!(report.checked, 4);
    }

    #[test]
    fn editing_a_record_is_detected_at_that_record() {
        let c = filled();
        c.tamper(|r| r[1].envelope.outcome = Outcome::Denied);
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (2, BreakKind::HashMismatch));
    }

    #[test]
    fn editing_and_rehashing_a_record_breaks_the_next_link() {
        let c = filled();
        c.tamper(|r| {
            r[1].envelope.actor.handle = "mallory".into();
            r[1].hash = r[1].expected_hash();
        });
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (3, BreakKind::PrevHashMismatch));
    }

    #[test]
    fn deleting_inserting_or_truncating_is_detected() {
        let c = filled();
        c.tamper(|r| {
            r.remove(1);
        });
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (3, BreakKind::SeqGap));

        let c = filled();
        c.tamper(|r| {
            r.truncate(3);
        });
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (3, BreakKind::HeadMismatch));

        let c = filled();
        c.tamper(|r| {
            r.remove(0);
        });
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (2, BreakKind::SeqGap));
    }

    #[test]
    fn pruning_keeps_the_chain_verifiable() {
        let c = filled();
        let now: DateTime<Utc> = "2026-07-01T00:00:00Z".parse().unwrap();
        let cutoff = now - Duration::days(90);
        let report = c.prune(cutoff, 100, now);
        assert_eq!((report.pruned, report.through_seq), (2, Some(2)));
        let after = c.verify();
        assert!(after.ok, "{after:?}");
        assert_eq!((after.checked, after.first_seq, after.checkpoints), (2, Some(3), 1));
        // New events carry on from the head, and a second prune chains its checkpoint.
        c.append(env("u9", "2026-07-02T00:00:00Z"));
        let report = c.prune("2026-06-15T00:00:00Z".parse().unwrap(), 100, now);
        assert_eq!(report.pruned, 2);
        let after = c.verify();
        assert!(after.ok, "{after:?}");
        assert_eq!(after.checkpoints, 2);
    }

    #[test]
    fn prune_respects_the_batch_and_stops_at_a_newer_record() {
        let c = filled();
        let now: DateTime<Utc> = "2026-07-01T00:00:00Z".parse().unwrap();
        assert_eq!(c.prune(now, 1, now).pruned, 1);
        assert_eq!(c.prune("2026-01-01T00:00:00Z".parse().unwrap(), 10, now).pruned, 0);
        assert!(c.verify().ok);
    }

    #[test]
    fn a_forged_checkpoint_or_a_prune_without_one_is_detected() {
        let c = filled();
        let now: DateTime<Utc> = "2026-07-01T00:00:00Z".parse().unwrap();
        c.prune(now - Duration::days(90), 100, now);
        c.inner.lock().unwrap().checkpoints[0].rows_pruned = 99;
        let b = c.verify().first_break.unwrap();
        assert_eq!(b.kind, BreakKind::CheckpointMismatch);

        // Records removed with no checkpoint: the chain no longer starts at seq 1.
        let c = filled();
        c.tamper(|r| {
            r.drain(..2);
        });
        let b = c.verify().first_break.unwrap();
        assert_eq!((b.seq, b.kind), (3, BreakKind::SeqGap));
    }

    #[test]
    fn retention_setting() {
        let get = |v: Option<&'static str>| {
            AuditRetention::from_lookup(move |_| v.map(String::from))
        };
        assert_eq!(get(None).unwrap().days, 90);
        assert_eq!(get(Some("30")).unwrap().days, 30);
        assert_eq!(get(Some("0")).unwrap().cutoff(Utc::now()), None);
        assert!(get(Some("-1")).is_err());
        assert!(get(Some("ninety")).is_err());
        let now: DateTime<Utc> = "2026-04-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            get(None).unwrap().cutoff(now),
            Some("2026-01-01T00:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn legacy_events_convert_to_tool_calls() {
        let e = AuditEvent {
            client: Client::Bot,
            at: "2026-11-14T00:00:00Z".parse().unwrap(),
            run_id: "run-1".into(),
            user: "ana".into(),
            skill: None,
            bot: Some("scout".into()),
            tool: "slack.search".into(),
            outcome: Outcome::Denied,
            sources: vec![],
            args_hash: Some("abc".into()),
            output_ref: None,
            detail: Some("scope".into()),
        };
        let env = AuditEnvelope::from(&e);
        assert_eq!((env.action.as_str(), env.channel), ("tool.call", Channel::Slack));
        assert_eq!(env.actor.kind, ActorType::Bot);
        assert_eq!(env.target.unwrap().id, "slack.search");
    }

    #[test]
    fn source_address_hash_is_keyed() {
        let a = hash_source_addr(b"k1", "203.0.113.7");
        assert_eq!(a.len(), 64);
        assert_ne!(a, hash_source_addr(b"k2", "203.0.113.7"));
        assert_ne!(a, sha256_hex("203.0.113.7"));
    }
}
