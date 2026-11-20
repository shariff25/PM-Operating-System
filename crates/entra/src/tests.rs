use super::testing::{jwk, keys, member_claims, sign, unsigned, FakeIdp};
use super::*;
use serde_json::{json, Value};

const TENANT: &str = "72f988bf-0000-4000-8000-0000000000aa";
const OTHER_TENANT: &str = "72f988bf-0000-4000-8000-0000000000bb";
const CLIENT: &str = "0b7f0c6e-1111-4222-8333-444455556666";
const OID: &str = "00000000-0000-0000-0000-00000000a0a0";
const NONCE: &str = "the-sign-in-nonce";

fn tenant(idp: &FakeIdp) -> Tenant {
    Tenant::from_settings(["T", "C", "A"], TENANT, CLIENT, Some(&idp.base)).unwrap()
}

fn claims(idp: &FakeIdp, now: i64) -> Value {
    member_claims(
        &idp.base,
        TENANT,
        CLIENT,
        OID,
        "Ana@Contoso.example",
        NONCE,
        now,
    )
}

#[test]
fn settings_are_checked() {
    let vars = ["TENANT", "CLIENT", "AUTHORITY"];
    let ok = Tenant::from_settings(vars, &TENANT.to_uppercase(), CLIENT, None).unwrap();
    assert_eq!(ok.tenant_id, TENANT);
    assert_eq!(ok.authority, DEFAULT_AUTHORITY);
    assert_eq!(
        ok.issuer(),
        format!("https://login.microsoftonline.com/{TENANT}/v2.0")
    );
    for bad in ["common", "organizations", "Consumers"] {
        let err = Tenant::from_settings(vars, bad, CLIENT, None).unwrap_err();
        assert!(
            err.contains("multi-tenant") && err.contains("TENANT"),
            "{err}"
        );
    }
    assert!(Tenant::from_settings(vars, TENANT, "my-app", None)
        .unwrap_err()
        .contains("CLIENT must be a GUID"));
    for (authority, want) in [
        ("http://idp.example.com", "https unless"),
        ("https://idp.example.com/tenant", "no path"),
        ("not a url", "must be a URL"),
    ] {
        let err = Tenant::from_settings(vars, TENANT, CLIENT, Some(authority)).unwrap_err();
        assert!(err.contains(want), "{err}");
    }
    let t = Tenant::from_settings(vars, TENANT, CLIENT, Some("http://127.0.0.1:9/")).unwrap();
    assert_eq!(t.authority, "http://127.0.0.1:9");
}

#[tokio::test]
async fn a_member_token_names_oid_tid_and_the_sign_in_name() {
    let idp = FakeIdp::start(TENANT).await;
    let v = Verifier::new(tenant(&idp)).unwrap();
    let now = Utc::now();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    let id = v.validate(&token, NONCE, now).await.unwrap();
    assert_eq!(id.oid, OID);
    assert_eq!(id.tid, TENANT);
    assert_eq!(
        id.preferred_username.as_deref(),
        Some("ana@contoso.example")
    );
    assert_eq!(id.issued_at, now.timestamp());
    // Debug never shows who.
    let shown = format!("{id:?}");
    assert!(!shown.contains(OID) && !shown.contains("ana@"), "{shown}");
}

