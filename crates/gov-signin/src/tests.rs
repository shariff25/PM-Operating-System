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
async fn a_member_token_names_oid_tid_and_when_they_signed_in() {
    let idp = FakeIdp::start(TENANT).await;
    let v = Verifier::new(tenant(&idp)).unwrap();
    let now = Utc::now();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    let id = v.validate(&token, NONCE, now).await.unwrap();
    assert_eq!(id.oid, OID);
    assert_eq!(id.tid, TENANT);
    assert_eq!(id.issued_at, now.timestamp());
    assert_eq!(id.auth_time, now.timestamp());
    // Debug never shows who.
    let shown = format!("{id:?}");
    assert!(!shown.contains(OID), "{shown}");
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
        (
            "no acct",
            good(&without("acct")),
            Failure::Misconfigured("no acct claim"),
        ),
        (
            "no auth_time",
            good(&without("auth_time")),
            Failure::Misconfigured("no auth_time claim"),
        ),
        (
            "auth_time ahead of the clock",
            good(&with("auth_time", json!(t + SKEW_SECONDS + 1))),
            Failure::Failed("token authenticated in the future"),
        ),
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
        // The re-check of eb24d28's missing tests. A v1-format token (issuer
        // `https://sts.windows.net/<tid>/`, `ver` 1.0) for this very tenant and app: refused,
        // only the v2.0 issuer is accepted.
        (
            "a v1 token (sts.windows.net issuer)",
            good(&{
                let mut c = with("iss", json!(format!("https://sts.windows.net/{TENANT}/")));
                c["ver"] = json!("1.0");
                c
            }),
            Failure::Failed("token issuer"),
        ),
        // `idp` naming another tenant, in either issuer form: someone from that tenant, here
        // as a guest-like account even with `acct` 0.
        (
            "idp names another tenant (v1 form)",
            good(&with(
                "idp",
                json!(format!("https://sts.windows.net/{OTHER_TENANT}/")),
            )),
            Failure::NotMember("another identity provider"),
        ),
        (
            "idp names another tenant (v2 form)",
            good(&with(
                "idp",
                json!(format!("{}/{OTHER_TENANT}/v2.0", idp.base)),
            )),
            Failure::NotMember("another identity provider"),
        ),
        // A token Entra issued to another application in the same tenant, for a real member
        // with an otherwise valid identity (signature, issuer, tenant, oid, acct, nonce all
        // right): its `aud` is that app's client id, so it is refused.
        (
            "another app in the same tenant",
            good(&{
                let mut c = with("aud", json!("9d2b6f1e-aaaa-4bbb-8ccc-ddddeeeeffff"));
                c["azp"] = json!("9d2b6f1e-aaaa-4bbb-8ccc-ddddeeeeffff");
                c
            }),
            Failure::Failed("token audience"),
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

/// LC: a refresh of the keys is one fetch however many requests want it at once, runs
/// outside the cache's lock (a token under a known key is checked at once meanwhile), and ends
/// within the fetch timeout when the tenant is slow, inside the caller's budget.
#[tokio::test]
async fn key_refreshes_are_coalesced_bounded_and_never_block_known_keys() {
    let idp = FakeIdp::start(TENANT).await;
    let mut cfg = tenant(&idp);
    cfg.jwks_refetch_after = StdDuration::from_millis(200);
    let v = std::sync::Arc::new(Verifier::new(cfg).unwrap());
    let now = Utc::now();
    let c = claims(&idp, now.timestamp());
    let known = sign(&c, keys::KEY_A_DER, "key-a");
    assert!(v.validate(&known, NONCE, now).await.is_ok());
    assert_eq!(idp.jwks_hits(), 1);
    // The tenant rotates to key B, slowly: twenty requests for it at once make one fetch.
    idp.set_keys(vec![
        jwk("key-a", keys::KEY_A_N),
        jwk("key-b", keys::KEY_B_N),
    ]);
    idp.set_jwks_delay(StdDuration::from_millis(500));
    tokio::time::sleep(StdDuration::from_millis(250)).await;
    let rotated = sign(&c, keys::KEY_B_DER, "key-b");
    let mut waiting = Vec::new();
    for _ in 0..20 {
        let (v, t) = (v.clone(), rotated.clone());
        waiting.push(tokio::spawn(
            async move { v.validate(&t, NONCE, now).await },
        ));
    }
    // Meanwhile a token under the known key isn't held up by the fetch.
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    let started = Instant::now();
    assert!(v.validate(&known, NONCE, now).await.is_ok());
    assert!(
        started.elapsed() < StdDuration::from_millis(250),
        "a known key waited for the fetch: {:?}",
        started.elapsed()
    );
    for w in waiting {
        assert!(w.await.unwrap().is_ok());
    }
    assert_eq!(idp.jwks_hits(), 2, "one fetch for twenty requests");
    // A tenant slower than the budget: the fetch gives up within FETCH_TIMEOUT, fails closed.
    idp.set_jwks_delay(StdDuration::from_secs(8));
    tokio::time::sleep(StdDuration::from_millis(250)).await;
    let started = Instant::now();
    let unknown = sign(&c, keys::KEY_B_DER, "key-c");
    assert_eq!(
        v.validate(&unknown, NONCE, now).await,
        Err(Failure::Unavailable("keys unavailable"))
    );
    let took = started.elapsed();
    assert!(
        took < FETCH_TIMEOUT + StdDuration::from_millis(500) && took < StdDuration::from_secs(5),
        "{took:?}"
    );
    assert!(
        v.validate(&known, NONCE, now).await.is_ok(),
        "known keys still work"
    );
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

/// The keys are fetched from the configured authority and nowhere else: not from an address
/// on another origin that the discovery document names, not by following a redirect, and not
/// past the size a response may have.
#[tokio::test]
async fn keys_come_only_from_the_authority_and_within_the_size_limit() {
    let now = Utc::now();
    let elsewhere = FakeIdp::start(TENANT).await;

    let idp = FakeIdp::start(TENANT).await;
    idp.set_keys_at(&elsewhere.keys_url());
    let v = Verifier::new(tenant(&idp)).unwrap();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    assert_eq!(
        v.validate(&token, NONCE, now).await,
        Err(Failure::Unavailable(
            "discovery endpoints off the authority"
        ))
    );

    let idp = FakeIdp::start(TENANT).await;
    idp.set_keys_moved_to(&elsewhere.keys_url());
    let v = Verifier::new(tenant(&idp)).unwrap();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    assert_eq!(
        v.validate(&token, NONCE, now).await,
        Err(Failure::Unavailable("keys unavailable"))
    );
    assert_eq!(elsewhere.jwks_hits(), 0);

    let idp = FakeIdp::start(TENANT).await;
    idp.set_keys_padding(MAX_RESPONSE);
    let v = Verifier::new(tenant(&idp)).unwrap();
    let token = sign(&claims(&idp, now.timestamp()), keys::KEY_A_DER, "key-a");
    assert_eq!(
        v.validate(&token, NONCE, now).await,
        Err(Failure::Unavailable("keys unavailable"))
    );
}
