//! The tool server's cross-check of the model gateway (slice H4,
//! `docs/openshell-any-host-spec.md` §3.6, "The runtime and the tool server cross-check the
//! gateway" and "Keeping every caller on one source").
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

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use pmos_contract::model_source::ModelSource;
use pmos_runtime::model_source::GatewaySource;

/// The code of a refusal for a changed (or unreadable) model source.
pub const MODEL_SOURCE_CHANGED: &str = "model_source_changed";

/// The audit action recorded once when the gateway's source is seen to change.
pub const SYSTEM_MODEL_SOURCE: &str = "system.model_source";

/// The audit action recorded when a session is closed for it.
pub const SESSION_CLOSED: &str = "session.closed";

/// The most sessions whose source is kept; past it, the oldest half is forgotten (their next
/// call is refused, which only closes them early).
const MAX_SESSIONS: usize = 100_000;

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
        }
    }

    /// The same, reusing a reading for `ttl` (tests).
    pub fn with_ttl(own: ModelSource, gateway_url: &str, ttl: Duration) -> Self {
        Self::with_gateway(own, GatewaySource::new(gateway_url).with_ttl(ttl))
    }

    pub fn own(&self) -> ModelSource {
        self.own
    }

    /// At a session's open: the source the gateway serves, if this stack may use it.
    pub async fn at_open(&self) -> Result<String, Refusal> {
        match self.gateway.current().await {
            Ok(s) if self.own.accepts(&s) => Ok(s),
            Ok(s) => Err(Refusal::Changed(s)),
            Err(why) => Err(Refusal::Unreadable(why)),
        }
    }

    /// Records the source a session opened under.
    pub fn record(&self, token: &str, source: String) {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        let mut next = self.next.lock().expect("sequence lock");
        *next += 1;
        if sessions.len() >= MAX_SESSIONS {
            let mut seqs: Vec<u64> = sessions.values().map(|(_, n)| *n).collect();
            seqs.sort_unstable();
            let cut = seqs[seqs.len() / 2];
            sessions.retain(|_, (_, n)| *n > cut);
        }
        sessions.insert(key(token), (source, *next));
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
        let current = match self.gateway.current().await {
            Ok(s) => s,
            Err(why) => return Err(Refusal::Unreadable(why)),
        };
        match recorded {
            Some(r) if r == current && self.own.accepts(&current) => Ok(()),
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
            Refusal::Changed(s) => s.clone(),
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
            Refusal::Unreadable(why) => format!("{why}; refused until it can be read"),
        }
    }
}
