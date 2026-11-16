//! Session ids, the cookie that carries them, and the CSRF token bound to each.
//!
//! - **Id:** 32 random bytes (256 bits) from the OS, base64url in the cookie. The server
//!   keeps only its SHA-256, so a copy of the sessions table can't be replayed as cookies.
//! - **Cookie:** `__Host-pmos_sid`, `Secure; HttpOnly; SameSite=Lax; Path=/`, no `Domain`.
//!   The `__Host-` prefix makes browsers refuse it unless it was set that way by this exact
//!   host over HTTPS, so a sibling subdomain can't plant or overwrite it.
//! - **CSRF:** `HMAC-SHA256(key = session id, "pmos-web/csrf/v1")`, sent back in the
//!   `X-CSRF-Token` header on every state-changing call. Only someone who can read the cookie
//!   (which script can't, being `HttpOnly`) or a response body from this origin can know it.
//!   It changes whenever the session id does, it is unrelated to the stored hash, and it
//!   needs no server key, so restarts and replicas agree.

use axum::http::{header, HeaderMap};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const COOKIE_NAME: &str = "__Host-pmos_sid";
pub const CSRF_HEADER: &str = "x-csrf-token";
const CSRF_LABEL: &[u8] = b"pmos-web/csrf/v1";
/// 32 bytes in unpadded base64url.
const TOKEN_LEN: usize = 43;

/// The raw session id, as the cookie carries it. Never logged or stored.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

impl SessionToken {
    pub fn generate() -> Self {
        Self(URL_SAFE_NO_PAD.encode(crate::random::bytes::<32>()))
    }

    /// A token from a cookie value, if it has the shape this server makes.
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == TOKEN_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .then(|| Self(value.to_string()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// What the store keys the session by: hex SHA-256 of the id.
    pub fn hash(&self) -> String {
        hex(&Sha256::digest(self.0.as_bytes()))
    }

    /// The CSRF token for this session.
    pub fn csrf(&self) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.0.as_bytes()).expect("HMAC takes any key length");
        mac.update(CSRF_LABEL);
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// Whether `presented` is this session's CSRF token, compared in constant time.
    pub fn csrf_matches(&self, presented: &str) -> bool {
        pmos_runtime::token_eq(presented, &self.csrf())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A short, non-secret reference to a session for audit and logs: the first 16 hex digits
/// of the stored hash. It can't be turned back into the cookie.
pub fn session_ref(id_hash: &str) -> String {
    id_hash.chars().take(16).collect()
}

/// How long sessions last.
#[derive(Debug, Clone, Copy)]
pub struct SessionPolicy {
    /// Signed out after this long with no request.
    pub idle: Duration,
    /// Signed out this long after signing in, however active.
    pub absolute: Duration,
    /// `last_seen_at` is written at most this often, to spare the database.
    pub touch_every: Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            idle: Duration::minutes(60),
            absolute: Duration::hours(10),
            touch_every: Duration::seconds(60),
        }
    }
}

/// A session as stored. Keyed by the hash of its id, never the id itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id_hash: String,
    pub user_id: Uuid,
    /// The [`crate::provider::AuthProvider`] that signed the person in (`local`, later
    /// `entra`).
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// The absolute expiry.
    pub expires_at: DateTime<Utc>,
}

impl Session {
    pub fn is_live(&self, now: DateTime<Utc>, policy: &SessionPolicy) -> bool {
        now < self.expires_at && now - self.last_seen_at < policy.idle
    }
}

/// The session token in a request's `Cookie` headers. Only the exact `__Host-` name counts.
pub fn token_from_headers(headers: &HeaderMap) -> Option<SessionToken> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .and_then(|(_, value)| SessionToken::parse(value.trim()))
}

/// `Set-Cookie` for a new session. It lives no longer than the session's absolute expiry.
pub fn set_cookie(token: &SessionToken, policy: &SessionPolicy) -> String {
    format!(
        "{COOKIE_NAME}={}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Lax",
        token.expose(),
        policy.absolute.num_seconds()
    )
}

/// `Set-Cookie` that removes the session cookie.
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn tokens_are_256_bits_and_hashed() {
        let t = SessionToken::generate();
        assert_eq!(t.expose().len(), TOKEN_LEN);
        assert_eq!(URL_SAFE_NO_PAD.decode(t.expose()).unwrap().len(), 32);
        assert_eq!(t.hash().len(), 64);
        assert_ne!(t.hash(), t.expose());
        assert_ne!(SessionToken::generate(), t);
        assert!(!format!("{t:?}").contains(t.expose()));
    }

    #[test]
    fn csrf_is_bound_to_the_session() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert!(a.csrf_matches(&a.csrf()));
        assert!(!a.csrf_matches(&b.csrf()));
        assert!(!a.csrf_matches(""));
        assert_ne!(a.csrf(), a.hash());
    }

    #[test]
    fn cookie_attributes() {
        let t = SessionToken::generate();
        let c = set_cookie(&t, &SessionPolicy::default());
        assert!(c.starts_with("__Host-pmos_sid="));
        for attr in ["Path=/", "Secure", "HttpOnly", "SameSite=Lax", "Max-Age=36000"] {
            assert!(c.contains(attr), "{c} lacks {attr}");
        }
        assert!(!c.contains("Domain"));
        assert!(clear_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn reads_only_the_host_cookie() {
        let t = SessionToken::generate();
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("pmos_sid=evil; {COOKIE_NAME}={}", t.expose()))
                .unwrap(),
        );
        assert_eq!(token_from_headers(&h), Some(t));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("pmos_sid=x; other=y"));
        assert_eq!(token_from_headers(&h), None);
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-pmos_sid=too-short"),
        );
        assert_eq!(token_from_headers(&h), None);
    }

    #[test]
    fn idle_and_absolute_expiry() {
        let p = SessionPolicy::default();
        let t = Utc::now();
        let s = Session {
            id_hash: "h".into(),
            user_id: Uuid::new_v4(),
            provider: "local".into(),
            created_at: t,
            last_seen_at: t,
            expires_at: t + p.absolute,
        };
        assert!(s.is_live(t + Duration::minutes(59), &p));
        assert!(!s.is_live(t + Duration::minutes(60), &p));
        let busy = Session {
            last_seen_at: t + Duration::hours(10) - Duration::minutes(1),
            ..s.clone()
        };
        assert!(busy.is_live(t + Duration::hours(10) - Duration::seconds(1), &p));
        assert!(!busy.is_live(t + Duration::hours(10), &p));
    }
}
