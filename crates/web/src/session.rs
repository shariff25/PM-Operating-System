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

/// The admin door's session cookie (slice H10, `docs/openshell-any-host-spec.md` §3.8): its
/// own name, `SameSite=Strict`, never `__Host-pmos`.
pub const ADMIN_COOKIE: &str = "__Host-pmos-admin";

/// Which front door of `pmos-web` a request came in by, and a session was made on (slice H10):
/// the browser-facing listener (`PMOS_WEB_BIND`, 8770) or the admin-only listener
/// (`PMOS_WEB_ADMIN_BIND`, 8772). Nothing made on one door works on the other: each has its own
/// cookie, and each accepts only its own sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Door {
    Public,
    Admin,
}

impl Door {
    /// As the `web_sessions.door` column stores it.
    pub fn as_str(self) -> &'static str {
        match self {
            Door::Public => "public",
            Door::Admin => "admin",
        }
    }

    /// The column's value; anything else is `None` (the column's check allows only the two).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Door::Public),
            "admin" => Some(Door::Admin),
            _ => None,
        }
    }

    /// This door's session cookie name.
    pub fn cookie_name(self) -> &'static str {
        match self {
            Door::Public => COOKIE,
            Door::Admin => ADMIN_COOKIE,
        }
    }

    /// The session id the browser sent in this door's cookie, if any.
    pub fn cookie_value(self, headers: &HeaderMap) -> Option<String> {
        named_cookie(headers, self.cookie_name())
    }

    /// `Set-Cookie` for a new session on this door: the public door's is `SameSite=Lax`
    /// (links from Slack and email open a signed-in page), the admin door's `SameSite=Strict`
    /// (r7, X6): no other site's link or form carries it.
    pub fn set_cookie(self, value: &str) -> String {
        format!(
            "{}={value}; Path=/; Secure; HttpOnly; SameSite={}",
            self.cookie_name(),
            self.same_site()
        )
    }

    /// `Set-Cookie` that removes this door's cookie.
    pub fn clear_cookie(self) -> String {
        format!(
            "{}=; Path=/; Secure; HttpOnly; SameSite={}; Max-Age=0",
            self.cookie_name(),
            self.same_site()
        )
    }

    fn same_site(self) -> &'static str {
        match self {
            Door::Public => "Lax",
            Door::Admin => "Strict",
        }
    }
}

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
    /// For an Entra ID sign-in, the token's `oid` (the person's object id in the tenant) and
    /// `tid` (the tenant): checked against a member's `entra-oid` on every request. `None`
    /// for development sign-in.
    pub entra_oid: Option<String>,
    pub entra_tid: Option<String>,
    /// For a password sign-in (`docs/password-sign-in.md`), the password credential's epoch it
    /// signed in with: checked against the user database on every request, so a reset or a
    /// password change ends it. `None` for every other sign-in.
    pub password_epoch: Option<i64>,
    /// The door the session was made on (slice H10). A row from before H10 reads as
    /// [`Door::Public`] (the column's default), so none can be used on the admin door.
    pub door: Door,
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

/// Where sessions live. [`MemorySessionStore`] loses them on restart (everyone signs in
/// again) and two `pmos-web` processes don't share them; [`crate::pg::PgStore`] keeps them in
/// `web_sessions` by the SHA-256 of the id only, enforces idle and absolute expiry in the
/// query, and deletes expired rows in [`SessionStore::purge`].
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn insert(&self, id: IdHash, session: Session) -> Result<(), StoreError>;

    /// The live session for `id` made on `door`, with `last_seen` moved to `now`. An expired
    /// one, or one made on the other door, is removed and reads as `None`: `idle` and `max` are
    /// that door's own bounds.
    async fn touch(
        &self,
        id: &IdHash,
        now: DateTime<Utc>,
        idle: Duration,
        max: Duration,
        door: Door,
    ) -> Result<Option<Session>, StoreError>;

    async fn remove(&self, id: &IdHash) -> Result<(), StoreError>;

    /// End every session of `handle` (they left the team).
    async fn remove_user(&self, handle: &str) -> Result<(), StoreError>;

    /// Does the store answer? For `GET /readyz`. Memory always does.
    async fn ping(&self) -> Result<(), StoreError> {
        Ok(())
    }

    /// Delete every expired session; the number removed. Called by the periodic purge
    /// (`app::Housekeeping`) and at sign-in. The in-memory store sweeps on every `touch` too.
    async fn purge(
        &self,
        _now: DateTime<Utc>,
        _idle: Duration,
        _max: Duration,
    ) -> Result<u64, StoreError> {
        Ok(0)
    }
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
        door: Door,
    ) -> Result<Option<Session>, StoreError> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        // Sweep this door's sessions while here, so the map doesn't grow with abandoned ones
        // (the other door's have bounds of their own).
        sessions.retain(|_, s| s.door != door || s.live(now, idle, max));
        if sessions.get(id).is_some_and(|s| s.door != door) {
            // Presented on the other door: never served there, and ended.
            sessions.remove(id);
            return Ok(None);
        }
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

