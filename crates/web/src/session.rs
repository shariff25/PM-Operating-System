//! Sessions: who is signed in, behind the `__Host-pmos` cookie.
//!
//! - **Ids** are 256 random bits from the operating system, sent base64url in the cookie.
//!   The store keeps only their SHA-256, so a copy of the store can't be replayed as cookies.
//! - **Expiry:** idle (no request for `idle`, default 8 hours) and absolute (`max` after
//!   sign-in, default 24 hours), whichever comes first.
//! - **CSRF:** each session has its own random 256-bit token, which `GET /api/me` returns
//!   and every write sends as `X-Pmos-Csrf`. It is compared in constant time.
//! - **Rotation:** signing in always makes a new id and ends any session the browser
//!   already had.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use axum::http::HeaderMap;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// The cookie's name. `__Host-` makes the browser insist on `Secure`, `Path=/` and no
/// `Domain`, so a sibling subdomain can't set or read it.
pub const COOKIE: &str = "__Host-pmos";

/// The header writes carry the CSRF token in.
pub const CSRF_HEADER: &str = "x-pmos-csrf";

/// A stored session's key: the SHA-256 of the cookie value.
pub type IdHash = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub handle: String,
    pub csrf_token: String,
    pub created_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Session {
    fn live(&self, now: DateTime<Utc>, idle: Duration, max: Duration) -> bool {
        now < self.last_seen + idle && now < self.created_at + max
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where sessions live. [`MemorySessionStore`] is the only one today: a restart signs
/// everyone out, and two `pmos-web` processes don't share sessions. A Postgres store would
/// need a `web_sessions (id_hash bytea primary key, handle, csrf_token, created_at,
/// last_seen)` table, an index on `handle`, and a periodic delete of expired rows.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn insert(&self, id: IdHash, session: Session) -> Result<(), StoreError>;

    /// The live session for `id`, with `last_seen` moved to `now`. An expired one is removed
    /// and reads as `None`.
    async fn touch(
        &self,
        id: &IdHash,
        now: DateTime<Utc>,
        idle: Duration,
        max: Duration,
    ) -> Result<Option<Session>, StoreError>;

    async fn remove(&self, id: &IdHash) -> Result<(), StoreError>;

    /// End every session of `handle` (they left the team).
    async fn remove_user(&self, handle: &str) -> Result<(), StoreError>;
}

#[derive(Default)]
pub struct MemorySessionStore {
    sessions: Mutex<HashMap<IdHash, Session>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().expect("sessions lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn insert(&self, id: IdHash, session: Session) -> Result<(), StoreError> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(id, session);
        Ok(())
    }

    async fn touch(
        &self,
        id: &IdHash,
        now: DateTime<Utc>,
        idle: Duration,
        max: Duration,
    ) -> Result<Option<Session>, StoreError> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        // Sweep while here, so the map doesn't grow with abandoned sessions.
        sessions.retain(|_, s| s.live(now, idle, max));
        Ok(sessions.get_mut(id).map(|s| {
            s.last_seen = now;
            s.clone()
        }))
    }

    async fn remove(&self, id: &IdHash) -> Result<(), StoreError> {
        self.sessions.lock().expect("sessions lock").remove(id);
        Ok(())
    }

    async fn remove_user(&self, handle: &str) -> Result<(), StoreError> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .retain(|_, s| s.handle != handle);
        Ok(())
    }
}

/// 256 random bits, base64url without padding (43 characters).
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system's random number generator");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_id(cookie_value: &str) -> IdHash {
    Sha256::digest(cookie_value.as_bytes()).into()
}

/// The session id the browser sent in `Cookie`, if any.
pub fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.trim().to_string())
        .filter(|v| v.len() == 43)
}

/// `Set-Cookie` for a new session: `Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`, no
/// `Domain`, and no `Max-Age` (the server ends it).
pub fn set_cookie(value: &str) -> String {
    format!("{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax")
}

/// `Set-Cookie` that removes the cookie.
pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-16T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    fn session(handle: &str) -> Session {
        Session {
            handle: handle.into(),
            csrf_token: random_token(),
            created_at: at(0),
            last_seen: at(0),
        }
    }

    #[test]
    fn tokens_are_random_and_256_bits() {
        let a = random_token();
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_token());
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a)
            .unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[tokio::test]
    async fn sessions_expire_when_idle_and_absolutely() {
        let store = MemorySessionStore::new();
        let (idle, max) = (Duration::hours(8), Duration::hours(24));
        let id = hash_id("one");
        store.insert(id, session("ana")).await.unwrap();
        // Kept alive by use, every 7 hours...
        for step in 1..=3 {
            assert!(store
                .touch(&id, at(step * 7 * 60), idle, max)
                .await
                .unwrap()
                .is_some());
        }
        // ...but never past 24 hours from sign-in.
        assert!(store
            .touch(&id, at(24 * 60), idle, max)
            .await
            .unwrap()
            .is_none());
        assert!(store.is_empty());

        let id = hash_id("two");
        store.insert(id, session("ana")).await.unwrap();
        assert!(store
            .touch(&id, at(8 * 60), idle, max)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn removes_a_users_sessions() {
        let store = MemorySessionStore::new();
        store.insert(hash_id("a"), session("ana")).await.unwrap();
        store.insert(hash_id("b"), session("ben")).await.unwrap();
        store.remove_user("ana").await.unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn reads_the_cookie() {
        let value = random_token();
        let mut h = HeaderMap::new();
        h.append(
            "cookie",
            HeaderValue::from_str("theme=dark; other=1").unwrap(),
        );
        h.append(
            "cookie",
            HeaderValue::from_str(&format!("{COOKIE}={value}")).unwrap(),
        );
        assert_eq!(cookie_value(&h), Some(value));
        let mut bad = HeaderMap::new();
        bad.insert("cookie", HeaderValue::from_static("__Host-pmos=short"));
        assert_eq!(cookie_value(&bad), None);
        assert_eq!(
            set_cookie("x"),
            "__Host-pmos=x; Path=/; Secure; HttpOnly; SameSite=Lax"
        );
    }
}
