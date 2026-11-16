//! Passwords: Argon2id hashing, the strength policy, and lockout with backoff.
//!
//! - **Hashing:** Argon2id (RFC 9106) with the OWASP minimum by default (19 MiB, 2 passes,
//!   1 lane), a 16-byte random salt, stored as a PHC string so the parameters travel with the
//!   hash and can be raised later: a hash made with older parameters is replaced at the next
//!   successful sign-in. Verification compares in constant time (the `argon2` crate's
//!   `verify_password`). Hashing is CPU and memory heavy, so it runs off the async threads and
//!   at most [`HASH_CONCURRENCY`] at a time.
//! - **No user enumeration by timing:** an unknown handle is verified against a dummy hash
//!   made at start-up with the same parameters, so it costs the same as a wrong password.
//! - **Policy:** 12 to 128 characters, not a common password (also with digits or symbols
//!   tacked on), not containing the handle, at least 5 different characters. No composition
//!   rules (NIST SP 800-63B).

use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio::sync::Semaphore;

/// Concurrent hash computations. With 19 MiB each this bounds memory at about 80 MiB and
/// stops a flood of sign-ins from starving the rest of the server.
pub const HASH_CONCURRENCY: usize = 4;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl HashParams {
    /// OWASP Password Storage Cheat Sheet minimum for Argon2id.
    pub const PRODUCTION: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
        lanes: 1,
    };

    /// Cheap parameters for tests only: still Argon2id, still salted.
    pub const TEST: Self = Self {
        memory_kib: 1024,
        iterations: 1,
        lanes: 1,
    };
}

#[derive(Debug, thiserror::Error)]
#[error("password hashing failed: {0}")]
pub struct HashError(String);

/// Hashes and verifies passwords. Cheap to clone.
#[derive(Clone)]
pub struct Hasher {
    argon: Argon2<'static>,
    params: HashParams,
    /// The hash an unknown handle is checked against, so it takes as long as a known one.
    dummy: Arc<str>,
    permits: Arc<Semaphore>,
}

impl std::fmt::Debug for Hasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hasher").field("params", &self.params).finish()
    }
}

impl Hasher {
    pub fn new(params: HashParams) -> Result<Self, HashError> {
        let p = Params::new(params.memory_kib, params.iterations, params.lanes, None)
            .map_err(|e| HashError(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
        let dummy_password = crate::random::temporary_password();
        let dummy = hash_with(&argon, &dummy_password)?;
        Ok(Self {
            argon,
            params,
            dummy: dummy.into(),
            permits: Arc::new(Semaphore::new(HASH_CONCURRENCY)),
        })
    }

    pub fn params(&self) -> HashParams {
        self.params
    }

    /// A new PHC string for `password`, blocking. Prefer [`Hasher::hash`].
    pub fn hash_blocking(&self, password: &str) -> Result<String, HashError> {
        hash_with(&self.argon, password)
    }

    /// Whether `password` matches `stored`, blocking. A malformed `stored` never matches.
    pub fn verify_blocking(&self, password: &str, stored: &str) -> bool {
        match PasswordHash::new(stored) {
            Ok(parsed) => self
                .argon
                .verify_password(password.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => {
                // Spend the same time anyway: a corrupt row must not be distinguishable.
                self.burn(password);
                false
            }
        }
    }

    /// Verify against the dummy hash: what an unknown handle costs. Always false.
    pub fn burn(&self, password: &str) {
        if let Ok(parsed) = PasswordHash::new(&self.dummy) {
            let _ = self.argon.verify_password(password.as_bytes(), &parsed);
        }
    }

    /// Whether `stored` was made with other parameters (or another algorithm) and should be
    /// replaced after the next successful sign-in.
    pub fn needs_rehash(&self, stored: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(stored) else {
            return true;
        };
        if parsed.algorithm.as_str() != "argon2id" {
            return true;
        }
        match Params::try_from(&parsed) {
            Ok(p) => {
                p.m_cost() != self.params.memory_kib
                    || p.t_cost() != self.params.iterations
                    || p.p_cost() != self.params.lanes
            }
            Err(_) => true,
        }
    }

    pub async fn hash(&self, password: &str) -> Result<String, HashError> {
        let me = self.clone();
        let password = password.to_string();
        self.run(move || me.hash_blocking(&password)).await?
    }

    /// Verify `password` against `stored`, or against the dummy hash when `stored` is `None`
    /// (an unknown handle). Same cost either way.
    pub async fn verify(&self, password: &str, stored: Option<&str>) -> Result<bool, HashError> {
        let me = self.clone();
        let password = password.to_string();
        let stored = stored.map(str::to_string);
        self.run(move || match stored {
            Some(s) => me.verify_blocking(&password, &s),
            None => {
                me.burn(&password);
                false
            }
        })
        .await
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, HashError> {
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| HashError(e.to_string()))?;
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| HashError(e.to_string()))
    }
}

fn hash_with(argon: &Argon2<'static>, password: &str) -> Result<String, HashError> {
    let salt = SaltString::encode_b64(&crate::random::bytes::<16>())
        .map_err(|e| HashError(e.to_string()))?;
    argon
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| HashError(e.to_string()))
}

// ----- Policy ------------------------------------------------------------------------------

/// Why a new password was refused. Shown to the person choosing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Weakness {
    TooShort,
    TooLong,
    Common,
    ContainsHandle,
    TooFewDistinctCharacters,
    SameAsCurrent,
}

