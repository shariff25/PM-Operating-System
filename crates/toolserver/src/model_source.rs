//! The tool server's cross-check of the model gateway (slice H4,
//! `docs/openshell-any-host-spec.md` §3.6, "The runtime and the tool server cross-check the
//! gateway" and "Keeping every caller on one source"), and from slice H12 the switch
//! acknowledgement and the data-class ceiling.
//!
//! A worker calls the gateway directly with the tool results it gets here, so the tool server
//! makes sure the gateway serves the source this stack may use:
//!
//! - **At a session's open** the gateway's reported source must be this tool server's
//!   `PMOS_MODEL_SOURCE` (or the stub, which sends no prompt anywhere); a missing or unreadable
//!   answer refuses the session (fail closed). The session records the source it opened under.
//! - **At every tool call** the gateway's source must still be the one the session opened
//!   under. On a mismatch, or no readable answer, the call is refused (`409
//!   model_source_changed`), the session is closed (its token stops working), and the audit
//!   log gets `system.model_source` once per change and `session.closed` with the reason.
//!
//! The gateway's answer is read from its `/readyz` (no token: the tool server must not be able
//! to call the model), at most 10 seconds old and dropped on any failed read, so for at most 10
//! seconds after a gateway restart a tool result may still reach a worker; the worker's next
//! model call is then refused by the gateway itself (X4).
//!
//! What each session opened under is kept in memory: after a tool server restart, a session
//! opened before it has no record and its next tool call is refused and the session closed.
//!
//! **Slice H12.**
//!
//! - **The switch acknowledgement (r6 V6, r7 X3).** The database's history of the sources it
//!   has served ([`History`]; permanent, like `held_real_data`) decides whether a gateway that
//!   reports `hosted:<name>` may be used: when the database has ever served `local` or another
//!   `hosted:` entry, or has the `held_real_data` marker and has never served `hosted:<name>`,
//!   no session opens until `sudo pmos-admin hosted acknowledge-switch` records an
//!   acknowledgement for `hosted:<name>` ([`acknowledgement_needed`]). One holds until the
//!   database next serves another real source. `stub` and `hosted-test` are recorded but never
//!   count. The runtime asks for the same answer (`GET /internal/system/model-source-status`).
//! - **The test-only list.** A gateway reporting `hosted:stack-test` is taken only by a
//!   `hosted` stack that passed every fake-data check at start ([`SourceGuard::with_fake_eligible`]).
//! - **The ceiling (r6 V11).** Under `hosted` the gateway's effective `max_data_class` is kept
//!   from its last reading ([`SourceGuard::ceiling`]); a tool result above it is refused.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use pmos_contract::model::Sensitivity;
use pmos_contract::model_source::{hosted_name, reported_is_real, ModelSource, STACK_TEST_SOURCE};
use pmos_runtime::model_source::{GatewayReport, GatewaySource};

/// The code of a refusal for a changed (or unreadable) model source.
pub const MODEL_SOURCE_CHANGED: &str = "model_source_changed";

/// The code of a refusal while a switch to a hosted entry waits for its acknowledgement.
pub const ACKNOWLEDGEMENT_REQUIRED: &str = "model_source_acknowledgement_required";

/// The audit action recorded once when the gateway's source is seen to change.
pub const SYSTEM_MODEL_SOURCE: &str = "system.model_source";

/// The audit action recorded when a switch is acknowledged (metadata an Auditor sees).
pub const SYSTEM_MODEL_SOURCE_ACKNOWLEDGED: &str = "system.model_source_acknowledged";

/// The audit action recorded when a session opens (slice H12, §3.9).
pub const SESSION_OPEN: &str = "session.open";

/// The audit action recorded when a session is closed for it.
pub const SESSION_CLOSED: &str = "session.closed";

/// The most sessions whose source is kept; past it, the oldest half is forgotten (their next
/// call is refused, which only closes them early).
const MAX_SESSIONS: usize = 100_000;

/// One source in the database's history: when it was first and last served.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceRecord {
    pub source: String,
    /// `local` or a `hosted:<name>` but the test-only one.
    pub real: bool,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
}

/// One acknowledgement of a switch to `to`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Acknowledgement {
    pub to: String,
    pub by: String,
    pub reason: String,
    pub at: DateTime<Utc>,
}

