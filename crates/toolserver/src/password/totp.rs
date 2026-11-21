//! The authenticator app's codes: TOTP, RFC 6238 over HOTP, RFC 4226.
//!
//! HMAC-SHA-1, 6 digits, 30-second steps from the Unix epoch, a 160-bit secret made here
//! ([`new_secret`]). A code is accepted for the step before, the current step and the step
//! after (clock skew of up to 30 seconds either way), and only for a step later than the last
//! one accepted, so each code works once ([`verify`]). Codes are compared in constant time.
//!
//! The secret is shown once at enrolment, as base32 text and an `otpauth://` URI ([`uri`]) the
//! front end draws as a QR code; it is stored sealed by the vault (`crate::vault::KeyRing`).

use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Digits in a code.
pub const DIGITS: u32 = 6;
/// Seconds in a step.
pub const PERIOD: i64 = 30;
/// Steps accepted either side of the current one.
pub const SKEW_STEPS: i64 = 1;
/// The secret's length in bytes (160 bits, RFC 4226's recommendation).
pub const SECRET_BYTES: usize = 20;
/// The issuer the authenticator app shows.
pub const ISSUER: &str = "PM OS";

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 (RFC 4648), upper-case, no padding.
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Base32 back to bytes: case-insensitive, spaces and `=` ignored. `None` for any other
/// character.
pub fn unbase32(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8));
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.chars().filter(|c| !matches!(c, ' ' | '=')) {
        let v = ALPHABET
            .iter()
            .position(|a| *a as char == c.to_ascii_uppercase())? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
        buffer &= (1 << bits) - 1;
    }
    Some(out)
}

/// A new 160-bit secret from the operating system's generator.
pub fn new_secret() -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let mut s = Zeroizing::new(vec![0u8; SECRET_BYTES]);
    getrandom::fill(&mut s).map_err(|_| "no randomness")?;
    Ok(s)
}

/// HOTP (RFC 4226) for `counter`: the 6-digit code as a number.
pub fn hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let bin = (u32::from(digest[offset]) & 0x7f) << 24
        | u32::from(digest[offset + 1]) << 16
        | u32::from(digest[offset + 2]) << 8
        | u32::from(digest[offset + 3]);
    bin % 10u32.pow(DIGITS)
}

/// The step `unix_seconds` falls in.
pub fn step_at(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(PERIOD)
}

/// The code for `step`, as the app shows it (zero-padded).
pub fn code_at(secret: &[u8], step: i64) -> String {
    format!(
        "{:0width$}",
        hotp(secret, step.max(0) as u64),
        width = DIGITS as usize
    )
}

/// A code as typed: digits only once spaces are removed, exactly [`DIGITS`] of them.
pub fn normalize(code: &str) -> Option<String> {
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    (digits.len() == DIGITS as usize && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some(digits)
}

/// The step `code` is valid for at `unix_seconds` (the step before, now, or after), if it is
/// later than `last_step` (the last one accepted for this secret). Every candidate is checked,
/// in constant time each, whatever matched.
pub fn verify(secret: &[u8], code: &str, unix_seconds: i64, last_step: Option<i64>) -> Option<i64> {
    let code = normalize(code)?;
    let now = step_at(unix_seconds);
    let mut found = None;
    for step in (now - SKEW_STEPS)..=(now + SKEW_STEPS) {
        let candidate = code_at(secret, step);
        let same: bool = candidate.as_bytes().ct_eq(code.as_bytes()).into();
        let fresh = last_step.is_none_or(|last| step > last);
        if same && fresh && found.is_none() {
            found = Some(step);
        }
    }
    found
}

fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The enrolment URI (the Key Uri Format authenticator apps read from a QR code):
/// `otpauth://totp/PM%20OS:<account>?secret=<base32>&issuer=PM%20OS&algorithm=SHA1&digits=6&period=30`.
pub fn uri(account: &str, secret_base32: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={secret_base32}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={PERIOD}",
        percent(ISSUER),
        percent(account),
        percent(ISSUER),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 Appendix B's SHA-1 secret.
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc_6238_test_vectors_last_six_digits() {
        // (time, the 8-digit TOTP from the RFC): our 6 digits are its last six.
        for (t, eight) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(code_at(RFC_SECRET, step_at(t)), eight[2..], "t={t}");
        }
        // RFC 4226 Appendix D's HOTP values, 6 digits.
        let hotp_values = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (counter, want) in hotp_values.iter().enumerate() {
            assert_eq!(hotp(RFC_SECRET, counter as u64), *want);
        }
    }

    #[test]
    fn base32_round_trips_with_rfc_4648_vectors() {
        for (raw, enc) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32(raw.as_bytes()), enc);
            assert_eq!(unbase32(enc).unwrap().as_slice(), raw.as_bytes());
            assert_eq!(
                unbase32(&enc.to_lowercase()).unwrap().as_slice(),
                raw.as_bytes()
            );
        }
        assert!(unbase32("MZ1W").is_none());
        let s = new_secret().unwrap();
        assert_eq!(s.len(), SECRET_BYTES);
        assert_eq!(base32(&s).len(), 32);
        assert_eq!(unbase32(&base32(&s)).unwrap().as_slice(), s.as_slice());
        assert_ne!(s.as_slice(), new_secret().unwrap().as_slice());
    }

    #[test]
    fn the_skew_window_is_one_step_either_way() {
        let t = 1_111_111_111;
        let now = step_at(t);
        for (offset, ok) in [(-2, false), (-1, true), (0, true), (1, true), (2, false)] {
            let code = code_at(RFC_SECRET, now + offset);
            assert_eq!(
                verify(RFC_SECRET, &code, t, None),
                ok.then_some(now + offset),
                "offset {offset}"
            );
        }
        // 29 seconds into the next step, the previous code is still accepted; 31 not.
        let code = code_at(RFC_SECRET, now);
        let next_step_start = (now + 1) * PERIOD;
        assert!(verify(RFC_SECRET, &code, next_step_start + 29, None).is_some());
        assert!(verify(RFC_SECRET, &code, next_step_start + PERIOD + 1, None).is_none());
    }

    #[test]
    fn a_code_is_accepted_once() {
        let t = 2_000_000_000;
        let code = code_at(RFC_SECRET, step_at(t));
        let step = verify(RFC_SECRET, &code, t, None).unwrap();
        assert_eq!(verify(RFC_SECRET, &code, t, Some(step)), None);
        // An older code is not accepted after a newer one either.
        let older = code_at(RFC_SECRET, step - 1);
        assert_eq!(verify(RFC_SECRET, &older, t, Some(step)), None);
        // The next step's code still is.
        let newer = code_at(RFC_SECRET, step + 1);
        assert_eq!(verify(RFC_SECRET, &newer, t, Some(step)), Some(step + 1));
    }

    #[test]
    fn codes_are_six_digits_as_typed() {
        assert_eq!(normalize(" 123 456 ").as_deref(), Some("123456"));
        for bad in ["12345", "1234567", "12345a", "", "１２３４５６"] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_uri_is_the_key_uri_format() {
        let u = uri("ana@example.com", "JBSWY3DPEHPK3PXP");
        assert_eq!(
            u,
            "otpauth://totp/PM%20OS:ana@example.com?secret=JBSWY3DPEHPK3PXP&issuer=PM%20OS&algorithm=SHA1&digits=6&period=30"
        );
        assert!(uri("a b&c", "X").contains("PM%20OS:a%20b%26c?"));
    }
}
