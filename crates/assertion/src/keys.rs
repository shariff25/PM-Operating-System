//! Keys, signing and verification.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::fields::{valid_name, Claims, RequestBinding};
use crate::replay::{ReplayGuard, ReplayRejection};
use crate::wire::{self, Assertion, Malformed, Nonce, MAX_LEN, MAX_LIFETIME_SECS, NONCE_LEN};

/// Shortest key, in bytes.
pub const MIN_KEY_LEN: usize = 32;
/// Longest key, in bytes (HMAC-SHA256 hashes anything longer than its 64-byte block anyway).
pub const MAX_KEY_LEN: usize = 64;
/// The clock skew a verifier allows unless told otherwise.
pub const DEFAULT_CLOCK_SKEW_SECS: u64 = 5;
/// The most clock skew a verifier may allow.
pub const MAX_CLOCK_SKEW_SECS: u64 = 30;

/// Domain separation: these keys sign nothing else, but if one were ever reused, a MAC made
/// here could not be mistaken for another protocol's.
const MAC_CONTEXT: &[u8] = b"pmos-assertion v1\0";

/// A key problem. Never includes key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeyError {
    #[error("a key must be {MIN_KEY_LEN} to {MAX_KEY_LEN} bytes")]
    Length,
    #[error("the key id is not a valid name")]
    KeyId,
    #[error("the key is not valid standard base64")]
    Encoding,
    #[error("the current and previous keys have the same id")]
    DuplicateId,
    #[error("the system random source failed")]
    Random,
}

/// One HMAC key and its id. `Debug` shows the id only; there is no way to read the secret
/// back out.
pub struct Key {
    id: String,
    secret: Box<[u8]>,
}

impl Key {
    /// A key from raw bytes (32 to 64).
    pub fn new(id: &str, secret: &[u8]) -> Result<Self, KeyError> {
        if !valid_name(id) {
            return Err(KeyError::KeyId);
        }
        if !(MIN_KEY_LEN..=MAX_KEY_LEN).contains(&secret.len()) {
            return Err(KeyError::Length);
        }
        Ok(Self {
            id: id.to_string(),
            secret: secret.into(),
        })
    }

    /// A key from standard base64 (as kept in a secret store or environment variable).
    pub fn from_base64(id: &str, encoded: &str) -> Result<Self, KeyError> {
        let bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|_| KeyError::Encoding)?;
        let key = Self::new(id, &bytes);
        drop_bytes(bytes);
        key
    }

    /// A fresh random 32-byte key.
    pub fn generate(id: &str) -> Result<Self, KeyError> {
        let mut bytes = [0u8; MIN_KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|_| KeyError::Random)?;
        let key = Self::new(id, &bytes);
        bytes.fill(0);
        key
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn mac(&self, signed: &str) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).expect("any key length");
        mac.update(MAC_CONTEXT);
        mac.update(signed.as_bytes());
        mac.finalize().into_bytes().into()
    }
}

fn drop_bytes(mut bytes: Vec<u8>) {
    bytes.fill(0);
}

impl Drop for Key {
    fn drop(&mut self) {
        // Best effort; the optimiser may elide it, and copies may exist elsewhere.
        self.secret.fill(0);
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Why signing failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SignError {
    #[error("the lifetime must be 1 to {MAX_LIFETIME_SECS} seconds")]
    Lifetime,
    #[error("the issue time is too large")]
    Clock,
    #[error("the system random source failed")]
    Random,
    #[error("the assertion would exceed {MAX_LEN} bytes")]
    TooLong,
}

/// Why verification failed. Map every variant to one generic `401` for the caller and log
/// the variant: the distinction is for operators, not for whoever sent the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("malformed assertion: {0}")]
    Malformed(Malformed),
    #[error("unknown key id")]
    UnknownKey,
    #[error("bad signature")]
    BadSignature,
    #[error("issuer not allowed")]
    UnknownIssuer,
    #[error("wrong audience")]
    WrongAudience,
    #[error("not yet valid")]
    NotYetValid,
    #[error("expired")]
    Expired,
    #[error("request does not match the binding")]
    RequestMismatch,
    #[error("replayed")]
    Replay,
    #[error("replay store unavailable")]
    ReplayStoreUnavailable,
}

impl From<Malformed> for VerifyError {
    fn from(m: Malformed) -> Self {
        VerifyError::Malformed(m)
    }
}

/// A verifier's settings problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PolicyError {
    #[error("the audience is not a valid name")]
    Audience,
    #[error("an issuer is not a valid name")]
    Issuer,
    #[error("no issuer is allowed")]
    NoIssuers,
    #[error("the clock skew may be at most {MAX_CLOCK_SKEW_SECS} seconds")]
    Skew,
}

/// What a verifying service accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyPolicy {
    audience: String,
    issuers: Vec<String>,
    skew: u64,
    require_binding: bool,
}

impl VerifyPolicy {
    /// Accept assertions for `audience` (this service's own name) from `issuers` (the
    /// client names allowed to act for people here). By default: 5 seconds of skew and a
    /// request binding required.
    pub fn new(audience: &str, issuers: &[&str]) -> Result<Self, PolicyError> {
        if !valid_name(audience) {
            return Err(PolicyError::Audience);
        }
        if issuers.is_empty() {
            return Err(PolicyError::NoIssuers);
        }
        if !issuers.iter().all(|i| valid_name(i)) {
            return Err(PolicyError::Issuer);
        }
        Ok(Self {
            audience: audience.to_string(),
            issuers: issuers.iter().map(|i| i.to_string()).collect(),
            skew: DEFAULT_CLOCK_SKEW_SECS,
            require_binding: true,
        })
    }

