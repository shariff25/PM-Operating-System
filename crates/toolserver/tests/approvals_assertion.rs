//! The approvals routes for the services that act for a person (`pmos-web`, the console):
//! their own tokens, and their user assertions in each `PMOS_ASSERTION_MODE`
//! (`docs/user-assertion.md`). The orchestrator and the Slack bot are never affected.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::Fixture;
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::connectors::fake::{SLACK_ALL_TEAM, SLACK_PM, SLACK_WITH_OUTSIDER};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test keys only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const CONSOLE_KEY: &str = "Y29uc29sZS10b29sc2VydmVyLXRlc3Qta2V5LTAxMjM=";
const OTHER: &str = "ben";

struct Server {
    base: String,
    cancel: CancellationToken,
    /// The PM's pending item and another PM's.
    mine: String,
    theirs: String,
    _f: Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn assertions(mode: Mode) -> Assertions {
    let keys = parse_keys(
        &format!("web:toolserver:w1:{WEB_KEY},console:toolserver:c1:{CONSOLE_KEY}"),
        "the tool server",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
    Assertions::new(mode, keys).unwrap()
}

/// A pending Slack draft of `user`'s.
async fn draft(f: &Fixture, user: &str, run: &str) -> String {
    let at = Utc::now();
    let s = f
        .svc
        .create_session(user, "reply-test", run, Some(Duration::hours(1)), at)
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Yes."}),
            at,
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

async fn start(mode: Mode) -> Server {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    let mine = draft(&f, SLACK_PM, "tsk_mine").await;
    let theirs = draft(&f, OTHER, "tsk_theirs").await;
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: Some(CONSOLE_TOKEN.into()),
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        None,
        PersonClients {
            web_token: Some(WEB_TOKEN.into()),
            assertions: Arc::new(assertions(mode)),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    Server {
        base,
        cancel,
        mine,
        theirs,
        _f: f,
    }
}

struct Signer {
    ring: KeyRing,
    issuer: &'static str,
    audience: &'static str,
}

impl Signer {
    fn web() -> Self {
        Self {
            ring: KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap()),
            issuer: "web",
            audience: "toolserver",
        }
    }

    fn console() -> Self {
        Self {
            ring: KeyRing::new(Key::from_base64("c1", CONSOLE_KEY).unwrap()),
            issuer: "console",
            audience: "toolserver",
        }
    }

    fn sign_at(&self, sub: &str, method: &str, target: &str, body: &[u8], now: u64) -> String {
        let claims = Claims::new(self.issuer, self.audience, sub, &[Role::Pm], "ses_t").unwrap();
        let req = RequestBinding::new(method, target, body).unwrap();
        self.ring.sign(&claims, now, 60, Some(&req)).unwrap()
    }

    fn sign(&self, sub: &str, method: &str, target: &str, body: &[u8]) -> String {
        self.sign_at(sub, method, target, body, unix_now())
    }
}

impl Server {
    async fn send(
        &self,
        method: Method,
        target: &str,
        token: &str,
        body: Option<&[u8]>,
        assertion: Option<&str>,
    ) -> (u16, Value) {
        let mut r = reqwest::Client::new()
            .request(method, format!("{}{target}", self.base))
            .bearer_auth(token);
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_vec());
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, target: &str, token: &str, assertion: Option<&str>) -> (u16, Value) {
        self.send(Method::GET, target, token, None, assertion).await
    }
}

fn list_target(user: &str) -> String {
    format!("/internal/approvals?user={user}&status=pending")
}

fn reject(by: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"decided_by": by, "decision": "reject"})).unwrap()
}

fn refused(status: u16, body: &Value) {
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["code"], "bad_assertion", "{body}");
}