#[derive(Debug, Clone, Copy)]
pub struct PasswordPolicy {
    pub min_chars: usize,
    pub max_chars: usize,
    pub min_distinct: usize,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_chars: 12,
            max_chars: 128,
            min_distinct: 5,
        }
    }
}

/// Passwords nobody should use, lowercase. Checked whole and with leading or trailing digits
/// and symbols removed ("Password123!" is "password").
const DENY: &[&str] = &[
    "password", "passw0rd", "p@ssword", "p@ssw0rd", "passphrase", "qwerty", "qwertyuiop",
    "qwertyuiopasdf", "asdfghjkl", "zxcvbnm", "azerty", "letmein", "welcome", "welcometo",
    "iloveyou", "admin", "administrator", "root", "changeme", "changeit", "default", "secret",
    "monkey", "dragon", "football", "baseball", "sunshine", "princess", "shadow", "master",
    "superman", "batman", "trustno", "whatever", "starwars", "login", "abc", "abcdef",
    "abcdefgh", "abcdefghijkl", "abcdefghijklmnop", "test", "testing", "temp", "guest", "user",
    "pmos", "pmoperatingsystem", "productmanager", "product", "manager", "spring", "summer",
    "autumn", "winter", "january", "december", "company", "office", "microsoft", "azure",
    "entra", "slack", "atlassian", "jira", "confluence",
];

/// The part of a password left after stripping leading and trailing digits and symbols.
fn core(lower: &str) -> &str {
    lower.trim_matches(|c: char| !c.is_alphabetic())
}

/// Ascending or descending runs of digits, like `123456789012` or `987654321098`.
fn is_digit_run(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 4 || !b.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let up = b.windows(2).all(|w| (w[0] - b'0' + 1) % 10 == w[1] - b'0');
    let down = b.windows(2).all(|w| (w[1] - b'0' + 1) % 10 == w[0] - b'0');
    up || down
}

impl PasswordPolicy {
    /// Everything wrong with `password` for the account `handle`; empty when it is fine.
    pub fn check(&self, password: &str, handle: &str) -> Vec<Weakness> {
        let mut out = Vec::new();
        let chars = password.chars().count();
        if chars < self.min_chars {
            out.push(Weakness::TooShort);
        }
        if chars > self.max_chars {
            out.push(Weakness::TooLong);
        }
        let lower = password.to_lowercase();
        let stripped = core(&lower);
        let common = DENY.contains(&lower.as_str())
            || DENY.contains(&stripped)
            || (stripped.is_empty() && is_digit_run(&lower))
            // The same short word over and over: "passwordpassword".
            || DENY.iter().any(|w| {
                !stripped.is_empty()
                    && stripped.len() % w.len() == 0
                    && stripped.as_bytes().chunks(w.len()).all(|c| c == w.as_bytes())
            });
        if common {
            out.push(Weakness::Common);
        }
        if handle.len() >= 3 && lower.contains(&handle.to_lowercase()) {
            out.push(Weakness::ContainsHandle);
        }
        let mut distinct: Vec<char> = password.chars().collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() < self.min_distinct {
            out.push(Weakness::TooFewDistinctCharacters);
        }
        out
    }
}

// ----- Lockout -----------------------------------------------------------------------------

