//! Passwords: the rules a new one must meet, and Argon2id hashing.
//!
//! - **Argon2id** with OWASP's Password Storage Cheat Sheet minimum, `m=19456` KiB (19 MiB),
//!   `t=2`, `p=1` ([`CURRENT`]), a 16-byte random salt per hash, a 32-byte output, stored as a
//!   PHC string (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`), so the parameters travel with
//!   the hash. A stored hash below the current parameters is re-hashed at the next correct
//!   sign-in ([`needs_rehash`]).
//! - **Rules** ([`check_new`]): 12 to 128 characters, any characters, no composition rules; not
//!   the handle, the sign-in name or its local part, and not one of [`COMMON`].
//! - The work runs on the blocking pool (the caller does that, `password::PasswordService`),
//!   never on the async executor.
//! - Never logged, never in an error: errors here are fixed words. Inputs are taken by
//!   reference and the caller keeps them in `Zeroizing` buffers; Argon2's own buffers are
//!   zeroized by the crate (its `zeroize` feature).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

/// The shortest password accepted, in characters.
pub const MIN_LEN: usize = 12;
/// The longest password accepted, in characters.
pub const MAX_LEN: usize = 128;

/// OWASP's minimum for Argon2id (Password Storage Cheat Sheet, checked 2026-11-20): 19 MiB of
/// memory, 2 iterations, 1 lane.
pub const CURRENT: HashParams = HashParams {
    m_kib: 19_456,
    t: 2,
    p: 1,
};

/// The Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashParams {
    /// Memory, in KiB.
    pub m_kib: u32,
    /// Iterations.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

impl HashParams {
    fn argon2(self) -> Result<Argon2<'static>, &'static str> {
        let params =
            Params::new(self.m_kib, self.t, self.p, Some(32)).map_err(|_| "bad parameters")?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    /// At least as strong as `other` in every parameter.
    pub fn at_least(self, other: HashParams) -> bool {
        self.m_kib >= other.m_kib && self.t >= other.t && self.p >= other.p
    }
}

/// Why a new password isn't accepted: `422 weak_password` with this as `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weak {
    TooShort,
    TooLong,
    /// The handle, the sign-in name, or the sign-in name's part before `@`.
    MatchesName,
    /// On the built-in list of the most common passwords.
    Common,
}

impl Weak {
    pub fn code(self) -> &'static str {
        match self {
            Self::TooShort => "too_short",
            Self::TooLong => "too_long",
            Self::MatchesName => "matches_name",
            Self::Common => "common",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::TooShort => "a password needs at least 12 characters",
            Self::TooLong => "a password may have at most 128 characters",
            Self::MatchesName => "a password can't be your handle or sign-in name",
            Self::Common => "that password is one of the most common ones; choose another",
        }
    }
}

/// The most common passwords of 12 characters or more (from the public breach-frequency lists
/// most guessing starts with), lower-case. Shorter ones are refused for their length anyway.
pub const COMMON: [&str; 60] = [
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123456789123",
    "123123123123",
    "123412341234",
    "111111111111",
    "000000000000",
    "222222222222",
    "666666666666",
    "987654321012",
    "1234567890qwerty",
    "1q2w3e4r5t6y",
    "1q2w3e4r5t6y7u",
    "1qaz2wsx3edc",
    "1qaz2wsx3edc4rfv",
    "zaq12wsxcde3",
    "zaq1zaq1zaq1",
    "qwertyuiop12",
    "qwertyuiopasdf",
    "qwertyuiop[]",
    "qwerty123456",
    "qwerqwerqwer",
    "asdfghjkl123",
    "asdfasdfasdf",
    "zxcvbnm12345",
    "1234qwerasdf",
    "abcdefghijkl",
    "abc123abc123",
    "aaaaaaaaaaaa",
    "password1234",
    "password123!",
    "password12345",
    "passwordpassword",
    "passw0rd1234",
    "p@ssw0rd1234",
    "p@ssword1234",
    "password1!password1!",
    "administrator",
    "administrator1",
    "changeme1234",
    "welcome12345",
    "welcome1234!",
    "letmein12345",
    "iloveyou1234",
    "trustno1trustno1",
    "princess1234",
    "football1234",
    "baseball1234",
    "monkey123456",
    "dragon123456",
    "sunshine1234",
    "superman1234",
    "starwars1234",
    "michael12345",
    "computer1234",
    "internet1234",
    "whatever1234",
    "pmospassword",
    "pmospassword1",
];