#[tokio::test]
async fn a_valid_assertion_is_accepted_on_every_approvals_route() {
    for mode in [Mode::Verify, Mode::Require] {
        let srv = start(mode).await;
        let web = Signer::web();
        let list = list_target(SLACK_PM);
        let a = web.sign(SLACK_PM, "GET", &list, b"");
        let (s, v) = srv.get(&list, WEB_TOKEN, Some(&a)).await;
        assert_eq!(s, 200, "{mode:?}: {v}");
        assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
        // One item: only the subject's own, named with `?user=`.
        let read = format!("/internal/approvals/{}?user={SLACK_PM}", srv.mine);
        let a = web.sign(SLACK_PM, "GET", &read, b"");
        let (s, v) = srv.get(&read, WEB_TOKEN, Some(&a)).await;
        assert_eq!(s, 200, "{v}");
        let other = format!("/internal/approvals/{}?user={SLACK_PM}", srv.theirs);
        let a = web.sign(SLACK_PM, "GET", &other, b"");
        let (s, _) = srv.get(&other, WEB_TOKEN, Some(&a)).await;
        assert_eq!(s, 404);
        let sources = format!("/internal/approvals/{}/sources?user={SLACK_PM}", srv.mine);
        let a = web.sign(SLACK_PM, "GET", &sources, b"");
        let (s, v) = srv.get(&sources, WEB_TOKEN, Some(&a)).await;
        assert_eq!(s, 200, "{v}");
        let decide = format!("/internal/approvals/{}/decision", srv.mine);
        let body = reject(SLACK_PM);
        let a = web.sign(SLACK_PM, "POST", &decide, &body);
        let (s, v) = srv
            .send(Method::POST, &decide, WEB_TOKEN, Some(&body), Some(&a))
            .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["status"], "rejected");
        assert_eq!(v["decided_by"], SLACK_PM);
        // The console, with its own token and key.
        let list = list_target(OTHER);
        let a = Signer::console().sign(OTHER, "GET", &list, b"");
        let (s, v) = srv.get(&list, CONSOLE_TOKEN, Some(&a)).await;
        assert_eq!(s, 200, "{v}");
    }
}

#[tokio::test]
async fn a_missing_assertion_is_refused_in_require_and_accepted_in_verify_and_off() {
    for (mode, want) in [(Mode::Off, 200), (Mode::Verify, 200), (Mode::Require, 401)] {
        let srv = start(mode).await;
        let list = list_target(SLACK_PM);
        for token in [WEB_TOKEN, CONSOLE_TOKEN] {
            let (s, v) = srv.get(&list, token, None).await;
            assert_eq!(s, want, "{mode:?}: {v}");
        }
        let decide = format!("/internal/approvals/{}/decision", srv.mine);
        let (s, v) = srv
            .send(
                Method::POST,
                &decide,
                WEB_TOKEN,
                Some(&reject(SLACK_PM)),
                None,
            )
            .await;
        assert_eq!(s, want, "{mode:?}: {v}");
        // A single item from the web token names its PM, so it needs one too.
        let read = format!("/internal/approvals/{}?user={SLACK_PM}", srv.mine);
        let (s, v) = srv.get(&read, WEB_TOKEN, None).await;
        assert_eq!(s, want, "{mode:?}: {v}");
        // From the console's read-only token a single item may name nobody, but under
        // `require` it still needs an assertion (W3); in `off` and `verify` it reads without.
        let read = format!("/internal/approvals/{}", srv.mine);
        let (s, v) = srv.get(&read, CONSOLE_TOKEN, None).await;
        assert_eq!(s, want, "{mode:?}: {v}");
        if want == 401 {
            refused(s, &v);
        }
    }
}

