//! Tests across the crate: every verification rule and error, the wire format's strictness,
//! property-style round trips and tampering, and key redaction.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use pmos_contract::model::Role;

use crate::wire::{self, Malformed, Nonce};
use crate::*;

const NOW: u64 = 1_800_000_000;
/// Test-only key material: a counting pattern, never a real key.
const KEY_BYTES: [u8; 32] = {
    let mut k = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        k[i] = i as u8;
        i += 1;
    }
    k
};

fn key(id: &str, fill: u8) -> Key {
    Key::new(id, &[fill; 32]).unwrap()
}

fn ring() -> KeyRing {
    KeyRing::new(Key::new("k1", &KEY_BYTES).unwrap())
}

fn claims() -> Claims {
    Claims::new("web", "bots", "ana", &[Role::Admin, Role::Pm], "ses_1").unwrap()
}

fn policy() -> VerifyPolicy {
    VerifyPolicy::new("bots", &["web", "console"]).unwrap()
}

fn req() -> RequestBinding {
    RequestBinding::new("POST", "/v1/tasks", b"{}").unwrap()
}

fn sign_at(ring: &KeyRing, c: &Claims, now: u64, ttl: u64, r: Option<&RequestBinding>) -> String {
    ring.sign(c, now, ttl, r).unwrap()
}

/// A replay guard that accepts everything: isolates the other checks.
struct AcceptAll;
impl ReplayGuard for AcceptAll {
    fn check_and_record(&self, _: &Nonce, _: u64, _: u64) -> Result<(), ReplayRejection> {
        Ok(())
    }
}

struct Down;
impl ReplayGuard for Down {
    fn check_and_record(&self, _: &Nonce, _: u64, _: u64) -> Result<(), ReplayRejection> {
        Err(ReplayRejection::Unavailable)
    }
}

fn verify(ring: &KeyRing, token: &str, now: u64) -> Result<Assertion, VerifyError> {
    ring.verify(token, &policy(), now, Some(&req()), &AcceptAll)
}

// ---------- Wire format ----------

#[test]
fn matches_the_independently_computed_test_vectors() {
    // Computed outside Rust (Python hmac/hashlib) from the documented format.
    let unbound = "v=1;kid=k1;iss=web;aud=bots;sub=ana;roles=pm,admin;sid=ses_1;iat=1800000000;exp=1800000060;nonce=BwcHBwcHBwcHBwcHBwcHBw;req=-;sig=x-BKUI5mh1PCMCLIJtOSTKD055pwfkM94XwX8llZ6qc";
    let bound = "v=1;kid=k1;iss=web;aud=bots;sub=ana;roles=pm,admin;sid=ses_1;iat=1800000000;exp=1800000060;nonce=BwcHBwcHBwcHBwcHBwcHBw;req=6YkoG4-kWITz5ORbi2RvQCWbj7Jym5YmNORsUG4hfIA;sig=yJEk1jGoUQpzZCq5P1wekqL7Witr_iTo7Wa0APTx-7Y";
    let r = ring();
    let nonce = Nonce([7; NONCE_LEN]);
    assert_eq!(
        r.sign_with_nonce(&claims(), NOW, 60, None, nonce).unwrap(),
        unbound
    );
    assert_eq!(
        r.sign_with_nonce(&claims(), NOW, 60, Some(&req()), nonce)
            .unwrap(),
        bound
    );
    let a = verify(&r, bound, NOW).unwrap();
    assert_eq!(a.version(), 1);
    assert_eq!(a.key_id(), "k1");
    assert_eq!(a.issuer(), "web");
    assert_eq!(a.audience(), "bots");
    assert_eq!(a.subject(), "ana");
    assert_eq!(a.roles(), &[Role::Pm, Role::Admin]);
    assert!(a.has_role(&Role::Admin) && !a.has_role(&Role::Auditor));
    assert_eq!(a.session(), "ses_1");
    assert_eq!((a.issued_at(), a.expires_at()), (NOW, NOW + 60));
    assert_eq!(a.nonce().as_bytes(), &[7; NONCE_LEN]);
    assert!(a.is_request_bound());
}