/// Every refusal: forged, unsigned, another algorithm, wrong `aud`, `iss`, `tid`, expired,
/// not yet valid, wrong nonce, no `oid`, a guest, no `acct`, another `idp`.
#[tokio::test]
async fn every_bad_token_is_refused() {
    let idp = FakeIdp::start(TENANT).await;
    let v = Verifier::new(tenant(&idp)).unwrap();
    let now = Utc::now();
    let t = now.timestamp();
    let base = claims(&idp, t);
    let with = |k: &str, val: Value| {
        let mut c = base.clone();
        c[k] = val;
        c
    };
    let without = |k: &str| {
        let mut c = base.clone();
        c.as_object_mut().unwrap().remove(k);
        c
    };
    let good = |c: &Value| sign(c, keys::KEY_A_DER, "key-a");
    let cases: Vec<(&str, String, Failure)> = vec![
        // Signed with a key the tenant doesn't publish, under its published kid.
        (
            "forged",
            sign(&base, keys::KEY_B_DER, "key-a"),
            Failure::Failed("token signature or shape"),
        ),
        (
            "unsigned",
            unsigned(&base, "key-a"),
            Failure::Failed("malformed token"),
        ),
        (
            "HS256 with the modulus as secret",
            {
                let mut h = jsonwebtoken::Header::new(Algorithm::HS256);
                h.kid = Some("key-a".into());
                jsonwebtoken::encode(
                    &h,
                    &base,
                    &jsonwebtoken::EncodingKey::from_secret(keys::KEY_A_N.as_bytes()),
                )
                .unwrap()
            },
            Failure::Failed("token not RS256"),
        ),
        (
            "wrong aud",
            good(&with("aud", json!("aaaaaaaa-0000-0000-0000-000000000000"))),
            Failure::Failed("token audience"),
        ),
        (
            "two audiences",
            good(&with("aud", json!([CLIENT, "another"]))),
            Failure::Failed("token audience"),
        ),
        (
            "wrong iss",
            good(&with(
                "iss",
                json!(format!("{}/{OTHER_TENANT}/v2.0", idp.base)),
            )),
            Failure::Failed("token issuer"),
        ),
        (
            "wrong tid",
            good(&with("tid", json!(OTHER_TENANT))),
            Failure::Failed("token tenant"),
        ),
        (
            "no tid",
            good(&without("tid")),
            Failure::Failed("token tenant"),
        ),
        (
            "expired",
            good(&with("exp", json!(t - SKEW_SECONDS - 1))),
            Failure::Failed("token expired"),
        ),
        (
            "not yet valid",
            good(&with("nbf", json!(t + SKEW_SECONDS + 5))),
            Failure::Failed("token not yet valid"),
        ),
        (
            "issued in the future",
            good(&with("iat", json!(t + SKEW_SECONDS + 5))),
            Failure::Failed("token issued in the future"),
        ),
        (
            "another nonce",
            good(&with("nonce", json!("another"))),
            Failure::Failed("token nonce"),
        ),
        (
            "no nonce",
            good(&without("nonce")),
            Failure::Failed("token nonce"),
        ),
        (
            "no oid",
            good(&without("oid")),
            Failure::Failed("token without an object id"),
        ),
        (
            "oid not a GUID",
            good(&with("oid", json!("ana"))),
            Failure::Failed("token without an object id"),
        ),
        (
            "a guest",
            good(&with("acct", json!(1))),
            Failure::NotMember("a guest account"),
        ),
        (
            "a guest, as a string",
            good(&with("acct", json!("1"))),
            Failure::NotMember("a guest account"),
        ),
        ("no acct", good(&without("acct")), Failure::Misconfigured),
        (
            "another identity provider",
            good(&with("idp", json!("live.com"))),
            Failure::NotMember("another identity provider"),
        ),
        (
            "unknown kid",
            sign(&base, keys::KEY_A_DER, "key-z"),
            Failure::Failed("unknown signing key"),
        ),
    ];
    for (name, token, want) in cases {
        assert_eq!(v.validate(&token, NONCE, now).await, Err(want), "{name}");
    }
    // An empty expected nonce never matches.
    let token = good(&with("nonce", json!("")));
    assert!(v.validate(&token, "", now).await.is_err());
    // A member whose idp names this tenant (either issuer form) is fine.
    for idp_value in [
        format!("{}/{TENANT}/v2.0", idp.base),
        format!("https://sts.windows.net/{TENANT}/"),
    ] {
        let token = good(&with("idp", json!(idp_value)));
        assert!(v.validate(&token, NONCE, now).await.is_ok(), "{idp_value}");
    }
    // And `acct` as the string "0".
    assert!(v
        .validate(&good(&with("acct", json!("0"))), NONCE, now)
        .await
        .is_ok());
}