/// The rule of r7, X3: is an acknowledgement needed before `to` (`hosted:<name>`) is served?
/// Needed when the database has served a real source other than `to`, or has held real data
/// and never served `to`; and no acknowledgement for `to` is in force, one being in force when
/// no real source other than `to` has been served since it was made. `stub` and `hosted-test`
/// never count. Not needed for any other `to`.
pub fn acknowledgement_needed(
    to: &str,
    history: &[SourceRecord],
    held_real_data: bool,
    acks: &[Acknowledgement],
) -> bool {
    if hosted_name(to).is_none() || to == STACK_TEST_SOURCE {
        return false;
    }
    let others: Vec<&SourceRecord> = history
        .iter()
        .filter(|r| r.real && r.source != to)
        .collect();
    let served_to = history.iter().any(|r| r.source == to);
    let needs = !others.is_empty() || (held_real_data && !served_to);
    if !needs {
        return false;
    }
    let in_force = acks
        .iter()
        .filter(|a| a.to == to)
        .any(|a| others.iter().all(|r| r.last_at <= a.at));
    !in_force
}

/// Where the history and the acknowledgements are kept: the tool server's database, or memory
/// (`PMOS_DEV=1` and tests).
#[derive(Clone)]
pub enum History {
    Memory(std::sync::Arc<Mutex<(BTreeMap<String, SourceRecord>, Vec<Acknowledgement>)>>),
    Pg(crate::pg::PgStore),
}

impl Default for History {
    fn default() -> Self {
        Self::Memory(Default::default())
    }
}

impl History {
    /// Records that `source` was served now; returns the source served last before it.
    pub async fn served(&self, source: &str, now: DateTime<Utc>) -> Result<Option<String>, String> {
        match self {
            Self::Memory(m) => {
                let mut m = m.lock().expect("history lock");
                let last =
                    m.0.values()
                        .max_by_key(|r| r.last_at)
                        .map(|r| r.source.clone());
                m.0.entry(source.to_string())
                    .and_modify(|r| r.last_at = now)
                    .or_insert(SourceRecord {
                        source: source.to_string(),
                        real: reported_is_real(source),
                        first_at: now,
                        last_at: now,
                    });
                Ok(last)
            }
            Self::Pg(pg) => pg
                .source_served(source, reported_is_real(source), now)
                .await
                .map_err(|e| format!("recording the model source: {e}")),
        }
    }

    pub async fn records(&self) -> Result<Vec<SourceRecord>, String> {
        match self {
            Self::Memory(m) => Ok(m
                .lock()
                .expect("history lock")
                .0
                .values()
                .cloned()
                .collect()),
            Self::Pg(pg) => pg
                .source_history()
                .await
                .map_err(|e| format!("reading the model-source history: {e}")),
        }
    }

    pub async fn acknowledgements(&self) -> Result<Vec<Acknowledgement>, String> {
        match self {
            Self::Memory(m) => Ok(m.lock().expect("history lock").1.clone()),
            Self::Pg(pg) => pg
                .source_acknowledgements()
                .await
                .map_err(|e| format!("reading the acknowledgements: {e}")),
        }
    }

    pub async fn acknowledge(&self, ack: Acknowledgement) -> Result<(), String> {
        match self {
            Self::Memory(m) => {
                m.lock().expect("history lock").1.push(ack);
                Ok(())
            }
            Self::Pg(pg) => pg
                .acknowledge_source(&ack)
                .await
                .map_err(|e| format!("recording the acknowledgement: {e}")),
        }
    }
}

/// What `GET /internal/system/model-source-status` answers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SwitchStatus {
    /// The gateway's source as last read, or `None` when it can't be read.
    pub gateway: Option<String>,
    /// `hosted:<name>`'s entry name, when the gateway serves one.
    pub hosted_name: Option<String>,
    /// Under `hosted`: the gateway's effective data-class ceiling.
    pub max_data_class: Option<String>,
    /// Whether the gateway is fake-data mode (`hosted-test`, the test-only list).
    pub fake_data: bool,
    pub acknowledgement_needed: bool,
    /// May sessions open now (and the runtime dispatch)? `false` whenever anything is unknown.
    pub ok: bool,
    /// Why not, in words with no URL.
    pub refused: Option<String>,
    pub held_real_data: bool,
    pub history: Vec<SourceRecord>,
}