/// Check a new password for `handle` (with `sign_in_name`, when they have one).
pub fn check_new(password: &str, handle: &str, sign_in_name: Option<&str>) -> Result<(), Weak> {
    let chars = password.chars().count();
    if chars < MIN_LEN {
        return Err(Weak::TooShort);
    }
    if chars > MAX_LEN {
        return Err(Weak::TooLong);
    }
    let lowered = Zeroizing::new(password.trim().to_lowercase());
    let mut names = vec![handle.to_lowercase()];
    if let Some(name) = sign_in_name {
        let name = name.to_lowercase();
        if let Some((local, _)) = name.split_once('@') {
            names.push(local.to_string());
        }
        names.push(name);
    }
    if names.iter().any(|n| !n.is_empty() && *n == *lowered) {
        return Err(Weak::MatchesName);
    }
    if COMMON.contains(&lowered.as_str()) {
        return Err(Weak::Common);
    }
    Ok(())
}

/// 16 bytes from the operating system's generator.
fn salt() -> Result<SaltString, &'static str> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "no randomness")?;
    SaltString::encode_b64(&bytes).map_err(|_| "bad salt")
}

/// Hash `password` with `params`: a PHC string. Slow on purpose: call it on the blocking pool.
pub fn hash(password: &[u8], params: HashParams) -> Result<String, &'static str> {
    let salt = salt()?;
    params
        .argon2()?
        .hash_password(password, &salt)
        .map(|h| h.to_string())
        .map_err(|_| "hashing failed")
}

/// Does `password` match `stored` (a PHC string)? A string that doesn't parse, or isn't
/// Argon2id, never matches. Slow on purpose: call it on the blocking pool.
pub fn verify(password: &[u8], stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    if parsed.algorithm.as_str() != "argon2id" {
        return false;
    }
    Argon2::default().verify_password(password, &parsed).is_ok()
}

/// The parameters `stored` was hashed with, if it is an Argon2id PHC string.
pub fn params_of(stored: &str) -> Option<HashParams> {
    let parsed = PasswordHash::new(stored).ok()?;
    if parsed.algorithm.as_str() != "argon2id" {
        return None;
    }
    let p = Params::try_from(&parsed).ok()?;
    Some(HashParams {
        m_kib: p.m_cost(),
        t: p.t_cost(),
        p: p.p_cost(),
    })
}

/// Should `stored` be hashed again with `current` at the next correct sign-in? When it isn't
/// Argon2id, or any of its parameters is below `current`'s.
pub fn needs_rehash(stored: &str, current: HashParams) -> bool {
    params_of(stored).is_none_or(|p| !p.at_least(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small parameters, so the unit tests are quick; the real ones are tested in
    /// `tests/password.rs`.
    const QUICK: HashParams = HashParams {
        m_kib: 1024,
        t: 1,
        p: 1,
    };

    #[test]
    fn hashes_verify_and_carry_their_parameters() {
        let h = hash(b"correct horse battery", QUICK).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=1024,t=1,p=1$"), "{h}");
        assert!(verify(b"correct horse battery", &h));
        assert!(!verify(b"correct horse batterz", &h));
        assert!(!verify(b"correct horse battery", "not a hash"));
        assert_eq!(params_of(&h), Some(QUICK));
        // A unique salt per hash.
        assert_ne!(h, hash(b"correct horse battery", QUICK).unwrap());
        assert!(needs_rehash(&h, CURRENT));
        assert!(!needs_rehash(&h, QUICK));
        assert!(needs_rehash(
            "$argon2i$v=19$m=65536,t=3,p=1$c2FsdHNhbHQ$aGFzaA",
            QUICK
        ));
    }

    #[test]
    fn the_current_parameters_are_owasps_minimum_or_more() {
        assert!(CURRENT.at_least(HashParams {
            m_kib: 19_456,
            t: 2,
            p: 1
        }));
    }

    #[test]
    fn new_password_rules() {
        let ok = |p: &str| check_new(p, "ana", Some("ana.silva@example.com"));
        assert_eq!(ok("short"), Err(Weak::TooShort));
        assert_eq!(ok(&"x".repeat(11)), Err(Weak::TooShort));
        assert_eq!(ok(&"é".repeat(12)), Ok(()), "characters, not bytes");
        assert_eq!(ok(&"x".repeat(129)), Err(Weak::TooLong));
        assert_eq!(ok(&"x".repeat(128)), Ok(()));
        assert_eq!(ok("ANA.SILVA@example.com"), Err(Weak::MatchesName));
        // Spaces around a name don't make it another password.
        assert_eq!(ok("ana.silva   "), Err(Weak::MatchesName));
        assert_eq!(
            check_new("ana.silva.pmos", "ana", Some("ana.silva.pmos@x.io")),
            Err(Weak::MatchesName)
        );
        assert_eq!(
            check_new("longhandle12", "longhandle12", None),
            Err(Weak::MatchesName)
        );
        assert_eq!(ok("Password1234"), Err(Weak::Common));
        assert_eq!(ok("correct horse battery staple"), Ok(()));
        // No composition rules: any characters, spaces included.
        assert_eq!(ok("            a"), Ok(()));
        for c in COMMON {
            assert!(c.chars().count() >= MIN_LEN, "{c}");
            assert_eq!(c, c.to_lowercase());
        }
    }
}