/// The session's reference in a user assertion's `sid` claim (`docs/user-assertion.md`):
/// 16 hex characters of a SHA-256 over the session's stored hash, under a label of its own.
/// It tells one sign-in from another in an upstream's logs, and can't be turned back into the
/// session id, the cookie, or the key `web_sessions` stores the session under.
pub fn assertion_ref(id: &IdHash) -> String {
    let digest = Sha256::new()
        .chain_update(b"pmos-web assertion sid\0")
        .chain_update(id)
        .finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The public door's session id the browser sent in `Cookie`, if any.
pub fn cookie_value(headers: &HeaderMap) -> Option<String> {
    named_cookie(headers, COOKIE)
}

fn named_cookie(headers: &HeaderMap, wanted: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == wanted)
        .map(|(_, value)| value.trim().to_string())
        .filter(|v| v.len() == 43)
}

/// `Set-Cookie` for a new public-door session: `Secure`, `HttpOnly`, `SameSite=Lax`,
/// `Path=/`, no `Domain`, and no `Max-Age` (the server ends it). [`Door::set_cookie`] for the
/// admin door's.
pub fn set_cookie(value: &str) -> String {
    Door::Public.set_cookie(value)
}

/// `Set-Cookie` that removes the public door's cookie.
pub fn clear_cookie() -> String {
    Door::Public.clear_cookie()
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
            entra_oid: None,
            entra_tid: None,
            password_epoch: None,
            door: Door::Public,
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

    #[test]
    fn the_assertion_reference_is_short_stable_and_not_the_id() {
        let cookie = random_token();
        let id = hash_id(&cookie);
        let r = assertion_ref(&id);
        assert_eq!(r.len(), 16);
        assert!(r
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(r, assertion_ref(&hash_id(&cookie)));
        assert_ne!(r, assertion_ref(&hash_id(&random_token())));
        // Not the cookie, and not a prefix of the key the store keeps.
        let stored: String = id.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!stored.starts_with(&r) && !cookie.contains(&r));
        // A valid `sid`.
        assert!(pmos_assertion::Claims::new("web", "bots", "ana", &[], &r).is_ok());
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
                .touch(&id, at(step * 7 * 60), idle, max, Door::Public)
                .await
                .unwrap()
                .is_some());
        }
        // ...but never past 24 hours from sign-in.
        assert!(store
            .touch(&id, at(24 * 60), idle, max, Door::Public)
            .await
            .unwrap()
            .is_none());
        assert!(store.is_empty());

        let id = hash_id("two");
        store.insert(id, session("ana")).await.unwrap();
        assert!(store
            .touch(&id, at(8 * 60), idle, max, Door::Public)
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

    /// Slice H10 (X6): the admin door's cookie is its own, `SameSite=Strict`, and each door
    /// reads only its own cookie.
    #[test]
    fn each_door_has_its_own_cookie() {
        assert_eq!(
            Door::Admin.set_cookie("x"),
            "__Host-pmos-admin=x; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
        assert!(Door::Admin
            .clear_cookie()
            .ends_with("SameSite=Strict; Max-Age=0"));
        let (public, admin) = (random_token(), random_token());
        let mut h = HeaderMap::new();
        h.insert(
            "cookie",
            HeaderValue::from_str(&format!("__Host-pmos={public}; __Host-pmos-admin={admin}"))
                .unwrap(),
        );
        assert_eq!(Door::Public.cookie_value(&h), Some(public));
        assert_eq!(Door::Admin.cookie_value(&h), Some(admin.clone()));
        let mut only_public = HeaderMap::new();
        only_public.insert(
            "cookie",
            HeaderValue::from_str(&format!("__Host-pmos={admin}")).unwrap(),
        );
        assert_eq!(Door::Admin.cookie_value(&only_public), None);
        assert_eq!(Door::parse("admin"), Some(Door::Admin));
        assert_eq!(Door::parse("Admin"), None);
    }

    /// A session made on one door reads as `None` on the other, and is ended; sweeping one
    /// door's expired sessions leaves the other door's alone.
    #[tokio::test]
    async fn a_session_works_only_on_its_own_door() {
        let store = MemorySessionStore::new();
        let (idle, max) = (Duration::hours(8), Duration::hours(24));
        let public = hash_id("public");
        store.insert(public, session("ana")).await.unwrap();
        let admin = hash_id("admin");
        let mut s = session("ada");
        s.door = Door::Admin;
        store.insert(admin, s).await.unwrap();
        assert!(store
            .touch(&public, at(1), idle, max, Door::Admin)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store.len(),
            1,
            "the public session presented on the admin door ends"
        );
        // The admin door's shorter bounds sweep only admin sessions.
        let other = hash_id("other");
        store.insert(other, session("ben")).await.unwrap();
        let (a_idle, a_max) = (Duration::minutes(30), Duration::minutes(480));
        assert!(store
            .touch(&admin, at(60), a_idle, a_max, Door::Admin)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .touch(&other, at(60), idle, max, Door::Public)
            .await
            .unwrap()
            .is_some());
    }
}