/// Per-account lockout. From the `threshold`-th consecutive wrong password on, each further
/// wrong password locks the account, for `base`, then twice as long each time, up to `max`.
/// Attempts while locked are refused without being checked or counted (so they can't extend
/// the lock), and a successful sign-in resets the count.
#[derive(Debug, Clone, Copy)]
pub struct LockoutPolicy {
    pub threshold: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self {
            threshold: 5,
            base: Duration::minutes(1),
            max: Duration::hours(1),
        }
    }
}

impl LockoutPolicy {
    /// The lock after the `failed`-th consecutive failure at `at`, if any.
    pub fn lock_after(&self, failed: u32, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.threshold == 0 || failed < self.threshold {
            return None;
        }
        let doublings = (failed - self.threshold).min(30);
        let lock = self
            .base
            .checked_mul(1i32 << doublings.min(20))
            .unwrap_or(self.max)
            .min(self.max);
        Some(at + lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_argon2id_salted_and_verify() {
        let h = Hasher::new(HashParams::TEST).unwrap();
        let a = h.hash_blocking("correct horse battery").unwrap();
        let b = h.hash_blocking("correct horse battery").unwrap();
        assert!(a.starts_with("$argon2id$v=19$m=1024,t=1,p=1$"), "{a}");
        assert_ne!(a, b, "a fresh salt each time");
        assert!(h.verify_blocking("correct horse battery", &a));
        assert!(!h.verify_blocking("correct horse batterz", &a));
        assert!(!h.verify_blocking("", &a));
        assert!(!h.verify_blocking("correct horse battery", "not a hash"));
        assert!(!h.needs_rehash(&a));
    }

    #[test]
    fn production_parameters_are_owasp_minimum() {
        let p = HashParams::PRODUCTION;
        assert_eq!((p.memory_kib, p.iterations, p.lanes), (19456, 2, 1));
    }

    #[test]
    fn hashes_with_other_parameters_need_rehash() {
        let old = Hasher::new(HashParams::TEST).unwrap();
        let new = Hasher::new(HashParams {
            memory_kib: 2048,
            ..HashParams::TEST
        })
        .unwrap();
        let stored = old.hash_blocking("correct horse battery").unwrap();
        assert!(new.needs_rehash(&stored));
        assert!(new.verify_blocking("correct horse battery", &stored));
        assert!(new.needs_rehash("$2b$12$bcrypt-looking"));
    }

    #[tokio::test]
    async fn async_verify_of_unknown_is_false() {
        let h = Hasher::new(HashParams::TEST).unwrap();
        assert!(!h.verify("anything at all", None).await.unwrap());
        let stored = h.hash("long enough passphrase").await.unwrap();
        assert!(h.verify("long enough passphrase", Some(&stored)).await.unwrap());
    }

    #[test]
    fn policy() {
        let p = PasswordPolicy::default();
        assert!(p.check("violet-kettle-orbit-42", "ana").is_empty());
        assert_eq!(p.check("short1!", "ana"), vec![Weakness::TooShort]);
        assert!(p.check(&"ab1!".repeat(40), "ana").contains(&Weakness::TooLong));
        for common in [
            "password1234",
            "Password123!",
            "!!Qwertyuiop!!",
            "123456789012",
            "987654321098",
            "passwordpassword",
            "Welcome2026!!",
            "changeme-2026",
        ] {
            assert!(
                p.check(common, "ana").contains(&Weakness::Common),
                "{common} should be common"
            );
        }
        assert!(p
            .check("ana.lima-likes-tea", "ana.lima")
            .contains(&Weakness::ContainsHandle));
        assert!(p
            .check("aaaaaaaaaaaabbbb", "ana")
            .contains(&Weakness::TooFewDistinctCharacters));
        // Length is in characters, not bytes.
        assert!(p.check("ñandú-ölçü-ß", "ana").is_empty());
    }

    #[test]
    fn lockout_backs_off_and_caps() {
        let p = LockoutPolicy::default();
        let t = Utc::now();
        assert_eq!(p.lock_after(4, t), None);
        assert_eq!(p.lock_after(5, t), Some(t + Duration::minutes(1)));
        assert_eq!(p.lock_after(6, t), Some(t + Duration::minutes(2)));
        assert_eq!(p.lock_after(8, t), Some(t + Duration::minutes(8)));
        assert_eq!(p.lock_after(12, t), Some(t + Duration::hours(1)));
        assert_eq!(p.lock_after(u32::MAX, t), Some(t + Duration::hours(1)));
    }
}
