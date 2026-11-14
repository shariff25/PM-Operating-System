//! Sessions: one per agent sandbox. The orchestrator creates a session for a user, a run and
//! either a skill or a named bot; the sandbox presents its token on every MCP request.

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
    /// The skill a skill session runs. `None` for a bot session.
    pub skill: Option<String>,
    /// The bot a bot session runs as (`docs/bot-runtime-spec.md` §2). `None` for a skill
    /// session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Session {
    /// A new skill session with a fresh token.
    pub fn new(user: &str, skill: &str, run_id: &str, ttl: Duration, now: DateTime<Utc>) -> Self {
        Self {
            token: new_token(),
            user: user.to_string(),
            skill: Some(skill.to_string()),
            bot: None,
            run_id: run_id.to_string(),
            created_at: now,
            expires_at: now + ttl,
        }
    }

    /// A new bot session with a fresh token. `user` is the PM the task is for.
    pub fn for_bot(user: &str, bot: &str, run_id: &str, ttl: Duration, now: DateTime<Utc>) -> Self {
        Self {
            skill: None,
            bot: Some(bot.to_string()),
            ..Self::new(user, "", run_id, ttl, now)
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
/// The same rule as the harness and `team.yaml`, so a handle can never escape `master/people/`.
///
/// A handle under `bots.` is never a PM's: it is a team bot's service identity, accepted only
/// where a team bot's own accounts are linked and used ([`is_bot_identity`]).
pub fn valid_handle(user: &str) -> bool {
    pmos_contract::model::valid_handle(user) && !user.starts_with(BOT_IDENTITY_PREFIX)
}

/// Team bots read with their own accounts, linked under `bots.<name>`.
pub const BOT_IDENTITY_PREFIX: &str = "bots.";

/// The service identity a team bot reads as: `bots.<name>`.
pub fn bot_identity(bot: &str) -> String {
    format!("{BOT_IDENTITY_PREFIX}{bot}")
}

/// Is `handle` a team bot's service identity (`bots.<valid bot name>`)?
pub fn is_bot_identity(handle: &str) -> bool {
    handle
        .strip_prefix(BOT_IDENTITY_PREFIX)
        .is_some_and(pmos_contract::bots::valid_name)
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
        assert!(!valid_handle("bots.docs"), "a bot identity is never a PM");
        assert!(is_bot_identity("bots.docs"));
        assert_eq!(bot_identity("docs"), "bots.docs");
        for bad in [
            "bots.",
            "bots.Docs",
            "bots.a--b",
            "bots.docs/x",
            "docs",
            "ana",
        ] {
            assert!(!is_bot_identity(bad), "{bad}");
        }
    }

    #[test]
    fn bot_sessions_name_the_bot_not_a_skill() {
        let now: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        let s = Session::for_bot("ana", "docs", "tsk_1", Duration::minutes(30), now);
        assert_eq!(s.skill, None);
        assert_eq!(s.bot.as_deref(), Some("docs"));
        assert!(s.token.starts_with("pmos_s_"));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["bot"], "docs");
        assert!(json.get("token").is_none(), "never serialized");
    }
}