#[tokio::test]
async fn bad_assertions_are_refused_alike() {
    for mode in [Mode::Verify, Mode::Require] {
        let srv = start(mode).await;
        let web = Signer::web();
        let list = list_target(SLACK_PM);
        let now = unix_now();
        let cases = [
            // Expired.
            web.sign_at(SLACK_PM, "GET", &list, b"", now - 120),
            // Wrong audience: a runtime assertion.
            Signer {
                audience: "bots",
                ..Signer::web()
            }
            .sign(SLACK_PM, "GET", &list, b""),
            // Wrong issuer: the console's, with the web token.
            Signer::console().sign(SLACK_PM, "GET", &list, b""),
            // Wrong issuer: the web key claiming to be the console.
            Signer {
                issuer: "console",
                ..Signer::web()
            }
            .sign(SLACK_PM, "GET", &list, b""),
            // Tampered.
            web.sign(SLACK_PM, "GET", &list, b"")
                .replace("roles=pm", "roles=pm,admin"),
            // Request mismatch: another query.
            web.sign(SLACK_PM, "GET", "/internal/approvals?user=evals", b""),
            "junk".to_string(),
        ];
        for a in cases {
            let (s, v) = srv.get(&list, WEB_TOKEN, Some(&a)).await;
            refused(s, &v);
        }
        // Replayed: once, then refused.
        let once = web.sign(SLACK_PM, "GET", &list, b"");
        let (s, _) = srv.get(&list, WEB_TOKEN, Some(&once)).await;
        assert_eq!(s, 200);
        let (s, v) = srv.get(&list, WEB_TOKEN, Some(&once)).await;
        refused(s, &v);
        // A decision bound to another body.
        let decide = format!("/internal/approvals/{}/decision", srv.mine);
        let a = web.sign(SLACK_PM, "POST", &decide, &reject(SLACK_PM)[1..]);
        let (s, v) = srv
            .send(
                Method::POST,
                &decide,
                WEB_TOKEN,
                Some(&reject(SLACK_PM)),
                Some(&a),
            )
            .await;
        refused(s, &v);
    }
}

#[tokio::test]
async fn the_subject_must_be_the_person_named() {
    let srv = start(Mode::Require).await;
    let web = Signer::web();
    let list = list_target(OTHER);
    let a = web.sign(SLACK_PM, "GET", &list, b"");
    let (s, v) = srv.get(&list, WEB_TOKEN, Some(&a)).await;
    refused(s, &v);
    let sources = format!("/internal/approvals/{}/sources?user={OTHER}", srv.theirs);
    let a = web.sign(SLACK_PM, "GET", &sources, b"");
    let (s, v) = srv.get(&sources, WEB_TOKEN, Some(&a)).await;
    refused(s, &v);
    // Deciding someone else's item, or as someone else.
    let decide = format!("/internal/approvals/{}/decision", srv.theirs);
    let body = reject(OTHER);
    let a = web.sign(SLACK_PM, "POST", &decide, &body);
    let (s, v) = srv
        .send(Method::POST, &decide, WEB_TOKEN, Some(&body), Some(&a))
        .await;
    refused(s, &v);
    // Nothing was decided.
    let read = format!("/internal/approvals/{}", srv.theirs);
    let (_, item) = srv.get(&read, SERVICE_TOKEN, None).await;
    assert_eq!(item["status"], "pending");
}

#[tokio::test]
async fn the_orchestrator_and_the_slack_bot_are_unaffected_in_every_mode() {
    for mode in [Mode::Off, Mode::Verify, Mode::Require] {
        let srv = start(mode).await;
        let list = list_target(SLACK_PM);
        for a in [None, Some("junk")] {
            let (s, v) = srv.get(&list, SERVICE_TOKEN, a).await;
            assert_eq!(s, 200, "{mode:?}: {v}");
            let (s, v) = srv.get(&list, BOT_TOKEN, a).await;
            assert_eq!(s, 200, "{mode:?}: {v}");
            // The Slack bot names the item's PM (S2); without it, `404`.
            let read = format!("/internal/approvals/{}?user={OTHER}", srv.theirs);
            let (s, _) = srv.get(&read, BOT_TOKEN, a).await;
            assert_eq!(s, 200);
            let read = format!("/internal/approvals/{}", srv.theirs);
            let (s, _) = srv.get(&read, BOT_TOKEN, a).await;
            assert_eq!(s, 404);
        }
        // Sources stay closed to the Slack bot.
        let sources = format!("/internal/approvals/{}/sources?user={SLACK_PM}", srv.mine);
        let (s, _) = srv.get(&sources, BOT_TOKEN, None).await;
        assert_eq!(s, 403);
        let (s, _) = srv.get(&sources, SERVICE_TOKEN, None).await;
        assert_eq!(s, 200);
        // The Slack bot decides from its approval cards, without an assertion.
        let decide = format!("/internal/approvals/{}/decision", srv.mine);
        let (s, v) = srv
            .send(
                Method::POST,
                &decide,
                BOT_TOKEN,
                Some(&reject(SLACK_PM)),
                None,
            )
            .await;
        assert_eq!(s, 200, "{mode:?}: {v}");
    }
}

