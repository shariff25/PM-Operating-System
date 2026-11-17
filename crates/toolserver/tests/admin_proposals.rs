//! `POST /internal/admin/proposals`: `pmos-web` records each policy or content proposal it
//! opens as a pull request. Who may call it (the orchestrator and `pmos-web` only), whose
//! record it is (the `user=` admin, whose role is checked here and who must be the signed
//! assertion's subject, bound to the body), what is recorded (paths as a count, the pull
//! request as the target, never file text), and what is refused.

mod common;

use std::sync::Arc;

use common::Fixture;
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const BASE: &str = "1111111111111111111111111111111111111111";

struct Server {
    base: String,
    cancel: CancellationToken,
    _f: Fixture,
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
    Server {
        base,
        cancel,
        _f: f,
    }
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[], "unknown").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

fn record(kind: &str, stage: &str, pr: Option<u64>) -> Value {
    json!({
        "kind": kind,
        "stage": stage,
        "paths": ["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"],
        "repo": "acme/pmos",
        "base": BASE,
        "pr": pr,
    })
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

    /// `pmos-web`'s call for `viewer`, signed for `signer`.
    async fn propose_as(&self, signer: &str, viewer: &str, body: &Value) -> (u16, Value) {
        let target = format!("/internal/admin/proposals?user={viewer}");
        let bytes = serde_json::to_vec(body).unwrap();
        let a = sign(signer, "POST", &target, &bytes);
        self.send(
            reqwest::Method::POST,
            &target,
            Some(WEB_TOKEN),
            Some(&a),
            Some(&bytes),
        )
        .await
    }

    async fn events(&self, action: &str) -> Vec<Value> {
        let target = format!("/internal/admin/audit?user=ada&action={action}");
        let a = sign("ada", "GET", &target, b"");
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
async fn an_admins_proposal_is_recorded_without_file_text() {
    let srv = start(Mode::Require).await;
    let (status, body) = srv
        .propose_as("ada", "ada", &record("policy", "started", None))
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = srv
        .propose_as("ada", "ada", &record("policy", "opened", Some(42)))
        .await;
    assert_eq!(status, 200);
    let (status, _) = srv
        .propose_as("ada", "ada", &record("content", "failed", None))
        .await;
    assert_eq!(status, 200);
    let events = srv.events("admin.policy_proposal").await;
    assert_eq!(events.len(), 2, "{events:?}");
    // Newest first.
    assert_eq!(events[0]["outcome"], "ok");
    assert_eq!(events[0]["target"], "github:acme/pmos#42");
    assert_eq!(events[1]["outcome"], "started");
    assert_eq!(events[1]["target"], Value::Null);
    for e in &events {
        assert_eq!(e["actor"], "ada");
        assert_eq!(e["client"], "web");
        assert_eq!(e["sources"], 2, "paths as a count");
        assert_eq!(e["task_id"], "admin");
    }
    let content = srv.events("admin.content_proposal").await;
    assert_eq!(content[0]["outcome"], "error");
    srv.cancel.cancel();
}

#[tokio::test]
async fn only_an_admin_through_pmos_web_or_the_orchestrator() {
    let srv = start(Mode::Require).await;
    // An auditor and a PM: refused, and the refusal recorded.
    for who in ["aud", "ana"] {
        let (status, body) = srv
            .propose_as(who, who, &record("policy", "started", None))
            .await;
        assert_eq!(status, 403, "{who}: {body}");
        assert_eq!(body["code"], "forbidden");
    }
    let denied = srv.events("admin.policy_proposal").await;
    assert_eq!(denied.len(), 2);
    assert!(denied.iter().all(|e| e["outcome"] == "denied"));
    // Signed for a PM, naming the admin: refused.
    let (status, body) = srv
        .propose_as("ana", "ada", &record("policy", "started", None))
        .await;
    assert_eq!(status, 401, "{body}");
    // Signed over another body: refused.
    let target = "/internal/admin/proposals?user=ada";
    let signed = serde_json::to_vec(&record("content", "started", None)).unwrap();
    let sent = serde_json::to_vec(&record("policy", "started", None)).unwrap();
    let a = sign("ada", "POST", target, &signed);
    let (status, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            Some(&a),
            Some(&sent),
        )
        .await;
    assert_eq!(status, 401);
    // No assertion at all, in `require`.
    let (status, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            None,
            Some(&sent),
        )
        .await;
    assert_eq!(status, 401);
    // The Slack bot and the console may not; no token is no entry.
    for token in [BOT_TOKEN, CONSOLE_TOKEN] {
        let (status, _) = srv
            .send(
                reqwest::Method::POST,
                target,
                Some(token),
                None,
                Some(&sent),
            )
            .await;
        assert_eq!(status, 403);
    }
    let (status, _) = srv
        .send(reqwest::Method::POST, target, None, None, Some(&sent))
        .await;
    assert_eq!(status, 401);
    // The orchestrator may.
    let (status, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(SERVICE_TOKEN),
            None,
            Some(&sent),
        )
        .await;
    assert_eq!(status, 200);
    srv.cancel.cancel();
}

#[tokio::test]
async fn malformed_records_are_refused() {
    let srv = start(Mode::Require).await;
    let mut bad = Vec::new();
    for (k, v) in [
        ("kind", json!("deploy")),
        ("stage", json!("merged")),
        ("repo", json!("acme")),
        ("repo", json!("acme/../x")),
        ("base", json!("not-a-sha")),
        ("paths", json!(["../etc/passwd"])),
        ("paths", json!(["content/.git/config"])),
        ("paths", json!(["/abs"])),
        ("text", json!("file text is never taken")),
    ] {
        let mut r = record("policy", "started", None);
        r[k] = v;
        bad.push(r);
    }
    // Opened with no pull request.
    bad.push(record("policy", "opened", None));
    for r in bad {
        let (status, body) = srv.propose_as("ada", "ada", &r).await;
        assert_eq!(status, 400, "{r}: {body}");
    }
    assert!(srv.events("admin.policy_proposal").await.is_empty());
    srv.cancel.cancel();
}