    /// Allow this much clock difference between signer and verifier (at most 30 seconds).
    pub fn with_clock_skew(mut self, secs: u64) -> Result<Self, PolicyError> {
        if secs > MAX_CLOCK_SKEW_SECS {
            return Err(PolicyError::Skew);
        }
        self.skew = secs;
        Ok(self)
    }

    /// Accept assertions without a request binding. An assertion that has one is still
    /// checked against the request given to `verify`.
    pub fn allow_unbound(mut self) -> Self {
        self.require_binding = false;
        self
    }

    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn issuers(&self) -> &[String] {
        &self.issuers
    }
    pub fn clock_skew(&self) -> u64 {
        self.skew
    }
    pub fn requires_binding(&self) -> bool {
        self.require_binding
    }
}

/// The current key, which signs, and optionally the previous one, which still verifies, so a
/// key can be rotated without downtime.
pub struct KeyRing {
    current: Key,
    previous: Option<Key>,
}

impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRing")
            .field("current", &self.current)
            .field("previous", &self.previous)
            .finish()
    }
}

impl KeyRing {
    pub fn new(current: Key) -> Self {
        Self {
            current,
            previous: None,
        }
    }

    /// Also verify with `previous` (its id must differ from the current key's).
    pub fn with_previous(mut self, previous: Key) -> Result<Self, KeyError> {
        if previous.id == self.current.id {
            return Err(KeyError::DuplicateId);
        }
        self.previous = Some(previous);
        Ok(self)
    }

    pub fn current_id(&self) -> &str {
        &self.current.id
    }

    pub fn previous_id(&self) -> Option<&str> {
        self.previous.as_ref().map(|k| k.id.as_str())
    }

    fn key(&self, id: &str) -> Option<&Key> {
        std::iter::once(&self.current)
            .chain(self.previous.as_ref())
            .find(|k| k.id == id)
    }

    /// Sign `claims` with the current key, issued at `now` (unix seconds) and valid for
    /// `ttl_secs` (1 to 120), bound to `request` when given. Returns the header value.
    pub fn sign(
        &self,
        claims: &Claims,
        now: u64,
        ttl_secs: u64,
        request: Option<&RequestBinding>,
    ) -> Result<String, SignError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| SignError::Random)?;
        self.sign_with_nonce(claims, now, ttl_secs, request, Nonce(nonce))
    }

    pub(crate) fn sign_with_nonce(
        &self,
        claims: &Claims,
        now: u64,
        ttl_secs: u64,
        request: Option<&RequestBinding>,
        nonce: Nonce,
    ) -> Result<String, SignError> {
        if ttl_secs == 0 || ttl_secs > MAX_LIFETIME_SECS {
            return Err(SignError::Lifetime);
        }
        let expires_at = now.checked_add(ttl_secs).ok_or(SignError::Clock)?;
        let assertion = Assertion {
            key_id: self.current.id.clone(),
            issuer: claims.issuer.clone(),
            audience: claims.audience.clone(),
            subject: claims.subject.clone(),
            roles: claims.roles.clone(),
            session: claims.session.clone(),
            issued_at: now,
            expires_at,
            nonce,
            request: request.map(|r| r.digest),
        };
        let signed = assertion.signed_part();
        let token = format!(
            "{signed};sig={}",
            URL_SAFE_NO_PAD.encode(self.current.mac(&signed))
        );
        if token.len() > MAX_LEN {
            return Err(SignError::TooLong);
        }
        Ok(token)
    }

    /// Verify a header value. Checks, in order: form, key, signature, issuer, audience,
    /// time, request binding, and last the replay guard (so only fully valid assertions are
    /// recorded). `now` is the verifier's clock in unix seconds; `request` is the request
    /// being served, or `None` if the caller cannot bind.
    pub fn verify(
        &self,
        token: &str,
        policy: &VerifyPolicy,
        now: u64,
        request: Option<&RequestBinding>,
        replay: &dyn ReplayGuard,
    ) -> Result<Assertion, VerifyError> {
        let parsed = wire::parse(token)?;
        let a = parsed.assertion;
        let key = self.key(&a.key_id).ok_or(VerifyError::UnknownKey)?;
        let expected = key.mac(parsed.signed);
        if !bool::from(expected.ct_eq(&parsed.signature)) {
            return Err(VerifyError::BadSignature);
        }
        if !policy.issuers.contains(&a.issuer) {
            return Err(VerifyError::UnknownIssuer);
        }
        if a.audience != policy.audience {
            return Err(VerifyError::WrongAudience);
        }
        if a.issued_at > now.saturating_add(policy.skew) {
            return Err(VerifyError::NotYetValid);
        }
        let retain_until = a.expires_at.saturating_add(policy.skew);
        if now >= retain_until {
            return Err(VerifyError::Expired);
        }
        match (&a.request, request) {
            (Some(bound), Some(req)) => {
                if !bool::from(bound.ct_eq(&req.digest)) {
                    return Err(VerifyError::RequestMismatch);
                }
            }
            (Some(_), None) => return Err(VerifyError::RequestMismatch),
            (None, _) if policy.require_binding => return Err(VerifyError::RequestMismatch),
            (None, _) => {}
        }
        replay
            .check_and_record(&a.nonce, retain_until, now)
            .map_err(|r| match r {
                ReplayRejection::Seen => VerifyError::Replay,
                ReplayRejection::Unavailable => VerifyError::ReplayStoreUnavailable,
            })?;
        Ok(a)
    }
}

/// The system clock in unix seconds, for callers that do not inject their own.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
