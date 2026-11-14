//! Sessions: one per agent sandbox. The orchestrator creates a session for a user, a skill
//! and a run; the sandbox presents its token on every MCP request.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::store::StoreResult;

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

impl Session {
    /// A new session with a fresh token.
    pub fn new(user: &str, skill: &str, run_id: &str, ttl: Duration, now: DateTime<Utc>) -> Self {
        Self {
            token: new_token(),
            user: user.to_string(),
            skill: skill.to_string(),
            run_id: run_id.to_string(),
            created_at: now,
            expires_at: now + ttl,
        }
    }
}

#[async_trait]
pub trait SessionRepo: Send + Sync {
    /// Store a session made with [`Session::new`].
    async fn insert(&self, session: &Session, now: DateTime<Utc>) -> StoreResult<()>;
    /// The live session for `token`, if any.
    async fn get(&self, token: &str, now: DateTime<Utc>) -> StoreResult<Option<Session>>;
    /// End a session. Returns whether it existed.
    async fn revoke(&self, token: &str) -> StoreResult<bool>;
}

#[derive(Default)]
pub struct MemorySessions {
    sessions: Mutex<HashMap<String, Session>>,
}

#[async_trait]
impl SessionRepo for MemorySessions {
    async fn insert(&self, session: &Session, now: DateTime<Utc>) -> StoreResult<()> {
        let mut sessions = self.sessions.lock().expect("session lock");
        sessions.retain(|_, s| s.expires_at > now);
        sessions.insert(session.token.clone(), session.clone());
        Ok(())
    }

    async fn get(&self, token: &str, now: DateTime<Utc>) -> StoreResult<Option<Session>> {
        let sessions = self.sessions.lock().expect("session lock");
        Ok(sessions.get(token).filter(|s| s.expires_at > now).cloned())
    }

    async fn revoke(&self, token: &str) -> StoreResult<bool> {
        Ok(self
            .sessions
            .lock()
            .expect("session lock")
            .remove(token)
            .is_some())
    }
}

/// SHA-256 of a token, hex-encoded. Stores keep only this, so a database dump holds no
/// usable session tokens.
pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

    #[test]
    fn tokens_are_fresh_and_hashed() {
        let now: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        let a = Session::new("ana", "ask", "r1", Duration::minutes(30), now);
        let b = Session::new("ana", "ask", "r1", Duration::minutes(30), now);
        assert!(a.token.starts_with("pmos_s_"));
        assert_ne!(a.token, b.token);
        assert_eq!(token_hash(&a.token).len(), 64);
        assert_ne!(token_hash(&a.token), a.token);
        assert_eq!(a.expires_at, now + Duration::minutes(30));
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