#[tokio::test]
async fn person_tokens_reach_only_the_approvals_routes() {
    let srv = start(Mode::Off).await;
    for token in [WEB_TOKEN, CONSOLE_TOKEN] {
        let body = serde_json::to_vec(
            &json!({"user": SLACK_PM, "skill": "reply-test", "run_id": "tsk_x"}),
        )
        .unwrap();
        let (s, _) = srv
            .send(Method::POST, "/internal/sessions", token, Some(&body), None)
            .await;
        assert_eq!(s, 403);
        let (s, _) = srv.get("/internal/credentials", token, None).await;
        assert_eq!(s, 403);
        let (s, _) = srv
            .get(
                &format!("/internal/onboarding?user={SLACK_PM}"),
                token,
                None,
            )
            .await;
        assert_eq!(s, 403);
    }
    // An unknown token is still a 401.
    let (s, _) = srv
        .get(
            &list_target(SLACK_PM),
            "not-a-token-0123456789abcdef0123",
            None,
        )
        .await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn the_web_token_has_at_least_the_slack_bots_limits() {
    let srv = start(Mode::Off).await;
    // A read of one item must name the item's PM (S2): without `user`, or with another
    // member, it is `404`, as a missing item is.
    for read in [
        format!("/internal/approvals/{}", srv.mine),
        format!("/internal/approvals/{}?user=", srv.mine),
        format!("/internal/approvals/{}?user={OTHER}", srv.mine),
        format!("/internal/approvals/{}?user={SLACK_PM}", srv.theirs),
        format!("/internal/approvals/apr_nope?user={SLACK_PM}"),
    ] {
        let (s, v) = srv.get(&read, WEB_TOKEN, None).await;
        assert_eq!(s, 404, "{read}: {v}");
    }
    let (s, v) = srv
        .get(
            &format!("/internal/approvals/{}?user={SLACK_PM}", srv.mine),
            WEB_TOKEN,
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user"], SLACK_PM);
    // No ledger (so no approval ids from it), and no member decisions (so no `admin` grant).
    let (s, _) = srv.get("/internal/ledger", WEB_TOKEN, None).await;
    assert_eq!(s, 403);
    let (s, _) = srv.get("/internal/members/requests", WEB_TOKEN, None).await;
    assert_eq!(s, 403);
    let grant = serde_json::to_vec(&json!({
        "decision": "approve", "decided_by": SLACK_PM, "handle": "newbie", "roles": ["admin"]
    }))
    .unwrap();
    let (s, _) = srv
        .send(
            Method::POST,
            "/internal/members/requests/req_1/decision",
            WEB_TOKEN,
            Some(&grant),
            None,
        )
        .await;
    assert_eq!(s, 403);
    // An edit may change what a draft says, never where it goes (S2).
    let decide = format!("/internal/approvals/{}/decision", srv.mine);
    let edit = |args: Value| {
        serde_json::to_vec(&json!({"decided_by": SLACK_PM, "decision": "edit", "args": args}))
            .unwrap()
    };
    for moved in [
        json!({"channel": SLACK_WITH_OUTSIDER, "thread_ts": "1731600000.000100", "text": "Yes."}),
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000999", "text": "Yes."}),
    ] {
        let (s, v) = srv
            .send(Method::POST, &decide, WEB_TOKEN, Some(&edit(moved)), None)
            .await;
        assert_eq!(s, 422, "{v}");
        assert_eq!(v["code"], "target_changed", "{v}");
    }
    let read = format!("/internal/approvals/{}", srv.mine);
    let (_, item) = srv.get(&read, SERVICE_TOKEN, None).await;
    assert_eq!(item["status"], "pending");
    // A text-only edit goes ahead.
    let text_only = json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Yes, it does."});
    let (s, v) = srv
        .send(
            Method::POST,
            &decide,
            WEB_TOKEN,
            Some(&edit(text_only)),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "approved", "{v}");
    assert_eq!(v["args"]["text"], "Yes, it does.");
}

#[tokio::test]
async fn the_console_token_only_reads_even_with_an_assertion() {
    let srv = start(Mode::Require).await;
    let console = Signer::console();
    // Its two reads work, signed.
    let list = list_target(OTHER);
    let a = console.sign(OTHER, "GET", &list, b"");
    let (s, v) = srv.get(&list, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 200, "{v}");
    // Sources and decisions are `403`, whatever it carries (S4).
    let sources = format!("/internal/approvals/{}/sources?user={OTHER}", srv.theirs);
    let a = console.sign(OTHER, "GET", &sources, b"");
    let (s, _) = srv.get(&sources, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 403);
    let decide = format!("/internal/approvals/{}/decision", srv.theirs);
    let body = reject(OTHER);
    let a = console.sign(OTHER, "POST", &decide, &body);
    let (s, _) = srv
        .send(Method::POST, &decide, CONSOLE_TOKEN, Some(&body), Some(&a))
        .await;
    assert_eq!(s, 403);
    let read = format!("/internal/approvals/{}", srv.theirs);
    let (_, item) = srv.get(&read, SERVICE_TOKEN, None).await;
    assert_eq!(item["status"], "pending");
    // The admin reads are `pmos-web`'s and the orchestrator's only.
    let (s, _) = srv
        .get(
            &format!("/internal/admin/overview?user={OTHER}"),
            CONSOLE_TOKEN,
            None,
        )
        .await;
    assert_eq!(s, 403);
}

/// Every approvals route × method, for each client token, with the request otherwise right
/// (the PM named, signed for them when the client signs): exactly the expected set answers
/// `2xx`, and everything else is refused (`401`, `403` or `405`). A new route or method that
/// widens a client fails here (W1).
#[tokio::test]
async fn every_approvals_route_and_method_allows_exactly_what_each_client_needs() {
    const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
    type Allowed = &'static [(&'static str, &'static str)];
    let clients: [(&str, &str, Option<Signer>, Allowed); 4] = [
        (
            "orchestrator",
            SERVICE_TOKEN,
            None,
            &[
                ("GET", "item"),
                ("GET", "list"),
                ("GET", "sources"),
                ("POST", "decision"),
            ],
        ),
        (
            "slack bot",
            BOT_TOKEN,
            None,
            &[("GET", "item"), ("GET", "list"), ("POST", "decision")],
        ),
        (
            "web",
            WEB_TOKEN,
            Some(Signer::web()),
            &[
                ("GET", "item"),
                ("GET", "list"),
                ("GET", "sources"),
                ("POST", "decision"),
            ],
        ),
        (
            "console",
            CONSOLE_TOKEN,
            Some(Signer::console()),
            &[("GET", "item"), ("GET", "list")],
        ),
    ];
    for (name, token, signer, expected) in clients {
        // A fresh server each, since an allowed decision closes the item.
        let srv = start(Mode::Require).await;
        let routes = [
            ("list", list_target(SLACK_PM)),
            (
                "item",
                format!("/internal/approvals/{}?user={SLACK_PM}", srv.mine),
            ),
            (
                "sources",
                format!("/internal/approvals/{}/sources?user={SLACK_PM}", srv.mine),
            ),
            (
                "decision",
                format!("/internal/approvals/{}/decision", srv.mine),
            ),
        ];
        let mut allowed = Vec::new();
        for (route, target) in &routes {
            for method in METHODS {
                let body = (method != "GET" && method != "DELETE").then(|| reject(SLACK_PM));
                let a = signer.as_ref().map(|s| {
                    s.sign(
                        SLACK_PM,
                        method,
                        target,
                        body.as_deref().unwrap_or_default(),
                    )
                });
                let (s, v) = srv
                    .send(
                        Method::from_bytes(method.as_bytes()).unwrap(),
                        target,
                        token,
                        body.as_deref(),
                        a.as_deref(),
                    )
                    .await;
                match s {
                    200..=299 => allowed.push((method, *route)),
                    401 | 403 | 405 => {}
                    other => panic!("{name} {method} {route}: unexpected {other}: {v}"),
                }
            }
        }
        allowed.sort();
        assert_eq!(allowed, expected.to_vec(), "{name}");
    }
}

/// The console reads one item (its admin view) only with an assertion under `require`, and
/// then only the subject's own item, or any item when the subject is an admin in the tool
/// server's own `team.yaml` (W3). The web token always names the PM and reads only theirs.
#[tokio::test]
async fn one_item_reads_are_the_subjects_own_or_a_signed_admins() {
    let srv = start(Mode::Require).await;
    let console = Signer::console();
    let theirs = format!("/internal/approvals/{}", srv.theirs);
    // Unsigned: refused.
    let (s, v) = srv.get(&theirs, CONSOLE_TOKEN, None).await;
    refused(s, &v);
    // Signed as the owner: answered.
    let a = console.sign(OTHER, "GET", &theirs, b"");
    let (s, v) = srv.get(&theirs, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user"], OTHER);
    // Signed as another PM who isn't an admin: as if it didn't exist.
    let a = console.sign(SLACK_PM, "GET", &theirs, b"");
    let (s, _) = srv.get(&theirs, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 404);
    // The roles in the assertion don't count; the tool server's team.yaml does.
    let claims = Claims::new("console", "toolserver", SLACK_PM, &[Role::Admin], "ses_t").unwrap();
    let req = RequestBinding::new("GET", &theirs, b"").unwrap();
    let a = console
        .ring
        .sign(&claims, unix_now(), 60, Some(&req))
        .unwrap();
    let (s, _) = srv.get(&theirs, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 404);
    // Made an admin there: answered.
    let mut file = srv._f.svc.team().file();
    for m in &mut file.members {
        if m.handle == SLACK_PM {
            m.roles.push(Role::Admin);
        }
    }
    srv._f.svc.team().replace(file);
    let a = console.sign(SLACK_PM, "GET", &theirs, b"");
    let (s, v) = srv.get(&theirs, CONSOLE_TOKEN, Some(&a)).await;
    assert_eq!(s, 200, "{v}");
    // The web app is never the admin view: an admin still reads only their own items there.
    let read = format!("/internal/approvals/{}?user={SLACK_PM}", srv.theirs);
    let a = Signer::web().sign(SLACK_PM, "GET", &read, b"");
    let (s, _) = srv.get(&read, WEB_TOKEN, Some(&a)).await;
    assert_eq!(s, 404);
    let a = Signer::web().sign(SLACK_PM, "GET", &theirs, b"");
    let (s, _) = srv.get(&theirs, WEB_TOKEN, Some(&a)).await;
    assert_eq!(s, 404);
}

/// An edit may never move a draft for any client but the orchestrator (W4): the Slack bot
/// and `pmos-web` get `422 target_changed`, the console can't decide at all, and the
/// orchestrator may.
#[tokio::test]
async fn target_changed_applies_to_every_client_but_the_orchestrator() {
    let srv = start(Mode::Require).await;
    let decide = format!("/internal/approvals/{}/decision", srv.mine);
    let moved = serde_json::to_vec(&json!({
        "decided_by": SLACK_PM,
        "decision": "edit",
        "args": {"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000999", "text": "Yes."},
    }))
    .unwrap();
    for (token, signer) in [(BOT_TOKEN, None), (WEB_TOKEN, Some(Signer::web()))] {
        let a = signer.map(|s| s.sign(SLACK_PM, "POST", &decide, &moved));
        let (s, v) = srv
            .send(Method::POST, &decide, token, Some(&moved), a.as_deref())
            .await;
        assert_eq!(s, 422, "{v}");
        assert_eq!(v["code"], "target_changed", "{v}");
    }
    let a = Signer::console().sign(SLACK_PM, "POST", &decide, &moved);
    let (s, _) = srv
        .send(Method::POST, &decide, CONSOLE_TOKEN, Some(&moved), Some(&a))
        .await;
    assert_eq!(s, 403);
    let read = format!("/internal/approvals/{}", srv.mine);
    let (_, item) = srv.get(&read, SERVICE_TOKEN, None).await;
    assert_eq!(item["status"], "pending");
    let (s, v) = srv
        .send(Method::POST, &decide, SERVICE_TOKEN, Some(&moved), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["args"]["thread_ts"], "1731600000.000999");
}
