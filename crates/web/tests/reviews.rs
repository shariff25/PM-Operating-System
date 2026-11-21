//! `pmos-web`'s Reviewer routes (`/api/reviews…`, docs/web-api.md, Reviews; slice U6a) and the
//! recommendations on the owner's draft (`GET /api/drafts/{id}`), against a mock tool server:
//! who may call, the origin and CSRF checks, the forwarded request and its assertion, the
//! re-picked answers (metadata only for a restricted draft), the relayed refusal codes, and
//! `/api/me`'s `reviewer`. Fake data only.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::Duration;
use pmos_web::upstream::{Credentials, Signer};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

mod support;

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
// Test key only: fixed bytes, never used anywhere real.
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const HASH: &str = "3f9a1c2b4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8";

const TEAM: &str = "members:
  - {handle: adam, slack-id: U0ADM, roles: [admin]}
  - {handle: ana, slack-id: U0ANA, roles: [pm]}
  - {handle: rex, slack-id: U0REX, roles: [reviewer]}
";

#[derive(Debug, Clone)]
struct Call {
    uri: String,
    method: String,
    assertion: Option<String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct Tools {
    calls: Mutex<Vec<Call>>,
    directory: support::FakeDirectory,
    /// The bot marketplace's routes (the catalog, each PM's team), answered apart.
    bots: support::FakeBots,
    /// A refusal the reviewer routes answer with, instead of success.
    refuse: Mutex<Option<(u16, String)>>,
    /// The recommendation's answer is lost (a `500`).
    lose: Mutex<bool>,
}

impl Tools {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

/// A pending Jira draft as the tool server's reviewer view shows it, with fields the
/// contract doesn't have (which `pmos-web` must drop).
fn open_view() -> Value {
    json!({
        "id": "apr_1", "owner": "ana", "owner_name": "Ana <b>Silva</b>\u{7}",
        "tool": "jira.create_issue", "bot": "pm-ana", "skill": null, "status": "pending",
        "created_at": "2026-11-16T09:00:00Z", "expires_at": null, "revision": 2,
        "restricted": false, "sources_sensitivity": "internal",
        "args": {"project": "PM", "issue_type": "Story", "summary": "Fake: SSO grace",
                 "description": "Fake body [[src-3f9a1c]]."},
        "args_hash": HASH, "edited": false, "edited_at": null,
        "my_recommendation": {"id": "rec_1", "verdict": "approve", "note": "Fake note",
                              "note_removed": false, "draft_revision": 2,
                              "created_at": "2026-11-16T10:00:00Z", "stale": false},
        "sources": [{"system": "jira", "title": "PM-1: Fake epic", "url": "https://example.atlassian.net/browse/PM-1",
                     "ref": "PM-1", "cite": "src-3f9a1c", "tool": "jira.read", "read_at": "2026-11-16T08:59:00Z"}],
        "secret": "SECRET",
    })
}

/// A restricted draft: the tool server sends metadata only; `pmos-web` must not show text
/// even if some came.
fn restricted_view() -> Value {
    json!({
        "id": "apr_2", "owner": "ana", "owner_name": "", "tool": "slack.post_reply",
        "status": "pending", "created_at": "2026-11-16T09:30:00Z",
        "expires_at": "2026-11-17T09:30:00Z", "revision": 1, "restricted": true,
        "args": {"channel": "C0X", "thread_ts": "1.0", "text": "SECRET restricted text"},
        "args_hash": HASH,
    })
}

async fn toolserver(
    State(tools): State<Arc<Tools>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = tools.directory.handle(&method, &uri, &body) {
        return res;
    }
    if let Some(res) = tools.bots.handle(&method, &uri, &headers, &body) {
        return res;
    }
    tools.calls.lock().unwrap().push(Call {
        uri: uri.to_string(),
        method: method.to_string(),
        assertion: headers
            .get("x-pmos-assertion")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        body: body.to_vec(),
    });
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path = uri.path();
    if path.starts_with("/internal/reviews") {
        if let Some((status, code)) = tools.refuse.lock().unwrap().clone() {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"code": code, "error": "SECRET tool server sentence"})),
            )
                .into_response();
        }
        if method == Method::POST && *tools.lose.lock().unwrap() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    match (method.as_str(), path) {
        ("GET", "/internal/reviews") => Json(json!({
            "items": [open_view(), restricted_view(),
                      {"id": "apr_3", "owner": "ana", "tool": "bets.record", "restricted": false,
                       "status": "pending", "args": {}, "args_hash": HASH}],
            "total": 3, "truncated": false, "pms": ["ana", "Bad Handle"], "extra": "SECRET",
        }))
        .into_response(),
        ("GET", "/internal/reviews/apr_1") => Json(open_view()).into_response(),
        ("GET", "/internal/reviews/apr_2") => Json(restricted_view()).into_response(),
        ("POST", "/internal/reviews/apr_1/recommendation") => {
            let mut v = open_view();
            v["my_recommendation"]["verdict"] = json!("changes");
            Json(json!({"recommendation": v["my_recommendation"], "draft": v})).into_response()
        }
        // The owner's own draft, with the recommendations on it.
        ("GET", "/internal/approvals/apr_9") => Json(json!({
            "id": "apr_9", "user": "ana", "bot": "pm-ana", "run_id": "tsk_9",
            "tool": "jira.create_issue", "args": {"project": "PM", "issue_type": "Story", "summary": "Fake"},
            "args_hash": HASH, "revision": 3, "status": "pending", "created_at": "2026-11-16T09:00:00Z",
            "recommendations": [
                {"id": "rec_1", "reviewer": "rex", "reviewer_name": "Rex <img src=x onerror=alert(1)>",
                 "verdict": "approve", "note": "Fake note <!channel>\u{7}", "note_removed": false,
                 "draft_revision": 2, "created_at": "2026-11-16T10:00:00Z", "stale": true},
                {"id": "rec_2", "reviewer": "bo", "reviewer_name": "", "verdict": "changes",
                 "note_removed": true, "draft_revision": 3, "created_at": "2026-11-16T11:00:00Z",
                 "stale": false},
                {"id": "rec_3", "reviewer": "cy", "verdict": "reject", "created_at": "2026-11-16T11:00:00Z"},
                {"id": "rec_4", "reviewer": "Not A Handle", "verdict": "approve", "created_at": "2026-11-16T11:00:00Z"},
            ],
        }))
        .into_response(),
        ("GET", "/internal/approvals/apr_9/sources") => {
            Json(json!({"sources": [], "available": true, "truncated": false})).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve(app: Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, cancel)
}

fn ring() -> pmos_assertion::KeyRing {
    pmos_assertion::KeyRing::new(pmos_assertion::Key::from_base64("wt1", WEB_TOOLS_KEY).unwrap())
}

struct Web {
    base: String,
    tools: Arc<Tools>,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

struct Reply {
    status: StatusCode,
    text: String,
    body: Value,
}

async fn web() -> Web {
    let tools = Arc::new(Tools::default());
    tools.directory.seed_team(TEAM);
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url: "http://127.0.0.1:1".into(),
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url,
        toolserver_token: Credentials::new(TOOLS_TOKEN).with_signer(Signer::new(
            "web",
            "toolserver",
            ring(),
        )),
        content_root: dir.path().to_path_buf(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        password_login: false,
        trusted_proxies: Vec::new(),
    };
    let (base, stop_web) = serve(pmos_web::build(cfg, Deps::in_memory()).unwrap()).await;
    Web {
        base,
        tools,
        http: reqwest::Client::new(),
        _dir: dir,
        _stop: vec![stop_tools, stop_web],
    }
}

async fn reply(res: reqwest::Response) -> Reply {
    let status = res.status();
    let text = res.text().await.unwrap();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Reply { status, text, body }
}

impl Web {
    async fn login(&self, handle: &str) -> String {
        let res = self
            .http
            .post(format!("{}/api/auth/dev-login", self.base))
            .header("origin", self.base.clone())
            .json(&json!({ "handle": handle }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
        cookie
            .strip_prefix("__Host-pmos=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn get(&self, cookie: &str, path: &str) -> Reply {
        reply(
            self.http
                .get(format!("{}{path}", self.base))
                .header("cookie", format!("__Host-pmos={cookie}"))
                .send()
                .await
                .unwrap(),
        )
        .await
    }

    /// A write as the app sends it: same origin (unless `origin` says otherwise), the
    /// session's CSRF token (unless `csrf` is false), JSON.
    async fn post(
        &self,
        cookie: &str,
        path: &str,
        body: &Value,
        origin: Option<&str>,
        csrf: bool,
    ) -> Reply {
        let me = self.get(cookie, "/api/me").await;
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("cookie", format!("__Host-pmos={cookie}"))
            .json(body);
        if csrf {
            req = req.header("x-pmos-csrf", me.body["csrf_token"].as_str().unwrap());
        }
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        reply(req.send().await.unwrap()).await
    }
}

fn subject(call: &Call) -> String {
    let policy = pmos_assertion::VerifyPolicy::new("toolserver", &["web"]).unwrap();
    let binding = pmos_assertion::RequestBinding::new(&call.method, &call.uri, &call.body).unwrap();
    ring()
        .verify(
            call.assertion.as_deref().expect("signed"),
            &policy,
            pmos_assertion::unix_now(),
            Some(&binding),
            &pmos_assertion::MemoryReplayCache::default(),
        )
        .unwrap()
        .subject()
        .to_string()
}

fn recommend_body() -> Value {
    json!({"verdict": "changes", "note": "Fake note", "revision": 2, "args_hash": HASH})
}

#[tokio::test]
async fn me_says_who_reviews() {
    let w = web().await;
    for (who, reviewer) in [("rex", true), ("ana", false), ("adam", false)] {
        let c = w.login(who).await;
        let me = w.get(&c, "/api/me").await;
        assert_eq!(me.body["reviewer"], reviewer, "{who}");
    }
}

#[tokio::test]
async fn the_inbox_and_a_read_are_re_picked_and_signed_for_the_reviewer() {
    let w = web().await;
    let c = w.login("rex").await;
    let r = w.get(&c, "/api/reviews").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    let drafts = r.body["drafts"].as_array().unwrap();
    assert_eq!(
        drafts.len(),
        2,
        "a write that isn't a draft is left out: {}",
        r.text
    );
    assert_eq!(drafts[0]["id"], "apr_1");
    assert_eq!(drafts[0]["kind"], "jira_issue");
    assert_eq!(drafts[0]["title"], "Fake: SSO grace");
    assert_eq!(
        drafts[0]["owner_name"], "Ana <b>Silva</b>",
        "plain text, control characters out"
    );
    assert_eq!(drafts[0]["my_recommendation"]["verdict"], "approve");
    assert!(drafts[0].get("body").is_none());
    assert_eq!(drafts[1]["restricted"], true);
    assert_eq!(drafts[1]["title"], Value::Null);
    assert_eq!(drafts[1]["owner_name"], "ana");
    assert_eq!(r.body["total"], 3);
    assert_eq!(r.body["pms"], json!(["ana"]));
    let call = w.tools.calls().last().unwrap().clone();
    assert_eq!(call.uri, "/internal/reviews?user=rex");
    assert_eq!(subject(&call), "rex");

    let r = w.get(&c, "/api/reviews/apr_1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    assert_eq!(
        r.body["body"], "Fake body[[s1]].",
        "markers resolved as for the owner"
    );
    assert_eq!(r.body["sources"][0]["cited"], true);
    assert_eq!(r.body["target"]["system"], "jira");
    assert_eq!(r.body["revision"], 2);
    assert_eq!(r.body["args_hash"], HASH);
    assert_eq!(r.body["my_recommendation"]["note"], "Fake note");
    assert_eq!(r.body["my_recommendation"]["revision"], 2);
    let call = w.tools.calls().last().unwrap().clone();
    assert_eq!(call.uri, "/internal/reviews/apr_1?user=rex");
    assert_eq!(subject(&call), "rex");

    // Restricted: metadata only, even if text came along.
    let r = w.get(&c, "/api/reviews/apr_2").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    for hidden in [
        "title",
        "body",
        "target",
        "sources",
        "args_hash",
        "my_recommendation",
    ] {
        assert!(r.body.get(hidden).is_none(), "{hidden}: {}", r.text);
    }
    assert_eq!(r.body["kind"], "slack_reply");
    assert_eq!(r.body["expires_at"], "2026-11-17T09:30:00Z");
}

#[tokio::test]
async fn a_recommendation_is_forwarded_signed_with_the_version_read() {
    let w = web().await;
    let c = w.login("rex").await;
    let base = w.base.clone();
    let r = w
        .post(
            &c,
            "/api/reviews/apr_1/recommendation",
            &recommend_body(),
            Some(&base),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["recommendation"]["verdict"], "changes");
    assert_eq!(r.body["draft"]["id"], "apr_1");
    assert!(!r.text.contains("SECRET"));
    let call = w.tools.calls().last().unwrap().clone();
    assert_eq!(call.method, "POST");
    assert_eq!(call.uri, "/internal/reviews/apr_1/recommendation?user=rex");
    let sent: Value = serde_json::from_slice(&call.body).unwrap();
    assert_eq!(
        sent,
        json!({"verdict": "changes", "note": "Fake note", "draft_revision": 2, "args_hash": HASH})
    );
    assert_eq!(subject(&call), "rex");
}

#[tokio::test]
async fn only_a_reviewer_from_this_app_with_a_well_formed_body() {
    let w = web().await;
    let base = w.base.clone();
    // A PM or an admin without the reviewer role: refused here, no upstream call.
    for who in ["ana", "adam"] {
        let c = w.login(who).await;
        let before = w.tools.calls().len();
        for path in ["/api/reviews", "/api/reviews/apr_1"] {
            let r = w.get(&c, path).await;
            assert_eq!(
                (r.status, &r.body["code"]),
                (StatusCode::FORBIDDEN, &json!("forbidden")),
                "{who} {path}"
            );
        }
        let r = w
            .post(
                &c,
                "/api/reviews/apr_1/recommendation",
                &recommend_body(),
                Some(&base),
                true,
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who}");
        assert_eq!(w.tools.calls().len(), before, "{who}");
    }
    let c = w.login("rex").await;
    let before = w.tools.calls().len();
    for origin in [None, Some("https://evil.example")] {
        let r = w
            .post(
                &c,
                "/api/reviews/apr_1/recommendation",
                &recommend_body(),
                origin,
                true,
            )
            .await;
        assert_eq!(r.body["code"], "bad_origin");
    }
    let r = w
        .post(
            &c,
            "/api/reviews/apr_1/recommendation",
            &recommend_body(),
            Some(&base),
            false,
        )
        .await;
    assert_eq!(r.body["code"], "bad_csrf");
    for bad in [
        json!({"verdict": "reject", "revision": 2, "args_hash": HASH}),
        json!({"verdict": "approve", "revision": 0, "args_hash": HASH}),
        json!({"verdict": "approve", "revision": 2, "args_hash": "abc"}),
        json!({"verdict": "approve", "revision": 2, "args_hash": HASH, "extra": true}),
        json!(["approve"]),
    ] {
        let r = w
            .post(
                &c,
                "/api/reviews/apr_1/recommendation",
                &bad,
                Some(&base),
                true,
            )
            .await;
        assert_eq!(r.body["code"], "bad_request", "{bad}");
    }
    let mut long = recommend_body();
    long["note"] = json!("x".repeat(1001));
    let r = w
        .post(
            &c,
            "/api/reviews/apr_1/recommendation",
            &long,
            Some(&base),
            true,
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::UNPROCESSABLE_ENTITY, &json!("invalid"))
    );
    let r = w.get(&c, "/api/reviews/bad%20id").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(w.tools.calls().len(), before, "refused before any call");
}

#[tokio::test]
async fn refusals_keep_the_tool_servers_code() {
    let w = web().await;
    let c = w.login("rex").await;
    let base = w.base.clone();
    for (status, code) in [
        (403, "own_draft"),
        (403, "restricted_draft"),
        (403, "forbidden"),
        (404, "not_found"),
        (409, "draft_changed"),
        (409, "not_pending"),
        (409, "expired"),
        (503, "assertion_required"),
        (503, "unavailable"),
    ] {
        *w.tools.refuse.lock().unwrap() = Some((status, code.to_string()));
        let r = w
            .post(
                &c,
                "/api/reviews/apr_1/recommendation",
                &recommend_body(),
                Some(&base),
                true,
            )
            .await;
        assert_eq!(
            (r.status.as_u16(), &r.body["code"]),
            (status, &json!(code)),
            "{}",
            r.text
        );
        assert!(
            !r.text.contains("SECRET"),
            "{code}: the sentence is pmos-web's own"
        );
        let r = w.get(&c, "/api/reviews/apr_1").await;
        assert_eq!(r.body["code"], code);
    }
    *w.tools.refuse.lock().unwrap() = Some((401, "bad_assertion".to_string()));
    let r = w.get(&c, "/api/reviews").await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::BAD_GATEWAY, &json!("upstream_unavailable"))
    );
    *w.tools.refuse.lock().unwrap() = None;
    *w.tools.lose.lock().unwrap() = true;
    let r = w
        .post(
            &c,
            "/api/reviews/apr_1/recommendation",
            &recommend_body(),
            Some(&base),
            true,
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::BAD_GATEWAY, &json!("unknown_outcome"))
    );
}

#[tokio::test]
async fn the_owners_draft_carries_its_recommendations() {
    let w = web().await;
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/drafts/apr_9").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let recs = r.body["recommendations"].as_array().unwrap();
    assert_eq!(
        recs.len(),
        2,
        "an unknown verdict and a malformed reviewer are left out: {}",
        r.text
    );
    assert_eq!(recs[0]["reviewer"], "rex");
    assert_eq!(
        recs[0]["reviewer_name"], "Rex <img src=x onerror=alert(1)>",
        "as text: the app renders it plain"
    );
    assert_eq!(recs[0]["verdict"], "approve");
    assert_eq!(recs[0]["note"], "Fake note <!channel>");
    assert_eq!(recs[0]["stale"], true);
    assert_eq!(recs[0]["created_at"], "2026-11-16T10:00:00Z");
    assert_eq!(recs[1]["reviewer_name"], "bo");
    assert_eq!(recs[1]["note"], Value::Null);
    assert_eq!(recs[1]["note_removed"], true);
    assert_eq!(recs[1]["stale"], false);
}
