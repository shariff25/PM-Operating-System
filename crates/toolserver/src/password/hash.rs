//! Passwords: the rules a new one must meet, and Argon2id hashing.
//!
//! - **Argon2id** with OWASP's Password Storage Cheat Sheet minimum, `m=19456` KiB (19 MiB),
//!   `t=2`, `p=1` ([`CURRENT`]), a 16-byte random salt per hash, a 32-byte output, stored as a
//!   PHC string (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`), so the parameters travel with
//!   the hash. A stored hash below the current parameters is re-hashed at the next correct
//!   sign-in ([`needs_rehash`]).
//! - **Rules** ([`check_new`]): at least 12 characters not counting whitespace at either end,
//!   counting a run of whitespace once and not counting characters that show nothing
//!   ([`collapsed`], [`is_ignorable`]), at most 128 as typed; any
//!   characters, no composition rules; not the handle, the sign-in name or its local part, and
//!   not one of [`COMMON`] (with or without its spaces, compared in NFKC). The rules never
//!   change what is hashed.
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
            Self::TooShort => {
                "a password needs at least 12 characters, not counting spaces at either end, and a run of spaces counts once"
            }
            Self::TooLong => "a password may have at most 128 characters",
            Self::MatchesName => "a password can't be your handle or sign-in name",
            Self::Common => "that password is one of the most common ones; choose another",
        }
    }
}

/// The common-password list (the gate review of 04c175f, G5): every entry of SecLists'
/// 1,000,000 most frequent passwords (xato.net's ten-million set) that the length rule
/// wouldn't already refuse, trimmed and lower-cased, one per line. Source, derivation, hashes
/// and the MIT licence: `crates/toolserver/data/common-passwords.NOTICE`.
pub const COMMON_LIST: &str = include_str!("../../data/common-passwords.txt");

/// A few more, written here: the ones of the old built-in list (the most common passwords of
/// 12 characters or more on public breach-frequency lists, and PM OS's own name), lower-case.
/// [`COMMON_LIST`] holds most of them already.
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

/// Every common password, as a set, built once. A lookup hashes the candidate (SipHash with a
/// random key) and compares it with the few entries in its bucket: its time depends on the
/// candidate's length, not on where, or whether, it is in the list.
fn common_set() -> &'static std::collections::HashSet<&'static str> {
    static SET: std::sync::OnceLock<std::collections::HashSet<&'static str>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| {
        COMMON_LIST
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .chain(COMMON)
            .collect()
    })
}

/// Is `lowered` (trimmed, lower-case) a common password? Never logged.
pub fn is_common(lowered: &str) -> bool {
    common_set().contains(lowered)
}

/// How many common passwords are refused (tests, the documentation).
pub fn common_count() -> usize {
    common_set().len()
}