/// Checks the gateway's source at session opens and tool calls.
pub struct SourceGuard {
    own: ModelSource,
    gateway: GatewaySource,
    /// By session token hash: the source the session opened under, and when (for pruning).
    sessions: Mutex<HashMap<String, (String, u64)>>,
    next: Mutex<u64>,
    /// The gateway source last seen at a refused call, so `system.model_source` is written once
    /// per change, not once per refused call.
    last_change: Mutex<Option<String>>,
    history: History,
    held_real_data: bool,
    /// Whether this stack passed every fake-data check at start (for `hosted:stack-test`).
    fake_eligible: bool,
    /// The last report read (the ceiling, the list's digest and commit for `session.open`).
    last_report: Mutex<Option<GatewayReport>>,
}

impl std::fmt::Debug for SourceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceGuard")
            .field("own", &self.own)
            .finish_non_exhaustive()
    }
}

/// Why a session or a call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The gateway's source couldn't be read (why, with no URL).
    Unreadable(String),
    /// It serves another source: the one it serves.
    Changed(String),
    /// It serves `hosted:<name>`, and the switch hasn't been acknowledged (X3).
    AcknowledgementNeeded(String),
}

impl SourceGuard {
    /// `gateway_url`: `PMOS_GATEWAY_URL`.
    pub fn new(own: ModelSource, gateway_url: &str) -> Self {
        Self::with_gateway(own, GatewaySource::new(gateway_url))
    }

    /// [`SourceGuard::new`] with the gateway's source read through `gateway` (tests use a
    /// shorter cache).
    pub fn with_gateway(own: ModelSource, gateway: GatewaySource) -> Self {
        Self {
            own,
            gateway,
            sessions: Mutex::default(),
            next: Mutex::new(0),
            last_change: Mutex::default(),
            history: History::default(),
            held_real_data: false,
            fake_eligible: own.fake_data_only(),
            last_report: Mutex::default(),
        }
    }

    /// The same, reusing a reading for `ttl` (tests).
    pub fn with_ttl(own: ModelSource, gateway_url: &str, ttl: Duration) -> Self {
        Self::with_gateway(own, GatewaySource::new(gateway_url).with_ttl(ttl))
    }

    /// Keeps the history in `history` (the database, in `main`).
    pub fn with_history(mut self, history: History) -> Self {
        self.history = history;
        self
    }

    /// Whether the database has the `held_real_data` marker (read at start, which is the only
    /// time it is written).
    pub fn with_held_real_data(mut self, held: bool) -> Self {
        self.held_real_data = held;
        self
    }

    /// Whether this stack passed every fake-data check at start (fake connectors, no GitHub
    /// memory, test content, no `held_real_data`): only then is a gateway reporting
    /// `hosted:stack-test` taken under `hosted`.
    pub fn with_fake_eligible(mut self, eligible: bool) -> Self {
        self.fake_eligible = eligible;
        self
    }

