//! `POST /internal/admin/events`: `pmos-web` records sign-ins, sign-outs and refused
//! sign-ins. Only `pmos-web`'s token; a sign-in or sign-out names the member, with a user
//! assertion for them; a refusal names no one. Only fixed actions, providers and codes, never
//! free text; and the events appear in the admin audit list with the existing fields.

mod common;

use std::sync::Arc;

use common::Fixture;
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

struct Server {
    base: String,
    cancel: CancellationToken,
    f: Fixture,
}

async fn start(mode: Mode) -> Server {
    let (f, _) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
        ],
        false,
    )
    .await
    .unwrap();
    let keys = parse_keys(
        &format!("web:toolserver:w1:{WEB_KEY}"),
        "the tool server",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
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
            assertions: Arc::new(Assertions::new(mode, keys).unwrap()),
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
    Server { base, cancel, f }
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[], "unknown").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

impl Server {
    async fn send(
        &self,
        method: reqwest::Method,
        target: &str,
        token: Option<&str>,
        assertion: Option<&str>,
        body: Option<&[u8]>,
    ) -> (u16, Value) {
        let mut r = reqwest::Client::new().request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
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

    /// `pmos-web`'s record of `body`, for `user` (signed for them) or for no one.
    async fn record(&self, user: Option<&str>, body: &Value) -> (u16, Value) {
        let target = match user {
            Some(u) => format!("/internal/admin/events?user={u}"),
            None => "/internal/admin/events".to_string(),
        };
        let bytes = serde_json::to_vec(body).unwrap();
        let a = user.map(|u| sign(u, "POST", &target, &bytes));
        self.send(
            reqwest::Method::POST,
            &target,
            Some(WEB_TOKEN),
            a.as_deref(),
            Some(&bytes),
        )
        .await
    }

    async fn events(&self, action: &str) -> Vec<Value> {
        let target = format!("/internal/admin/audit?user=aud&action={action}");
        let a = sign("aud", "GET", &target, b"");
        let (status, body) = self
            .send(
                reqwest::Method::GET,
                &target,
                Some(WEB_TOKEN),
                Some(&a),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["events"].as_array().unwrap().clone()
    }
}

#[tokio::test]
async fn sign_ins_sign_outs_and_refusals_are_listed() {
    let srv = start(Mode::Require).await;
    let (status, body) = srv
        .record(
            Some("ana"),
            &json!({"action": "web.sign_in", "provider": "entra"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["recorded"], true);
    let (status, _) = srv
        .record(
            Some("ana"),
            &json!({"action": "web.sign_out", "provider": "entra"}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, body) = srv
        .record(
            None,
            &json!({"action": "web.sign_in_refused", "provider": "entra",
                    "reason": "not_a_member", "attempt": "3f9a1c2b4d5e"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = srv
        .record(
            None,
            &json!({"action": "web.sign_in_refused", "provider": "entra",
                    "reason": "wrong_account", "member": "ana"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    let signed_in = srv.events("web.sign_in").await;
    assert_eq!(signed_in.len(), 1);
    let e = &signed_in[0];
    assert_eq!(e["actor"], "ana");
    assert_eq!(e["client"], "web");
    assert_eq!(e["system"], "web");
    assert_eq!(e["outcome"], "ok");
    assert_eq!(e["detail"], "provider: entra");
    assert_eq!(e["task_id"], "web-auth");
    assert_eq!(srv.events("web.sign_out").await[0]["actor"], "ana");

    let refused = srv.events("web.sign_in_refused").await;
    assert_eq!(refused.len(), 2);
    // Newest first.
    assert_eq!(refused[0]["actor"], "anonymous");
    assert_eq!(refused[0]["outcome"], "denied");
    assert_eq!(refused[0]["target"], "member:ana");
    assert_eq!(
        refused[0]["detail"],
        "provider: entra; reason: wrong_account"
    );
    assert_eq!(
        refused[1]["detail"],
        "provider: entra; reason: not_a_member; attempt: 3f9a1c2b4d5e"
    );
    assert_eq!(refused[1]["target"], Value::Null);
    srv.cancel.cancel();
}

#[tokio::test]
async fn only_fixed_actions_and_codes_never_free_text() {
    let srv = start(Mode::Require).await;
    for (user, body) in [
        (
            Some("ana"),
            json!({"action": "web.anything", "provider": "entra"}),
        ),
        (
            Some("ana"),
            json!({"action": "admin.audit", "provider": "entra"}),
        ),
        (
            Some("ana"),
            json!({"action": "web.sign_in", "provider": "okta"}),
        ),
        (
            Some("ana"),
            json!({"action": "web.sign_in", "provider": "entra", "note": "hello"}),
        ),
        (
            Some("ana"),
            json!({"action": "web.sign_in", "provider": "entra", "reason": "not_a_member"}),
        ),
        (None, json!({"action": "web.sign_in", "provider": "entra"})),
        (
            None,
            json!({"action": "web.sign_in_refused", "provider": "entra", "reason": "ana@example.com"}),
        ),
        (
            None,
            json!({"action": "web.sign_in_refused", "provider": "entra",
                   "reason": "not_a_member", "attempt": "ana@example.com"}),
        ),
        (
            None,
            json!({"action": "web.sign_in_refused", "provider": "entra",
                   "reason": "not_a_member", "member": "ana"}),
        ),
        (
            None,
            json!({"action": "web.sign_in_refused", "provider": "entra",
                   "reason": "wrong_account", "member": "mallory"}),
        ),
        (
            Some("ana"),
            json!({"action": "web.sign_in_refused", "provider": "entra", "reason": "failed"}),
        ),
    ] {
        let (status, resp) = srv.record(user, &body).await;
        assert_eq!(status, 400, "{body}: {resp}");
        assert_eq!(resp["code"], "bad_request");
    }
    // Someone who isn't a team member never signs in.
    let (status, _) = srv
        .record(
            Some("mallory"),
            &json!({"action": "web.sign_in", "provider": "dev"}),
        )
        .await;
    assert_eq!(status, 403);
    for action in ["web.sign_in", "web.sign_out", "web.sign_in_refused"] {
        assert!(srv.events(action).await.is_empty(), "{action}");
    }
    srv.cancel.cancel();
}

#[tokio::test]
async fn only_pmos_web_with_an_assertion_for_the_member() {
    let srv = start(Mode::Require).await;
    let body = serde_json::to_vec(&json!({"action": "web.sign_in", "provider": "entra"})).unwrap();
    let target = "/internal/admin/events?user=ana";
    // Signed for someone else, or not signed: refused.
    let other = sign("ada", "POST", target, &body);
    for a in [Some(other.as_str()), None] {
        let (status, _) = srv
            .send(
                reqwest::Method::POST,
                target,
                Some(WEB_TOKEN),
                a,
                Some(&body),
            )
            .await;
        assert_eq!(status, 401);
    }
    // The orchestrator, the Slack bot and the console may not record sign-ins.
    for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
        let (status, _) = srv
            .send(
                reqwest::Method::POST,
                target,
                Some(token),
                None,
                Some(&body),
            )
            .await;
        assert_eq!(status, 403, "{token}");
    }
    let (status, _) = srv
        .send(reqwest::Method::POST, target, None, None, Some(&body))
        .await;
    assert_eq!(status, 401);
    assert!(srv.events("web.sign_in").await.is_empty());
    srv.cancel.cancel();
}

#[tokio::test]
async fn an_event_that_cant_be_recorded_is_a_503() {
    let srv = start(Mode::Require).await;
    srv.f.audit_switch.down.store(true, Ordering::SeqCst);
    let (status, _) = srv
        .record(
            None,
            &json!({"action": "web.sign_in_refused", "provider": "dev", "reason": "not_a_member"}),
        )
        .await;
    assert_eq!(status, 503);
    srv.f.audit_switch.down.store(false, Ordering::SeqCst);
    srv.cancel.cancel();
}