/// Characters that show nothing and are dropped when the rules read a password (the review of
/// 8e8f8fe, a Low after M3): Unicode's `Default_Ignorable_Code_Point` (Unicode 15.1,
/// `DerivedCoreProperties.txt`), which holds the invisible format characters (the zero-width
/// space, the joiners and non-joiner, the word joiner, the byte-order mark, the bidirectional
/// marks and controls, the soft hyphen, the invisible operators, the tags) plus the variation
/// selectors, the Hangul fillers, the combining grapheme joiner and the Mongolian free
/// variation selectors and vowel separator, and the code points Unicode keeps unassigned in
/// those ranges. **Not** dropped: the format characters outside that property, which show a
/// glyph (the Arabic number signs U+0600..U+0605, U+06DD, U+070F, U+0890..U+0891, U+08E2, the
/// Kaithi number signs U+110BD, U+110CD). For the rules only: what is hashed is exactly what
/// was typed.
pub fn is_ignorable(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// `password` as the rules read it (the gate review of 8e8f8fe, M3): every character that
/// shows nothing ([`is_ignorable`]) dropped, then whitespace at either end dropped and every
/// run of whitespace inside made one space. Whitespace is Unicode's `White_Space` (spaces,
/// tabs, line breaks, the no-break and ideographic spaces and the other Unicode spaces), as
/// [`char::is_whitespace`] reads it. So twelve zero-width spaces are nothing, and a run of
/// spaces with zero-width spaces between them is one space. For the rules only: what is
/// hashed, and checked at sign-in, is always exactly what the person typed.
pub fn collapsed(password: &str) -> Zeroizing<String> {
    let visible: Zeroizing<String> =
        Zeroizing::new(password.chars().filter(|c| !is_ignorable(*c)).collect());
    let mut out = Zeroizing::new(String::with_capacity(visible.len()));
    for word in visible.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// `text` in Unicode's compatibility composition (NFKC), lower-cased: the form the name and
/// common-list checks compare, so full-width letters, ligatures and other compatibility forms
/// of a common password are caught (`ｐａｓｓｗｏｒｄ１２３４` is `password1234`). Never what is
/// hashed, and not what the length is counted on.
fn compared(text: &str) -> Zeroizing<String> {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: Zeroizing<String> = Zeroizing::new(text.nfkc().collect());
    Zeroizing::new(nfkc.to_lowercase())
}

/// Check a new password for `handle` (with `sign_in_name`, when they have one).
///
/// The length is counted on [`collapsed`] (M3): twelve spaces, or a short word padded with
/// spaces to twelve, is too short. The name and common-list checks are made on the collapsed
/// text in NFKC, lower-cased ([`compared`]), and again with every whitespace character taken
/// out, so `password` padded with spaces, `pass word 1234`, `password` and a zero-width space
/// and `1234`, or `password1234` in full-width letters, is refused as the common password it
/// is. Not normalised: letters that only look alike (a Cyrillic `а` for a Latin `a`), and what
/// is hashed (`docs/password-sign-in.md`, "The rules for a password").
pub fn check_new(password: &str, handle: &str, sign_in_name: Option<&str>) -> Result<(), Weak> {
    if password.chars().count() > MAX_LEN {
        return Err(Weak::TooLong);
    }
    let collapsed = collapsed(password);
    if collapsed.chars().count() < MIN_LEN {
        return Err(Weak::TooShort);
    }
    let lowered = compared(&collapsed);
    let joined: Zeroizing<String> =
        Zeroizing::new(lowered.chars().filter(|c| !c.is_whitespace()).collect());
    let mut names = vec![compared(handle).to_string()];
    if let Some(name) = sign_in_name {
        let name = compared(name).to_string();
        if let Some((local, _)) = name.split_once('@') {
            names.push(local.to_string());
        }
        names.push(name);
    }
    let forms = [lowered.as_str(), joined.as_str()];
    if names
        .iter()
        .any(|n| !n.is_empty() && forms.contains(&n.as_str()))
    {
        return Err(Weak::MatchesName);
    }
    if forms.iter().any(|f| is_common(f)) {
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
        // Spaces around a name don't make it another password (nor long enough: M3).
        assert_eq!(ok("ana.silva   "), Err(Weak::TooShort));
        assert_eq!(ok("   ana.silva@example.com   "), Err(Weak::MatchesName));
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
        assert_eq!(ok("plum kettle orbit 42"), Ok(()));
        assert_eq!(ok("plum\tkettle\u{3000}orbit 42"), Ok(()));
        // M3 (the gate review of 8e8f8fe): spaces don't make up the length. Before, this
        // asserted that twelve spaces and a letter were accepted; it now asserts the refusal.
        assert_eq!(ok("            a"), Err(Weak::TooShort));
        assert_eq!(ok(&" ".repeat(12)), Err(Weak::TooShort));
        assert_eq!(ok(&" ".repeat(40)), Err(Weak::TooShort));
        assert_eq!(ok("password    "), Err(Weak::TooShort));
        assert_eq!(ok("    password    "), Err(Weak::TooShort));
        assert_eq!(ok("ab          cd"), Err(Weak::TooShort), "runs count once");
        // Unicode whitespace too: tabs, line breaks, no-break and ideographic spaces.
        assert_eq!(
            ok("pass\u{a0}\u{a0}\u{a0}\u{a0}\u{3000}\t\n\u{2003}word"),
            Err(Weak::TooShort)
        );
        // Common ones padded, or spaced out inside, are still common.
        assert_eq!(ok("password1234    "), Err(Weak::Common));
        assert_eq!(ok("password   1234"), Err(Weak::Common));
        assert_eq!(ok("p a s s w o r d 1 2 3 4"), Err(Weak::Common));
        assert_eq!(ok("password\u{a0}1234"), Err(Weak::Common));
        assert_eq!(ok("ana . silva @ example . com"), Err(Weak::MatchesName));
        // The review of 8e8f8fe, a Low after M3: characters that show nothing don't make up
        // the length, and compatibility forms are compared as what they are. Before, these
        // asserted that `ｐａｓｓｗｏｒｄ１２３４` and `password`, a zero-width space and `1234` were
        // accepted; they now assert the refusals.
        assert_eq!(ok(&"\u{200b}".repeat(12)), Err(Weak::TooShort));
        assert_eq!(ok(&"\u{200b}".repeat(128)), Err(Weak::TooShort));
        assert_eq!(
            ok("abc\u{200b}\u{200c}\u{200d}\u{2060}\u{feff}\u{ad}defgh"),
            Err(Weak::TooShort)
        );
        assert_eq!(
            ok("abcdefghijk\u{fe0f}"),
            Err(Weak::TooShort),
            "a variation selector"
        );
        assert_eq!(ok("abcdefghijk\u{e0041}"), Err(Weak::TooShort), "a tag");
        assert_eq!(
            ok("abcdefghijk\u{3164}"),
            Err(Weak::TooShort),
            "a Hangul filler"
        );
        assert_eq!(
            ok("abcdefghijk\u{202e}"),
            Err(Weak::TooShort),
            "a bidi override"
        );
        assert_eq!(ok("password\u{200b}1234"), Err(Weak::Common));
        assert_eq!(ok("pass\u{200b} \u{200b} word1234"), Err(Weak::Common));
        assert_eq!(ok("ｐａｓｓｗｏｒｄ１２３４"), Err(Weak::Common));
        assert_eq!(ok("ＰＡＳＳＷＯＲＤ１２３４"), Err(Weak::Common));
        assert_eq!(ok("ana.silva@ｅｘａｍｐｌｅ.com"), Err(Weak::MatchesName));
        // Still not normalised (documented): letters that only look alike. A visible
        // character with an invisible one beside it counts as one.
        assert_eq!(ok("p\u{430}ssword1234"), Ok(()), "a Cyrillic a");
        assert_eq!(ok("plum kettle\u{200b} orbit 42"), Ok(()));
        assert_eq!(collapsed("a\u{200b} \u{200b} \u{feff}b").as_str(), "a b");
        assert_eq!(collapsed(&"\u{200b}".repeat(12)).as_str(), "");
        for c in [
            '\u{200b}',
            '\u{ad}',
            '\u{feff}',
            '\u{2064}',
            '\u{1d173}',
            '\u{e0fff}',
        ] {
            assert!(is_ignorable(c), "{c:?}");
        }
        for c in ['a', ' ', '\u{600}', '\u{6dd}', '\u{110bd}', '\u{2070}'] {
            assert!(!is_ignorable(c), "{c:?}");
        }
        // At most 128 as typed: spaces count there, since they are hashed.
        assert_eq!(
            ok(&format!("{}{}", "x".repeat(20), " ".repeat(109))),
            Err(Weak::TooLong)
        );
        assert_eq!(collapsed("  a \t b\u{3000}\u{3000}c  ").as_str(), "a b c");
        for c in COMMON {
            assert!(c.chars().count() >= MIN_LEN, "{c}");
            assert_eq!(c, c.to_lowercase());
        }
    }

    // G5: a real list, of at least the passwords the 10,000 most common would be past the
    // length rule, and far more: every one of the 1,000,000 most common that is long enough.
    #[test]
    fn the_common_list_is_a_real_one() {
        assert!(common_count() >= 46_000, "{}", common_count());
        for line in COMMON_LIST.lines() {
            let n = line.chars().count();
            assert!((MIN_LEN..=MAX_LEN).contains(&n));
            assert_eq!(line, line.trim().to_lowercase());
        }
        let ok = |p: &str| check_new(p, "ana", Some("ana.silva@example.com"));
        // From the list (high and low in it), in any case and with spaces around.
        for p in [
            "123qweasdzxc",
            "QAZWSXEDCRFV",
            "  qwertyqwerty ",
            "leavemealone",
            "iloveyouforever",
        ] {
            assert_eq!(ok(p), Err(Weak::Common), "{p}");
        }
        assert_eq!(
            ok("pmospassword1"),
            Err(Weak::Common),
            "the written ones too"
        );
        assert_eq!(ok("plum-kettle orbit 42"), Ok(()));
    }
}