    pub fn own(&self) -> ModelSource {
        self.own
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether this stack takes `reported` at all (before the acknowledgement).
    fn takes(&self, reported: &str) -> bool {
        self.own.accepts(reported)
            || (self.own == ModelSource::Hosted
                && reported == STACK_TEST_SOURCE
                && self.fake_eligible)
    }

    async fn read(&self) -> Result<GatewayReport, Refusal> {
        match self.gateway.report().await {
            Ok(r) => {
                *self.last_report.lock().expect("report lock") = Some(r.clone());
                Ok(r)
            }
            Err(why) => {
                *self.last_report.lock().expect("report lock") = None;
                Err(Refusal::Unreadable(why))
            }
        }
    }

    /// The gateway's last report (for `session.open`'s digest and commit).
    pub fn last_report(&self) -> Option<GatewayReport> {
        self.last_report.lock().expect("report lock").clone()
    }

    /// Under `hosted`: the gateway's effective data-class ceiling, from its last reading. With
    /// no reading, or a gateway that reports none under `hosted`, the lowest class (fail
    /// closed). `None` under any other source: no ceiling.
    pub fn ceiling(&self) -> Option<Sensitivity> {
        if self.own != ModelSource::Hosted {
            return None;
        }
        let report = self.last_report.lock().expect("report lock").clone();
        match report {
            Some(r) if r.source == pmos_contract::model_source::STUB => None,
            Some(r) => Some(r.ceiling().unwrap_or(Sensitivity::Public)),
            None => Some(Sensitivity::Public),
        }
    }

    /// At a session's open: the source the gateway serves, if this stack may use it now.
    pub async fn at_open(&self) -> Result<String, Refusal> {
        let report = self.read().await?;
        let s = report.source;
        if !self.takes(&s) {
            return Err(Refusal::Changed(s));
        }
        if self.needs_acknowledgement(&s).await? {
            return Err(Refusal::AcknowledgementNeeded(s));
        }
        Ok(s)
    }

    async fn needs_acknowledgement(&self, source: &str) -> Result<bool, Refusal> {
        if hosted_name(source).is_none() {
            return Ok(false);
        }
        let history = self.history.records().await.map_err(Refusal::Unreadable)?;
        let acks = self
            .history
            .acknowledgements()
            .await
            .map_err(Refusal::Unreadable)?;
        Ok(acknowledgement_needed(
            source,
            &history,
            self.held_real_data,
            &acks,
        ))
    }

    /// What the runtime and `pmos-admin status` are told: the gateway's source, whether a
    /// switch waits for its acknowledgement, and whether sessions may open now.
    pub async fn status(&self) -> SwitchStatus {
        let history = self.history.records().await.unwrap_or_default();
        let (gateway, report, refused) = match self.read().await {
            Ok(r) => (Some(r.source.clone()), Some(r), None),
            Err(why) => (None, None, Some(why.detail(self.own))),
        };
        let mut status = SwitchStatus {
            hosted_name: gateway.as_deref().and_then(hosted_name).map(String::from),
            max_data_class: report.as_ref().and_then(|r| r.max_data_class.clone()),
            fake_data: report.as_ref().is_some_and(GatewayReport::fake_data_only),
            gateway: gateway.clone(),
            acknowledgement_needed: false,
            ok: false,
            refused,
            held_real_data: self.held_real_data,
            history,
        };
        let Some(source) = gateway else {
            return status;
        };
        if !self.takes(&source) {
            status.refused = Some(Refusal::Changed(source).detail(self.own));
            return status;
        }
        match self.needs_acknowledgement(&source).await {
            Ok(true) => {
                status.acknowledgement_needed = true;
                status.refused = Some(Refusal::AcknowledgementNeeded(source).detail(self.own));
            }
            Ok(false) => status.ok = true,
            Err(why) => status.refused = Some(why.detail(self.own)),
        }
        status
    }

    /// Records an acknowledgement of the switch to `to`, which must be what the gateway
    /// reports now. `Ok`: the history it acknowledges (for the audit event).
    pub async fn acknowledge(
        &self,
        to: &str,
        by: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<SourceRecord>, String> {
        let current = self.read().await.map_err(|r| r.detail(self.own))?.source;
        if current != to {
            return Err(format!(
                "the model gateway reports `{current}`, not `{to}`; only the source it serves \
                 now can be acknowledged"
            ));
        }
        if hosted_name(to).is_none() || to == STACK_TEST_SOURCE {
            return Err("only a switch to an approved `hosted:<name>` is acknowledged".into());
        }
        if !self.takes(to) {
            return Err(format!(
                "this stack (PMOS_MODEL_SOURCE={}) doesn't serve `{to}`",
                self.own.as_str()
            ));
        }
        self.history
            .acknowledge(Acknowledgement {
                to: to.to_string(),
                by: by.to_string(),
                reason: reason.to_string(),
                at: now,
            })
            .await?;
        self.history.records().await
    }

    /// Records the source a session opened under, and that the database served it. `Ok`: the
    /// source served last before it, when it differs (a change, for `system.model_source`).
    pub async fn record(
        &self,
        token: &str,
        source: String,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, String> {
        let before = self.history.served(&source, now).await?;
        let mut sessions = self.sessions.lock().expect("sessions lock");
        let mut next = self.next.lock().expect("sequence lock");
        *next += 1;
        if sessions.len() >= MAX_SESSIONS {
            let mut seqs: Vec<u64> = sessions.values().map(|(_, n)| *n).collect();
            seqs.sort_unstable();
            let cut = seqs[seqs.len() / 2];
            sessions.retain(|_, (_, n)| *n > cut);
        }
        sessions.insert(key(token), (source.clone(), *next));
        Ok(before.filter(|b| *b != source))
    }

    /// At a tool call: the gateway still serves the source the session opened under. A
    /// session with no record (opened before a restart of this tool server) is refused too.
    pub async fn at_call(&self, token: &str) -> Result<(), Refusal> {
        let recorded = self
            .sessions
            .lock()
            .expect("sessions lock")
            .get(&key(token))
            .map(|(s, _)| s.clone());
        let current = self.read().await?.source;
        match recorded {
            Some(r) if r == current && self.takes(&current) => Ok(()),
            _ => Err(Refusal::Changed(current)),
        }
    }

    /// Forgets a closed session.
    pub fn forget(&self, token: &str) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(&key(token));
    }

    /// Whether this refusal is the first seen for its gateway source (so `system.model_source`
    /// is written once per change).
    pub fn first_seen(&self, refusal: &Refusal) -> bool {
        let seen = match refusal {
            Refusal::Changed(s) | Refusal::AcknowledgementNeeded(s) => s.clone(),
            Refusal::Unreadable(_) => "unreadable".to_string(),
        };
        let mut last = self.last_change.lock().expect("change lock");
        if last.as_deref() == Some(seen.as_str()) {
            return false;
        }
        *last = Some(seen);
        true
    }
}

fn key(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Refusal {
    /// What the audit log and the refusal say: the source the gateway serves (a known form,
    /// never a URL), or that it couldn't be read.
    pub fn detail(&self, own: ModelSource) -> String {
        match self {
            Refusal::Changed(s) => format!(
                "the model gateway serves `{s}`, not the source this session opened under \
                 (this stack: `{}`)",
                own.as_str()
            ),
            Refusal::AcknowledgementNeeded(s) => format!(
                "{ACKNOWLEDGEMENT_REQUIRED}: the model gateway serves `{s}`, and this database \
                 has served another real source or held real data; an administrator must \
                 confirm the model source switch (`sudo pmos-admin hosted acknowledge-switch \
                 --to {s} --reason \"...\"`)"
            ),
            Refusal::Unreadable(why) => format!("{why}; refused until it can be read"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000 + min * 60, 0).unwrap()
    }

    fn served(path: &[(&str, i64)]) -> Vec<SourceRecord> {
        let mut m: BTreeMap<String, SourceRecord> = BTreeMap::new();
        for (s, t) in path {
            m.entry(s.to_string())
                .and_modify(|r| r.last_at = at(*t))
                .or_insert(SourceRecord {
                    source: s.to_string(),
                    real: reported_is_real(s),
                    first_at: at(*t),
                    last_at: at(*t),
                });
        }
        m.into_values().collect()
    }

    fn ack(to: &str, t: i64) -> Acknowledgement {
        Acknowledgement {
            to: to.into(),
            by: "op".into(),
            reason: "test".into(),
            at: at(t),
        }
    }

    const TO: &str = "hosted:owner-hosted";

    /// X3: the history decides, not the last source; stub and hosted-test change nothing.
    #[test]
    fn the_acknowledgement_keys_on_the_whole_history() {
        // A fresh database goes straight to the entry: none needed.
        assert!(!acknowledgement_needed(TO, &[], false, &[]));
        // Only test sources before: none needed.
        let test_only = served(&[("stub", 1), ("hosted-test", 2)]);
        assert!(!acknowledgement_needed(TO, &test_only, false, &[]));
        // local -> stub -> hosted, and local -> hosted-test -> hosted: needed.
        for path in [
            vec![("local", 1), ("stub", 2)],
            vec![("local", 1), ("hosted-test", 2)],
            vec![("hosted:other-entry", 1), ("stub", 2)],
        ] {
            let h = served(&path);
            assert!(acknowledgement_needed(TO, &h, false, &[]), "{path:?}");
            // An acknowledgement made after them holds.
            assert!(!acknowledgement_needed(TO, &h, false, &[ack(TO, 3)]));
            // One for another entry doesn't.
            assert!(acknowledgement_needed(TO, &h, false, &[ack("hosted:x", 3)]));
        }
        // held_real_data and never this entry: needed; with this entry served before: not.
        assert!(acknowledgement_needed(TO, &[], true, &[]));
        assert!(!acknowledgement_needed(TO, &served(&[(TO, 1)]), true, &[]));
        // An acknowledgement is voided by a later real source.
        let h = served(&[("local", 1), (TO, 3), ("local", 5)]);
        assert!(acknowledgement_needed(TO, &h, false, &[ack(TO, 2)]));
        assert!(!acknowledgement_needed(
            TO,
            &h,
            false,
            &[ack(TO, 2), ack(TO, 6)]
        ));
        // Never for a source that isn't an approved entry.
        for other in ["local", "stub", "hosted-test", STACK_TEST_SOURCE] {
            assert!(!acknowledgement_needed(
                other,
                &served(&[("local", 1)]),
                true,
                &[]
            ));
        }
    }
}