/// A rotated key: a token under a `kid` the cache doesn't have refetches the keys and passes
/// once the tenant publishes it. Attacker-chosen `kid`s refetch at most once per interval.
#[tokio::test]
async fn an_unknown_kid_refetches_at_most_once_per_interval() {
    let idp = FakeIdp::start(TENANT).await;
    let mut cfg = tenant(&idp);
    cfg.jwks_refetch_after = StdDuration::from_millis(300);
    let v = Verifier::new(cfg).unwrap();
    let now = Utc::now();
    let c = claims(&idp, now.timestamp());
    assert!(v
        .validate(&sign(&c, keys::KEY_A_DER, "key-a"), NONCE, now)
        .await
        .is_ok());
    assert_eq!(idp.jwks_hits(), 1);
    // Many unknown kids within the interval: no more fetches.
    for i in 0..20 {
        let token = sign(&c, keys::KEY_B_DER, &format!("junk-{i}"));
        assert_eq!(
            v.validate(&token, NONCE, now).await,
            Err(Failure::Failed("unknown signing key"))
        );
    }
    assert_eq!(idp.jwks_hits(), 1);
    // The tenant rotates to key B; after the interval, a token signed with it refetches.
    idp.set_keys(vec![
        jwk("key-a", keys::KEY_A_N),
        jwk("key-b", keys::KEY_B_N),
    ]);
    tokio::time::sleep(StdDuration::from_millis(350)).await;
    let rotated = sign(&c, keys::KEY_B_DER, "key-b");
    assert!(v.validate(&rotated, NONCE, now).await.is_ok());
    assert_eq!(idp.jwks_hits(), 2);
    // Known keys need no fetch.
    assert!(v.validate(&rotated, NONCE, now).await.is_ok());
    assert_eq!(idp.jwks_hits(), 2);
}

/// The tenant's endpoints down: nothing validates (fail closed), and a stream of tokens
/// doesn't become a stream of fetches.
#[tokio::test]
async fn jwks_down_fails_closed() {
    let idp = FakeIdp::start(TENANT).await;
    idp.set_down(true);
    let v = Verifier::new(tenant(&idp)).unwrap();
    let now = Utc::now();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    for _ in 0..10 {
        assert!(matches!(
            v.validate(&token, NONCE, now).await,
            Err(Failure::Unavailable(_))
        ));
    }
    assert!(idp.discovery_hits() <= 1, "{}", idp.discovery_hits());
    assert_eq!(idp.jwks_hits(), 0);
    // Up again: after the backoff it works.
    idp.set_down(false);
    tokio::time::sleep(FAILED_FETCH_BACKOFF + StdDuration::from_millis(100)).await;
    assert!(v.validate(&token, NONCE, now).await.is_ok());
}

#[test]
fn a_token_is_used_once_within_its_lifetime() {
    let used = UsedTokens::default();
    let now = Utc::now();
    let id = Identity {
        oid: OID.into(),
        tid: TENANT.into(),
        preferred_username: None,
        issued_at: now.timestamp(),
        expires_at: now.timestamp() + 3600,
    };
    assert!(used.first_use(&id, now));
    assert!(!used.first_use(&id, now));
    // Another token for the same person (a later sign-in) is its own.
    let later = Identity {
        issued_at: id.issued_at + 1,
        ..id.clone()
    };
    assert!(used.first_use(&later, now));
    // Forgotten once expired (it would fail the expiry check anyway).
    let after = now + chrono::Duration::seconds(3600 + SKEW_SECONDS + 1);
    assert!(used.first_use(&id, after));
    assert_eq!(used.len(), 1);
}