#[test]
fn a_token_is_one_header_safe_line() {
    let t = sign_at(&ring(), &claims(), NOW, 60, Some(&req()));
    assert!(t.len() < 300, "{}", t.len());
    assert!(t.bytes().all(|c| c.is_ascii_graphic()));
    assert!(!t.contains(['"', '\\', ' ']));
}

#[test]
fn roles_may_be_empty() {
    let c = Claims::new("web", "bots", "ana", &[], "s").unwrap();
    let t = sign_at(&ring(), &c, NOW, 60, Some(&req()));
    assert!(t.contains(";roles=;"));
    assert!(verify(&ring(), &t, NOW).unwrap().roles().is_empty());
}

fn parse_err(t: &str) -> Malformed {
    match wire::parse(t) {
        Err(m) => m,
        Ok(_) => panic!("parsed: {t}"),
    }
}

fn base() -> String {
    ring()
        .sign_with_nonce(&claims(), NOW, 60, Some(&req()), Nonce([7; NONCE_LEN]))
        .unwrap()
}

/// Replace field `name`'s value in `t`.
fn with_field(t: &str, name: &str, value: &str) -> String {
    t.split(';')
        .map(|p| match p.split_once('=') {
            Some((n, _)) if n == name => format!("{n}={value}"),
            _ => p.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[test]
fn the_parser_refuses_bad_framing() {
    let t = base();
    assert!(wire::parse(&t).is_ok());
    assert_eq!(parse_err(""), Malformed::Empty);
    assert_eq!(parse_err(&"v".repeat(MAX_LEN + 1)), Malformed::TooLong);
    assert_eq!(
        parse_err(&format!("{t}{}", "a".repeat(MAX_LEN))),
        Malformed::TooLong
    );
    assert_eq!(
        parse_err(&t.replace(";sid", "; sid")),
        Malformed::BadCharacter
    );
    assert_eq!(parse_err(&t.replace("ana", "anä")), Malformed::BadCharacter);
    assert_eq!(parse_err(&format!("{t}\n")), Malformed::BadCharacter);
    assert_eq!(parse_err(&t.replacen("v=1;", "", 1)), Malformed::NoVersion);
    assert_eq!(parse_err(&format!("x=1;{t}")), Malformed::NoVersion);
    for v in ["2", "0", "01", "1.0", ""] {
        assert_eq!(
            parse_err(&t.replacen("v=1;", &format!("v={v};"), 1)),
            Malformed::UnknownVersion,
            "{v}"
        );
    }
    assert_eq!(
        parse_err(&t.replace(";sid=", ";sidses_1;x=")),
        Malformed::NoSeparator
    );
    assert_eq!(
        parse_err(&t.replace(";sid=", ";extra=1;sid=")),
        Malformed::UnknownField
    );
    assert_eq!(
        parse_err(&t.replace(";sid=", ";SID=")),
        Malformed::UnknownField
    );
    assert_eq!(
        parse_err(&t.replace(";sid=ses_1;", ";sid=ses_1;sid=ses_1;")),
        Malformed::DuplicateField("sid")
    );
    assert_eq!(
        parse_err(&format!("{t};sig=AAAA")),
        Malformed::DuplicateField("sig")
    );
    assert_eq!(
        parse_err(&t.replace(";iss=web;aud=bots;", ";aud=bots;iss=web;")),
        Malformed::FieldOrder("iss")
    );
    assert_eq!(
        parse_err(&t.replace(";roles=pm,admin", "")),
        Malformed::MissingField("roles")
    );
    let unsigned = &t[..t.find(";sig=").unwrap()];
    assert_eq!(parse_err(unsigned), Malformed::MissingField("sig"));
    assert_eq!(parse_err(&format!("{t};")), Malformed::NoSeparator);
}

#[test]
fn the_parser_refuses_bad_values() {
    let t = base();
    let cases: &[(&str, &str, Malformed)] = &[
        ("kid", "", Malformed::BadValue("kid")),
        ("kid", "K1", Malformed::BadValue("kid")),
        ("iss", "Web", Malformed::BadValue("iss")),
        ("iss", "a,b", Malformed::BadValue("iss")),
        ("aud", "", Malformed::BadValue("aud")),
        ("aud", "bots=x", Malformed::BadValue("aud")),
        ("sub", "", Malformed::BadValue("sub")),
        ("sub", "Ana", Malformed::BadValue("sub")),
        ("sub", "ana@acme.io", Malformed::BadValue("sub")),
        ("roles", "boss", Malformed::BadValue("roles")),
        ("roles", "PM", Malformed::BadValue("roles")),
        ("roles", "pm,", Malformed::BadValue("roles")),
        ("roles", ",pm", Malformed::BadValue("roles")),
        ("roles", "admin,pm", Malformed::NonCanonical("roles")),
        ("roles", "pm,pm", Malformed::NonCanonical("roles")),
        ("sid", "", Malformed::BadValue("sid")),
        ("sid", "a.b", Malformed::BadValue("sid")),
        ("iat", "", Malformed::BadValue("iat")),
        ("iat", "+1800000000", Malformed::BadValue("iat")),
        ("iat", "-1", Malformed::BadValue("iat")),
        ("iat", "01800000000", Malformed::NonCanonical("iat")),
        ("iat", "99999999999999999999", Malformed::BadValue("iat")),
        ("exp", "0x6b49d23c", Malformed::BadValue("exp")),
        ("exp", "01800000060", Malformed::NonCanonical("exp")),
        ("nonce", "", Malformed::BadValue("nonce")),
        (
            "nonce",
            "BwcHBwcHBwcHBwcHBwcHBw==",
            Malformed::BadValue("nonce"),
        ),
        (
            "nonce",
            "BwcHBwcHBwcHBwcHBwcHB",
            Malformed::BadValue("nonce"),
        ),
        (
            "nonce",
            "BwcHBwcHBwcHBwcHBwcHB+",
            Malformed::BadValue("nonce"),
        ),
        // Same bytes, stray trailing bits set: a second spelling.
        (
            "nonce",
            "BwcHBwcHBwcHBwcHBwcHBx",
            Malformed::NonCanonical("nonce"),
        ),
        ("req", "", Malformed::BadValue("req")),
        ("req", "--", Malformed::BadValue("req")),
        ("sig", "", Malformed::BadValue("sig")),
        ("sig", "AAAA", Malformed::BadValue("sig")),
    ];
    for (field, value, want) in cases {
        assert_eq!(
            parse_err(&with_field(&t, field, value)),
            *want,
            "{field}={value}"
        );
    }
    // A non-canonical spelling of the signature's bytes.
    let sig = t.rsplit_once("sig=").unwrap().1;
    // 32 bytes leave 2 spare bits in the last character; setting one spells the same bytes.
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut odd = sig.as_bytes().to_vec();
    let last = odd.last_mut().unwrap();
    let v = ALPHABET.iter().position(|c| c == last).unwrap();
    assert_eq!(v % 4, 0, "canonical final character");
    *last = ALPHABET[v + 1];
    assert_eq!(
        parse_err(&with_field(&t, "sig", std::str::from_utf8(&odd).unwrap())),
        Malformed::NonCanonical("sig")
    );
}

#[test]
fn the_parser_enforces_the_lifetime() {
    let t = base();
    for exp in ["1800000000", "1799999999", "1800000121"] {
        assert_eq!(
            parse_err(&with_field(&t, "exp", exp)),
            Malformed::Lifetime,
            "{exp}"
        );
    }
    assert!(wire::parse(&with_field(&t, "exp", "1800000120")).is_ok());
}

// ---------- Signing ----------

#[test]
fn signing_enforces_the_lifetime() {
    let r = ring();
    assert_eq!(
        r.sign(&claims(), NOW, 0, None).unwrap_err(),
        SignError::Lifetime
    );
    assert_eq!(
        r.sign(&claims(), NOW, MAX_LIFETIME_SECS + 1, None)
            .unwrap_err(),
        SignError::Lifetime
    );
    assert!(r.sign(&claims(), NOW, MAX_LIFETIME_SECS, None).is_ok());
    assert_eq!(
        r.sign(&claims(), u64::MAX - 10, 60, None).unwrap_err(),
        SignError::Clock
    );
}

#[test]
fn every_signature_has_a_fresh_nonce() {
    let r = ring();
    let a = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    let b = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    assert_ne!(a, b);
    let cache = MemoryReplayCache::default();
    assert!(r.verify(&a, &policy(), NOW, Some(&req()), &cache).is_ok());
    assert!(r.verify(&b, &policy(), NOW, Some(&req()), &cache).is_ok());
}

#[test]
fn longest_fields_still_fit() {
    let c = Claims::new(
        &"i".repeat(MAX_NAME_LEN),
        &"a".repeat(MAX_NAME_LEN),
        &"s".repeat(64),
        &[
            Role::Pm,
            Role::SkillAuthor,
            Role::Admin,
            Role::Reviewer,
            Role::Auditor,
        ],
        &"x".repeat(MAX_SESSION_LEN),
    )
    .unwrap();
    let r = KeyRing::new(key(&"k".repeat(MAX_NAME_LEN), 1));
    let t = sign_at(&r, &c, u64::MAX - 200, 120, Some(&req()));
    assert!(t.len() <= MAX_LEN);
    assert!(wire::parse(&t).is_ok());
}

// ---------- Verification: every rule ----------

#[test]
fn verifies_within_the_window_and_skew() {
    let r = ring();
    let t = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    for now in [NOW - 5, NOW, NOW + 59, NOW + 64] {
        assert!(verify(&r, &t, now).is_ok(), "{now}");
    }
    assert_eq!(
        verify(&r, &t, NOW - 6).unwrap_err(),
        VerifyError::NotYetValid
    );
    assert_eq!(verify(&r, &t, NOW + 65).unwrap_err(), VerifyError::Expired);
    assert_eq!(verify(&r, &t, u64::MAX).unwrap_err(), VerifyError::Expired);
}

#[test]
fn a_tighter_or_looser_skew() {
    let r = ring();
    let t = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    let strict = policy().with_clock_skew(0).unwrap();
    assert_eq!(
        r.verify(&t, &strict, NOW - 1, Some(&req()), &AcceptAll)
            .unwrap_err(),
        VerifyError::NotYetValid
    );
    assert_eq!(
        r.verify(&t, &strict, NOW + 60, Some(&req()), &AcceptAll)
            .unwrap_err(),
        VerifyError::Expired
    );
    assert!(r
        .verify(&t, &strict, NOW + 59, Some(&req()), &AcceptAll)
        .is_ok());
    let loose = policy().with_clock_skew(MAX_CLOCK_SKEW_SECS).unwrap();
    assert!(r
        .verify(&t, &loose, NOW + 89, Some(&req()), &AcceptAll)
        .is_ok());
    assert_eq!(
        policy()
            .with_clock_skew(MAX_CLOCK_SKEW_SECS + 1)
            .unwrap_err(),
        PolicyError::Skew
    );
    assert_eq!(policy().clock_skew(), DEFAULT_CLOCK_SKEW_SECS);
}

#[test]
fn wrong_audience_and_unknown_issuer() {
    let r = ring();
    let to_toolserver = Claims::new("web", "toolserver", "ana", &[Role::Pm], "s").unwrap();
    let t = sign_at(&r, &to_toolserver, NOW, 60, Some(&req()));
    assert_eq!(verify(&r, &t, NOW).unwrap_err(), VerifyError::WrongAudience);
    let from_slack = Claims::new("slack", "bots", "ana", &[Role::Pm], "s").unwrap();
    let t = sign_at(&r, &from_slack, NOW, 60, Some(&req()));
    assert_eq!(verify(&r, &t, NOW).unwrap_err(), VerifyError::UnknownIssuer);
}

#[test]
fn unknown_key_and_bad_signature() {
    let t = sign_at(&ring(), &claims(), NOW, 60, Some(&req()));
    let other_id = KeyRing::new(key("k9", 1));
    assert_eq!(
        verify(&other_id, &t, NOW).unwrap_err(),
        VerifyError::UnknownKey
    );
    let same_id_other_bytes = KeyRing::new(key("k1", 1));
    assert_eq!(
        verify(&same_id_other_bytes, &t, NOW).unwrap_err(),
        VerifyError::BadSignature
    );
}

#[test]
fn the_signature_is_checked_before_any_claim() {
    // A forgery learns nothing about which claim it got wrong.
    let r = ring();
    let t = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    let forged = with_field(&t, "aud", "toolserver");
    assert_eq!(
        verify(&r, &forged, NOW).unwrap_err(),
        VerifyError::BadSignature
    );
    let forged = with_field(&t, "iss", "evil");
    assert_eq!(
        verify(&r, &forged, NOW).unwrap_err(),
        VerifyError::BadSignature
    );
    // Expired and forged reads as forged.
    let forged = with_field(&t, "sub", "ben");
    assert_eq!(
        verify(&r, &forged, NOW + 1000).unwrap_err(),
        VerifyError::BadSignature
    );
}

#[test]
fn request_binding() {
    let r = ring();
    let bound = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    let other = RequestBinding::new("POST", "/v1/tasks", br#"{"x":1}"#).unwrap();
    let p = policy();
    assert!(r.verify(&bound, &p, NOW, Some(&req()), &AcceptAll).is_ok());
    assert_eq!(
        r.verify(&bound, &p, NOW, Some(&other), &AcceptAll)
            .unwrap_err(),
        VerifyError::RequestMismatch
    );
    assert_eq!(
        r.verify(&bound, &p, NOW, None, &AcceptAll).unwrap_err(),
        VerifyError::RequestMismatch
    );
    // Allowing unbound assertions does not relax a bound one.
    let lax = policy().allow_unbound();
    assert!(!lax.requires_binding());
    assert_eq!(
        r.verify(&bound, &lax, NOW, Some(&other), &AcceptAll)
            .unwrap_err(),
        VerifyError::RequestMismatch
    );
    let unbound = sign_at(&r, &claims(), NOW, 60, None);
    assert_eq!(
        r.verify(&unbound, &p, NOW, Some(&req()), &AcceptAll)
            .unwrap_err(),
        VerifyError::RequestMismatch
    );
    let a = r
        .verify(&unbound, &lax, NOW, Some(&req()), &AcceptAll)
        .unwrap();
    assert!(!a.is_request_bound());
    assert!(r.verify(&unbound, &lax, NOW, None, &AcceptAll).is_ok());
}

#[test]
fn replay_is_refused_and_only_valid_assertions_are_recorded() {
    let r = ring();
    let cache = MemoryReplayCache::new(10);
    let t = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    // A failing check records nothing.
    assert_eq!(
        r.verify(&t, &policy(), NOW + 1000, Some(&req()), &cache)
            .unwrap_err(),
        VerifyError::Expired
    );
    assert!(cache.is_empty());
    assert!(r.verify(&t, &policy(), NOW, Some(&req()), &cache).is_ok());
    assert_eq!(
        r.verify(&t, &policy(), NOW + 1, Some(&req()), &cache)
            .unwrap_err(),
        VerifyError::Replay
    );
    assert_eq!(cache.len(), 1);
    assert_eq!(
        r.verify(&t, &policy(), NOW, Some(&req()), &Down)
            .unwrap_err(),
        VerifyError::ReplayStoreUnavailable
    );
}

#[test]
fn the_replay_window_covers_the_skew() {
    struct Spy(std::sync::Mutex<Option<(u64, u64)>>);
    impl ReplayGuard for Spy {
        fn check_and_record(&self, _: &Nonce, until: u64, now: u64) -> Result<(), ReplayRejection> {
            *self.0.lock().unwrap() = Some((until, now));
            Ok(())
        }
    }
    let spy = Spy(Default::default());
    let r = ring();
    let t = sign_at(&r, &claims(), NOW, 60, Some(&req()));
    r.verify(&t, &policy(), NOW + 3, Some(&req()), &spy)
        .unwrap();
    assert_eq!(*spy.0.lock().unwrap(), Some((NOW + 65, NOW + 3)));
}

// ---------- Keys and rotation ----------

#[test]
fn rotation_without_downtime() {
    let old = KeyRing::new(key("k1", 1));
    let t_old = sign_at(&old, &claims(), NOW, 60, Some(&req()));
    // After rotation: k2 signs, k1 still verifies.
    let rotated = KeyRing::new(key("k2", 2))
        .with_previous(key("k1", 1))
        .unwrap();
    assert_eq!(rotated.current_id(), "k2");
    assert_eq!(rotated.previous_id(), Some("k1"));
    let t_new = sign_at(&rotated, &claims(), NOW, 60, Some(&req()));
    assert!(t_new.contains(";kid=k2;"));
    assert!(verify(&rotated, &t_old, NOW).is_ok());
    assert!(verify(&rotated, &t_new, NOW).is_ok());
    // A verifier that has not rotated yet but holds k2 as previous also accepts both.
    let staged = KeyRing::new(key("k1", 1))
        .with_previous(key("k2", 2))
        .unwrap();
    assert!(verify(&staged, &t_old, NOW).is_ok());
    assert!(verify(&staged, &t_new, NOW).is_ok());
    // Once k1 is dropped, its assertions stop verifying.
    let done = KeyRing::new(key("k2", 2));
    assert_eq!(
        verify(&done, &t_old, NOW).unwrap_err(),
        VerifyError::UnknownKey
    );
    assert!(verify(&done, &t_new, NOW).is_ok());
    assert_eq!(
        KeyRing::new(key("k1", 1))
            .with_previous(key("k1", 2))
            .unwrap_err(),
        KeyError::DuplicateId
    );
}

#[test]
fn key_rules() {
    assert_eq!(Key::new("k1", &[1; 31]).unwrap_err(), KeyError::Length);
    assert_eq!(Key::new("k1", &[]).unwrap_err(), KeyError::Length);
    assert_eq!(
        Key::new("k1", &[1; MAX_KEY_LEN + 1]).unwrap_err(),
        KeyError::Length
    );
    assert!(Key::new("k1", &[1; MIN_KEY_LEN]).is_ok());
    assert!(Key::new("k1", &[1; MAX_KEY_LEN]).is_ok());
    for id in ["", "K1", "k 1", "k;1", &"k".repeat(MAX_NAME_LEN + 1)] {
        assert_eq!(
            Key::new(id, &[1; 32]).unwrap_err(),
            KeyError::KeyId,
            "{id:?}"
        );
    }
    let b64 = STANDARD.encode([5u8; 32]);
    let a = KeyRing::new(Key::from_base64("k1", &b64).unwrap());
    let b = KeyRing::new(key("k1", 5));
    let t = sign_at(&a, &claims(), NOW, 60, Some(&req()));
    assert!(verify(&b, &t, NOW).is_ok());
    assert!(Key::from_base64("k1", &format!(" {b64}\n")).is_ok());
    assert_eq!(
        Key::from_base64("k1", "not base64!").unwrap_err(),
        KeyError::Encoding
    );
    assert_eq!(
        Key::from_base64("k1", &STANDARD.encode([5u8; 16])).unwrap_err(),
        KeyError::Length
    );
    let g1 = KeyRing::new(Key::generate("k1").unwrap());
    let g2 = KeyRing::new(Key::generate("k1").unwrap());
    let t = sign_at(&g1, &claims(), NOW, 60, Some(&req()));
    assert!(verify(&g1, &t, NOW).is_ok());
    assert_eq!(verify(&g2, &t, NOW).unwrap_err(), VerifyError::BadSignature);
}

#[test]
fn debug_output_never_contains_key_bytes() {
    let k = Key::new("k1", &KEY_BYTES).unwrap();
    let ring = KeyRing::new(Key::new("k2", &KEY_BYTES).unwrap())
        .with_previous(k)
        .unwrap();
    let hex_lower: String = KEY_BYTES.iter().map(|b| format!("{b:02x}")).collect();
    let hex_upper = hex_lower.to_uppercase();
    let decimal = format!("{:?}", KEY_BYTES);
    let decimal_inner = &decimal[1..decimal.len() - 1];
    let forms = [
        hex_lower.clone(),
        hex_upper,
        decimal_inner.to_string(),
        STANDARD.encode(KEY_BYTES),
        URL_SAFE_NO_PAD.encode(KEY_BYTES),
        String::from_utf8_lossy(&KEY_BYTES).into_owned(),
        // Any run of a few bytes, in case of partial printing.
        "0, 1, 2, 3".to_string(),
        "00010203".to_string(),
    ];
    for out in [
        format!("{ring:?}"),
        format!("{ring:#?}"),
        format!("{:?}", Key::new("k3", &KEY_BYTES).unwrap()),
    ] {
        assert!(out.contains("<redacted>"), "{out}");
        assert!(out.contains("k2") || out.contains("k3"), "{out}");
        for f in &forms {
            assert!(!out.contains(f.as_str()), "Debug leaked {f:?}");
        }
    }
    // Errors and the request binding print no material either.
    let dbg = format!("{:?}", req());
    assert!(!dbg.contains(&URL_SAFE_NO_PAD.encode(req().digest)));
}

#[test]
fn policy_rules() {
    assert_eq!(
        VerifyPolicy::new("Bots", &["web"]).unwrap_err(),
        PolicyError::Audience
    );
    assert_eq!(
        VerifyPolicy::new("bots", &[]).unwrap_err(),
        PolicyError::NoIssuers
    );
    assert_eq!(
        VerifyPolicy::new("bots", &["web", "We b"]).unwrap_err(),
        PolicyError::Issuer
    );
    let p = policy();
    assert_eq!(p.audience(), "bots");
    assert_eq!(p.issuers(), &["web".to_string(), "console".to_string()]);
    assert!(p.requires_binding());
}

#[test]
fn errors_never_echo_input() {
    let r = ring();
    let t = with_field(&base(), "sub", "secret-handle-xyz");
    let e = r
        .verify(&t, &policy(), NOW, Some(&req()), &AcceptAll)
        .unwrap_err();
    assert!(!e.to_string().contains("secret-handle-xyz"));
    let e = r
        .verify("v=1;kid=leaky-kid", &policy(), NOW, None, &AcceptAll)
        .unwrap_err();
    assert!(!e.to_string().contains("leaky"), "{e}");
}

// ---------- Property-style: round trips and tampering ----------

/// xorshift64*: deterministic, so a failure reproduces.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick(&mut self, alphabet: &[u8]) -> char {
        alphabet[self.below(alphabet.len() as u64) as usize] as char
    }
    fn word(&mut self, first: &[u8], rest: &[u8], max: u64) -> String {
        let len = 1 + self.below(max);
        let mut s = String::new();
        s.push(self.pick(first));
        for _ in 1..len {
            s.push(self.pick(rest));
        }
        s
    }
}

const LOWER_DIGIT: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const NAME_REST: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789._-";
const SESSION: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

fn random_case(rng: &mut Rng) -> (Claims, u64, u64, Option<RequestBinding>, Nonce) {
    let roles: Vec<Role> = Role::KNOWN
        .iter()
        .filter(|_| rng.below(2) == 0)
        .map(|r| Role::parse(r).unwrap())
        .collect();
    let claims = Claims::new(
        &rng.word(LOWER_DIGIT, NAME_REST, MAX_NAME_LEN as u64),
        &rng.word(LOWER_DIGIT, NAME_REST, MAX_NAME_LEN as u64),
        &rng.word(LOWER_DIGIT, NAME_REST, 64),
        &roles,
        &rng.word(SESSION, SESSION, MAX_SESSION_LEN as u64),
    )
    .unwrap();
    let now = match rng.below(4) {
        0 => rng.below(10),
        1 => u64::MAX - 200 - rng.below(1000),
        _ => rng.next() >> 1,
    };
    let ttl = 1 + rng.below(MAX_LIFETIME_SECS);
    let binding = (rng.below(3) != 0).then(|| {
        let body: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
        let path = format!("/{}", rng.word(LOWER_DIGIT, NAME_REST, 40));
        RequestBinding::new(
            ["GET", "POST", "PUT", "DELETE"][rng.below(4) as usize],
            &path,
            &body,
        )
        .unwrap()
    });
    let mut n = [0u8; NONCE_LEN];
    n.iter_mut().for_each(|b| *b = rng.next() as u8);
    (claims, now, ttl, binding, Nonce(n))
}

#[test]
fn property_sign_parse_serialise_round_trip() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let ring = KeyRing::new(Key::new("k1", &KEY_BYTES).unwrap());
    for _ in 0..2000 {
        let (c, now, ttl, b, nonce) = random_case(&mut rng);
        let t = ring
            .sign_with_nonce(&c, now, ttl, b.as_ref(), nonce)
            .unwrap();
        let parsed = wire::parse(&t).unwrap();
        let a = &parsed.assertion;
        assert_eq!(a.signed_part(), parsed.signed);
        assert_eq!(
            format!(
                "{};sig={}",
                parsed.signed,
                URL_SAFE_NO_PAD.encode(parsed.signature)
            ),
            t
        );
        assert_eq!(
            (
                a.issuer(),
                a.audience(),
                a.subject(),
                a.roles(),
                a.session()
            ),
            (
                c.issuer(),
                c.audience(),
                c.subject(),
                c.roles(),
                c.session()
            )
        );
        assert_eq!(
            (a.issued_at(), a.expires_at(), a.nonce()),
            (now, now + ttl, &nonce)
        );
        assert_eq!(a.request, b.map(|b| b.digest));
        let policy = VerifyPolicy::new(c.audience(), &[c.issuer()])
            .unwrap()
            .allow_unbound();
        let v = ring
            .verify(&t, &policy, now, b.as_ref(), &AcceptAll)
            .unwrap();
        assert_eq!(&v, a);
    }
}

#[test]
fn property_any_single_byte_change_is_refused() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let ring = KeyRing::new(Key::new("k1", &KEY_BYTES).unwrap());
    let replacements: Vec<u8> = (0x20u8..=0x7e).chain([0x00, 0x0a, 0x80, 0xff]).collect();
    for _ in 0..4 {
        let (c, now, ttl, b, nonce) = random_case(&mut rng);
        let t = ring
            .sign_with_nonce(&c, now, ttl, b.as_ref(), nonce)
            .unwrap();
        let policy = VerifyPolicy::new(c.audience(), &[c.issuer()])
            .unwrap()
            .allow_unbound();
        let check = |bytes: &[u8]| {
            // Non-UTF-8 cannot even reach the header parser as &str; refuse it like the
            // service would.
            let Ok(s) = std::str::from_utf8(bytes) else {
                return;
            };
            if s == t {
                return;
            }
            assert!(
                ring.verify(s, &policy, now, b.as_ref(), &AcceptAll)
                    .is_err(),
                "tampered token verified: {s}"
            );
        };
        let original = t.as_bytes();
        for i in 0..original.len() {
            for &r in &replacements {
                let mut m = original.to_vec();
                if m[i] == r {
                    continue;
                }
                m[i] = r;
                check(&m);
            }
            let mut deleted = original.to_vec();
            deleted.remove(i);
            check(&deleted);
            let mut inserted = original.to_vec();
            inserted.insert(i, original[i]);
            check(&inserted);
        }
        // And a stray byte at the end.
        check(format!("{t}A").as_bytes());
    }
}
