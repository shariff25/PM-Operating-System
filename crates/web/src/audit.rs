//! Security events, for the audit trail.
//!
//! An event carries ids, handles, role names and short reason codes only: never a password,
//! a session id or cookie, a CSRF token, a password hash, or text a person typed. A sign-in
//! failure for a handle that does not exist names no account at all, since the "handle" may
//! be a password typed into the wrong field.
//!
//! The field names follow the unified envelope proposed in brief 04 (§3.1) so the audit slice
//! can map them onto the shared sink. [`MemoryAuditSink`] is for tests and development;
//! [`TracingAuditSink`] writes one JSON log line per event under the `audit` target until
//! the real sink is wired in.

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pmos_contract::model::Role;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AuditAction {
    #[serde(rename = "auth.sign_in")]
    SignIn,
    #[serde(rename = "auth.failed")]
    SignInFailed,
    #[serde(rename = "auth.locked")]
    Lockout,
    #[serde(rename = "auth.sign_out")]
    SignOut,
    #[serde(rename = "auth.password_change")]
    PasswordChange,
    #[serde(rename = "auth.password_reset")]
    PasswordReset,
    #[serde(rename = "member.add")]
    UserCreated,
    #[serde(rename = "role.change")]
    RoleChange,
    #[serde(rename = "member.disable")]
    UserDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Ok,
    Denied,
}

/// Why something was denied. Codes, not prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    UnknownUser,
    WrongPassword,
    Locked,
    Disabled,
    LastAdmin,
    WeakPassword,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditActor {
    pub user_id: Uuid,
    pub handle: String,
    /// Roles at the time of the event.
    pub roles: Vec<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditTarget {
    /// Always `user` for now.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: Uuid,
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct AuditRequest {
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEvent {
    pub at: DateTime<Utc>,
    pub source: &'static str,
    pub channel: &'static str,
    pub action: AuditAction,
    pub outcome: AuditOutcome,
    pub actor: Option<AuditActor>,
    pub target: Option<AuditTarget>,
    pub reason_code: Option<ReasonCode>,
    /// The sign-in provider (`local`, later `entra`), for sign-in events.
    pub provider: Option<&'static str>,
    /// A non-secret reference to the session ([`crate::session::session_ref`]).
    pub session_ref: Option<String>,
    /// For `role.change`: the roles before and after.
    pub roles_before: Option<Vec<Role>>,
    pub roles_after: Option<Vec<Role>>,
    pub request: AuditRequest,
}

impl AuditEvent {
    pub fn new(at: DateTime<Utc>, action: AuditAction, outcome: AuditOutcome) -> Self {
        Self {
            at,
            source: "web",
            channel: "web",
            action,
            outcome,
            actor: None,
            target: None,
            reason_code: None,
            provider: None,
            session_ref: None,
            roles_before: None,
            roles_after: None,
            request: AuditRequest::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("audit sink failed: {0}")]
pub struct AuditError(pub String);

/// Where audit events go. A security-relevant change whose event can't be recorded is
/// reported to the caller as a failure (see [`crate::service`]).
#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError>;
}

/// Keeps every event in memory, for tests and `PMOS_DEV=1`.
#[derive(Debug, Default)]
pub struct MemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
    /// When set, every `record` fails: to test fail-closed behaviour.
    failing: Mutex<bool>,
}

impl MemoryAuditSink {
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn actions(&self) -> Vec<AuditAction> {
        self.events().into_iter().map(|e| e.action).collect()
    }

    pub fn set_failing(&self, failing: bool) {
        *self.failing.lock().unwrap_or_else(|e| e.into_inner()) = failing;
    }
}

#[async_trait]
impl AuditSink for MemoryAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        if *self.failing.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(AuditError("memory sink set to fail".into()));
        }
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
        Ok(())
    }
}

/// One JSON line per event in the service log, target `audit`. A stopgap until the shared
/// audit sink exists: it is not tamper-evident.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingAuditSink;

#[async_trait]
impl AuditSink for TracingAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        let line = serde_json::to_string(&event).map_err(|e| AuditError(e.to_string()))?;
        tracing::info!(target: "audit", event = %line, "audit");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_dotted_actions() {
        let e = AuditEvent::new(Utc::now(), AuditAction::SignInFailed, AuditOutcome::Denied);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["action"], "auth.failed");
        assert_eq!(v["outcome"], "denied");
        assert_eq!(v["source"], "web");
    }
}
