//! The one-time values of password sign-in: setup codes, recovery codes and the tokens
//! between steps. Each is random from the operating system, shown or sent once, and stored
//! only as a SHA-256 hash under a label of its own; every comparison of hashes is constant
//! time.
//!
//! | Value | Entropy | Shape | Lives |
//! | --- | --- | --- | --- |
//! | Setup (or reset) code | 100 bits | `XXXXX-XXXXX-XXXXX-XXXXX`, base32 | 24 hours, one setup |
//! | Recovery code | 80 bits | `xxxx-xxxx-xxxx-xxxx`, base32, lower-case | until used once, or regenerated |
//! | Pending token (after a correct password; during setup) | 256 bits | 43 base64url characters | 5 / 15 minutes |
//!
//! These entropies make a hash without a slow function enough: an offline guess at 80 bits is
//! out of reach, and online guesses are limited (`super::limits`).

use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::totp::base32;

/// How many recovery codes a person holds.
pub const RECOVERY_CODES: usize = 10;

fn random<const N: usize>() -> Result<Zeroizing<[u8; N]>, &'static str> {
    let mut b = Zeroizing::new([0u8; N]);
    getrandom::fill(b.as_mut()).map_err(|_| "no randomness")?;
    Ok(b)
}

fn grouped(text: &str, every: usize) -> String {
    text.as_bytes()
        .chunks(every)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// Strip what a person adds when typing a code (spaces, hyphens) and fold the case.
fn folded(code: &str) -> Zeroizing<String> {
    Zeroizing::new(
        code.chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect(),
    )
}

fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Do two stored hashes match? Constant time.
pub fn same_hash(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// A new setup code (100 bits): `XXXXX-XXXXX-XXXXX-XXXXX`.
pub fn new_setup_code() -> Result<Zeroizing<String>, &'static str> {
    let bytes = random::<13>()?;
    // 13 bytes are 104 bits; the first 20 base32 characters carry 100 of them.
    let text = Zeroizing::new(base32(bytes.as_ref()));
    Ok(Zeroizing::new(grouped(&text[..20], 5)))
}

/// A setup code as typed, folded: 20 base32 characters, or `None`.
fn setup_code_folded(code: &str) -> Option<Zeroizing<String>> {
    let f = folded(code);
    (f.len() == 20 && f.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'))).then_some(f)
}

/// The stored hash of a setup code, or `None` when it can't be one (wrong length or
/// characters: nothing is looked up).
pub fn setup_code_hash(code: &str) -> Option<String> {
    let f = setup_code_folded(code)?;
    Some(sha256_hex(&[b"pmos-password-setup-code-v1", f.as_bytes()]))
}

/// Ten new recovery codes, each `xxxx-xxxx-xxxx-xxxx` (80 bits).
pub fn new_recovery_codes() -> Result<Vec<Zeroizing<String>>, &'static str> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let bytes = random::<10>()?;
            let text = Zeroizing::new(base32(bytes.as_ref()).to_lowercase());
            Ok(Zeroizing::new(grouped(&text, 4)))
        })
        .collect()
}

/// The stored hash of `handle`'s recovery code, or `None` when it can't be one.
pub fn recovery_code_hash(handle: &str, code: &str) -> Option<String> {
    let f = folded(code);
    let ok = f.len() == 16 && f.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'));
    ok.then(|| {
        sha256_hex(&[
            b"pmos-password-recovery-code-v1",
            handle.as_bytes(),
            f.as_bytes(),
        ])
    })
}

/// A new pending token (256 bits), base64url without padding.
pub fn new_token() -> Result<Zeroizing<String>, &'static str> {
    let bytes = random::<32>()?;
    Ok(Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_ref()),
    ))
}

/// The stored hash of a pending token, or `None` when it can't be one.
pub fn token_hash(token: &str) -> Option<String> {
    let ok = token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| sha256_hex(&[b"pmos-password-pending-v1", token.as_bytes()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_codes_are_100_bits_and_typed_loosely() {
        let code = new_setup_code().unwrap();
        assert_eq!(code.len(), 23, "{}", code.as_str());
        assert_eq!(code.matches('-').count(), 3);
        let h = setup_code_hash(&code).unwrap();
        assert!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        // Lower case, spaces instead of hyphens: the same code.
        let typed = code.to_lowercase().replace('-', " ");
        assert_eq!(setup_code_hash(&typed).as_deref(), Some(h.as_str()));
        assert_ne!(code.as_str(), new_setup_code().unwrap().as_str());
        for bad in [
            "",
            "ABCDE",
            "ABCDE-ABCDE-ABCDE-ABCD1",
            "ABCDE-ABCDE-ABCDE-ABCDEF",
        ] {
            assert_eq!(setup_code_hash(bad), None, "{bad}");
        }
    }

    #[test]
    fn recovery_codes_are_ten_80_bit_codes_bound_to_their_owner() {
        let codes = new_recovery_codes().unwrap();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let distinct: std::collections::BTreeSet<&str> = codes.iter().map(|c| c.as_str()).collect();
        assert_eq!(distinct.len(), RECOVERY_CODES);
        for c in &codes {
            assert_eq!(c.len(), 19, "{}", c.as_str());
            let h = recovery_code_hash("ana", c).unwrap();
            assert_eq!(
                recovery_code_hash("ana", &c.to_uppercase()),
                Some(h.clone())
            );
            assert_ne!(recovery_code_hash("ben", c), Some(h));
        }
        assert_eq!(recovery_code_hash("ana", "abcd-efgh"), None);
    }

    #[test]
    fn tokens_are_256_bits_and_hashed() {
        let t = new_token().unwrap();
        assert_eq!(t.len(), 43);
        let h = token_hash(&t).unwrap();
        assert!(same_hash(&h, &token_hash(&t).unwrap()));
        assert!(!same_hash(&h, &token_hash(&new_token().unwrap()).unwrap()));
        assert_eq!(token_hash("short"), None);
        assert_eq!(token_hash(&"!".repeat(43)), None);
    }
}
