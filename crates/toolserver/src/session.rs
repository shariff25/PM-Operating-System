//! Sessions: one per agent sandbox. The orchestrator creates a session for a user, a skill
//! and a run; the sandbox presents its token on every MCP request.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    #[serde(skip_serializing)]
    pub token: String,
    pub user: String,
    pub skill: String,
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    pub fn create(
        &self,
        user: &str,
        skill: &str,
        run_id: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Session {
        let session = Session {
            token: new_token(),
            user: user.to_string(),
            skill: skill.to_string(),
            run_id: run_id.to_string(),
            created_at: now,
            expires_at: now + ttl,
        };
        let mut sessions = self.sessions.lock().expect("session lock");
        sessions.retain(|_, s| s.expires_at > now);
        sessions.insert(session.token.clone(), session.clone());
        session
    }

    /// The live session for `token`, if any.
    pub fn get(&self, token: &str, now: DateTime<Utc>) -> Option<Session> {
        let sessions = self.sessions.lock().expect("session lock");
        sessions.get(token).filter(|s| s.expires_at > now).cloned()
    }

    pub fn revoke(&self, token: &str) -> bool {
        self.sessions
            .lock()
            .expect("session lock")
            .remove(token)
            .is_some()
    }
}

/// 244 bits of randomness from two v4 UUIDs, prefixed so a leaked token is recognisable.
fn new_token() -> String {
    format!(
        "pmos_s_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A user handle: lowercase letters, digits, `.`, `_` and `-`, starting with a letter or digit.
/// Matches the harness's check, so a handle can never escape `master/people/`.
pub fn valid_handle(user: &str) -> bool {
    let mut chars = user.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        && user.len() <= 64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn sessions_expire() {
        let store = SessionStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let s = store.create("ana", "ask", "run-1", Duration::minutes(30), now);
        assert!(s.token.starts_with("pmos_s_"));
        assert!(store.get(&s.token, now + Duration::minutes(29)).is_some());
        assert!(store.get(&s.token, now + Duration::minutes(30)).is_none());
        assert!(store.get("pmos_s_unknown", now).is_none());
    }

    #[test]
    fn revoke_removes() {
        let store = SessionStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let s = store.create("ana", "ask", "run-1", Duration::minutes(30), now);
        assert!(store.revoke(&s.token));
        assert!(store.get(&s.token, now).is_none());
    }

    #[test]
    fn handles() {
        assert!(valid_handle("ana"));
        assert!(valid_handle("ana.k-2"));
        assert!(!valid_handle(""));
        assert!(!valid_handle("../etc"));
        assert!(!valid_handle("Ana"));
        assert!(!valid_handle(".ana"));
    }
}
