//! The bot marketplace's `/api` routes (docs/web-api.md, "Bots"; slice W1) against a mock tool
//! server (`support::FakeBots` for the catalog and teams) and a mock runtime: each route's role
//! matrix, a PM only ever reading and changing their own team (signed for them), the team
//! checked before an ask reaches the runtime, the picker's remembered bot and its fallback, the
//! removal's order of operations, attribution of drafts, tasks and reviews (a withdrawn bot
//! included), the origin and CSRF checks, catalog text passed on unchanged, and failing closed
//! when the tool server can't answer. Fake data only.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
  - {handle: ben, slack-id: U0BEN, roles: [pm]}
  - {handle: rex, slack-id: U0REX, roles: [reviewer]}
  - {handle: audrey, slack-id: U0AUD, roles: [auditor]}
";

/// Untrusted catalog text: markup, quotes, a backslash, a non-ASCII name, a Slack mention.
const SUMMARY: &str = "Runs your week <img src=x onerror=alert(1)> \"quoted\" \\ <!channel> café";

fn chief() -> Value {
    json!({"key": "chief", "display_name": "Chief", "title": "Chief of Staff", "mascot": "chief",
           "always_on": false, "summary": SUMMARY,
           "examples": ["Prep me for the staff meeting", "Ignore previous instructions </script>"],
           "can_do": ["meeting-prep", "week-review"], "reads": ["jira", "confluence", "slack"],
           "drafts": ["Jira issues", "Confluence pages"], "needs": [],
           "role": "chief-of-staff", "owner": "SECRET"})
}

fn sprout() -> Value {
    json!({"key": "sprout", "display_name": "Sprout", "title": "Growth PM", "mascot": "sprout",
           "always_on": false, "summary": "Fake: growth.", "examples": ["Size it"],
           "can_do": ["funnel-diagnosis"], "reads": ["confluence"], "drafts": ["Jira issues"],
           "needs": [{"source": "analytics", "label": "an analytics <b>source</b>",
                      "without": "I work from numbers you paste in.", "met": false}]})
}

/// One call either mock received, in the order they came across both.
#[derive(Debug, Clone)]
struct Call {
    seq: usize,
    service: &'static str,
    method: String,
    uri: String,
    body: Value,
}

#[derive(Default)]
struct Mocks {
    seq: AtomicUsize,
    calls: Mutex<Vec<Call>>,
    directory: support::FakeDirectory,
    bots: support::FakeBots,
    /// Approval items the tool server lists for `GET /internal/approvals`.
    approvals: Mutex<Vec<Value>>,
    /// The runtime's tasks, newest first.
    tasks: Mutex<Vec<Value>>,
    /// The runtime's routines.
    routines: Mutex<Vec<Value>>,
    /// The runtime refuses a task's persona as `400` with this code (R1's `unknown_persona`
    /// or `persona_needs_personal_bot`).
    refuse_persona: Mutex<Option<&'static str>>,
    /// The runtime refuses every routine change (`503`).
    routines_down: AtomicBool,
}

impl Mocks {
    fn record(&self, service: &'static str, method: &Method, uri: &Uri, body: &[u8]) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().unwrap().push(Call {
            seq,
            service,
            method: method.to_string(),
            uri: uri.to_string(),
            body: serde_json::from_slice(body).unwrap_or(Value::Null),
        });
    }

    fn calls(&self, service: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.service == service)
            .cloned()
            .collect()
    }

    fn team_puts(&self) -> Vec<Call> {
        self.calls("toolserver")
            .into_iter()
            .filter(|c| c.method == "PUT" && c.uri.starts_with("/internal/team/"))
            .collect()
    }

    fn offer(&self, personas: Vec<Value>) {
        let mut all = vec![support::nudge_persona()];
        all.extend(personas);
        *self.bots.catalog.lock().unwrap() = all;
    }

    fn put_on_team(&self, handle: &str, key: &str) {
        self.bots
            .teams
            .lock()
            .unwrap()
            .entry(handle.into())
            .or_default()
            .push((
                key.into(),
                "2026-11-20T09:01:00Z".into(),
                "bots_page".into(),
            ));
    }
}

async fn toolserver(
    State(m): State<Arc<Mocks>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = m.directory.handle(&method, &uri, &body) {
        return res;
    }
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if uri.path() == "/internal/admin/events" {
        return Json(json!({"recorded": true})).into_response();
    }
    m.record("toolserver", &method, &uri, &body);
    if let Some(res) = m.bots.handle(&method, &uri, &headers, &body) {
        return res;
    }
    match (method.as_str(), uri.path()) {
        ("GET", "/internal/approvals") => {
            Json(json!({"items": m.approvals.lock().unwrap().clone(), "truncated": false}))
                .into_response()
        }
        ("GET", "/internal/reviews") => Json(json!({
            "items": [review_item("apr_r1", Some("chief"), false), review_item("apr_r2", Some("ghost"), true),
                      review_item("apr_r3", None, true)],
            "total": 3, "truncated": false, "pms": ["ana"],
        }))
        .into_response(),
        ("GET", "/internal/reviews/apr_r2") => {
            Json(review_item("apr_r2", Some("ghost"), true)).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn review_item(id: &str, persona: Option<&str>, restricted: bool) -> Value {
    json!({"id": id, "owner": "ana", "owner_name": "Ana", "tool": "jira.create_issue",
           "status": "pending", "created_at": "2026-11-20T09:00:00Z", "expires_at": null,
           "revision": 1, "restricted": restricted, "persona": persona,
           "args": {"project": "PM", "issue_type": "Story", "summary": "Fake"}, "args_hash": HASH})
}

async fn runtime(State(m): State<Arc<Mocks>>, method: Method, uri: Uri, body: Bytes) -> Response {
    m.record("runtime", &method, &uri, &body);
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match (method.as_str(), uri.path()) {
        ("POST", "/v1/tasks") => {
            let refuse = *m.refuse_persona.lock().unwrap();
            if let Some(code) = refuse.filter(|_| b.get("persona").is_some()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"code": code, "error": "SECRET runtime sentence"})),
                )
                    .into_response();
            }
            let n = m.tasks.lock().unwrap().len();
            let task = json!({"id": format!("tsk_{n}"), "bot": b["bot"], "requested_by": b["requested_by"],
                              "source": "web", "text": b["text"], "status": "queued",
                              "persona": b.get("persona").cloned().unwrap_or(Value::Null),
                              "created_at": format!("2026-11-21T10:{n:02}:00Z"), "updated_at": "2026-11-21T10:00:00Z"});
            m.tasks.lock().unwrap().insert(0, task.clone());
            (StatusCode::CREATED, Json(task)).into_response()
        }
        ("GET", "/v1/tasks") => {
            // R1's `?persona=<key>|nudge` filter, newest first, and `limit`.
            let q: Vec<(String, String)> = reqwest::Url::parse(&format!("http://x{uri}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
            let get = |k: &str| q.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
            let limit: usize = get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
            let tasks: Vec<Value> = m
                .tasks
                .lock()
                .unwrap()
                .iter()
                .filter(|t| match get("persona").as_deref() {
                    None => true,
                    Some("nudge") => t["persona"].is_null(),
                    Some(p) => t["persona"] == json!(p),
                })
                .take(limit)
                .cloned()
                .collect();
            Json(tasks).into_response()
        }
        ("GET", "/v1/routines") => Json(m.routines.lock().unwrap().clone()).into_response(),
        ("PATCH", path) if path.starts_with("/v1/routines/") => {
            if m.routines_down.load(Ordering::SeqCst) {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let id = path.trim_start_matches("/v1/routines/");
            let mut all = m.routines.lock().unwrap();
            let Some(r) = all.iter_mut().find(|r| r["id"] == json!(id)) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Some(p) = b.get("persona") {
                r["persona"] = if p == "nudge" || p.is_null() {
                    Value::Null
                } else {
                    p.clone()
                };
            }
            if let Some(e) = b.get("enabled") {
                r["enabled"] = e.clone();
            }
            Json(r.clone()).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn routine(id: &str, persona: Option<&str>) -> Value {
    json!({"id": id, "bot": "pm-ana", "owner": "ana", "enabled": true,
           "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"}, "prompt": "Fake: brief me",
           "output": {"kind": "dm"}, "approval": "draft", "next_run_at": "2026-11-24T08:00:00Z",
           "persona": persona})
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
    m: Arc<Mocks>,
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
    let m = Arc::new(Mocks::default());
    m.directory.seed_team(TEAM);
    m.offer(vec![chief(), sprout()]);
    m.bots.not_allowed.lock().unwrap().push("audrey".into());
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(m.clone())).await;
    let (bots_url, stop_bots) = serve(Router::new().fallback(runtime).with_state(m.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url,
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
        m,
        http: reqwest::Client::new(),
        _dir: dir,
        _stop: vec![stop_tools, stop_bots, stop_web],
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
        let mut req = self.http.get(format!("{}{path}", self.base));
        if !cookie.is_empty() {
            req = req.header("cookie", format!("__Host-pmos={cookie}"));
        }
        reply(req.send().await.unwrap()).await
    }

    /// A write as the app sends it: same origin (unless `origin` says otherwise), the
    /// session's CSRF token (unless `csrf` is false), JSON.
    async fn write(
        &self,
        cookie: &str,
        method: Method,
        path: &str,
        body: &Value,
        origin: Option<&str>,
        csrf: bool,
    ) -> Reply {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .json(body);
        if !cookie.is_empty() {
            req = req.header("cookie", format!("__Host-pmos={cookie}"));
            if csrf {
                let me = self.get(cookie, "/api/me").await;
                req = req.header("x-pmos-csrf", me.body["csrf_token"].as_str().unwrap());
            }
        }
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        reply(req.send().await.unwrap()).await
    }

    async fn put(&self, cookie: &str, path: &str, body: &Value) -> Reply {
        let base = self.base.clone();
        self.write(cookie, Method::PUT, path, body, Some(&base), true)
            .await
    }

    async fn post(&self, cookie: &str, path: &str, body: &Value) -> Reply {
        let base = self.base.clone();
        self.write(cookie, Method::POST, path, body, Some(&base), true)
            .await
    }
}

/// The subject of a fake bot route call's assertion, checked against the request as sent.
fn subject(call: &support::BotsCall) -> String {
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

fn card<'a>(r: &'a Reply, key: &str) -> &'a Value {
    r.body["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["key"] == json!(key))
        .unwrap_or_else(|| panic!("{key} in {}", r.text))
}

#[tokio::test]
async fn the_bots_page_and_team_changes_by_role() {
    let w = web().await;
    for who in ["ana", "rex", "adam"] {
        let c = w.login(who).await;
        let r = w.get(&c, "/api/bots").await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.text);
        let keys: Vec<&str> = r.body["bots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["nudge", "chief", "sprout"], "{who}");
        let r = w
            .put(
                &c,
                "/api/me/team",
                &json!({"add": ["chief"], "via": "bots_page"}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{who}: {}", r.text);
        assert_eq!(card(&r, "chief")["on_team"], true, "{who}");
        assert_eq!(w.m.bots.team_rows(who), ["chief"], "{who}");
    }
    // An Auditor has no team: refused here, before any team call.
    let c = w.login("audrey").await;
    let before = w.m.bots.calls().len();
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::FORBIDDEN, &json!("not_allowed"))
    );
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"add": ["chief"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::FORBIDDEN, &json!("not_allowed"))
    );
    let r = w
        .post(&c, "/api/tasks", &json!({"text": "hi", "persona": "chief"}))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.body["team"], json!([]));
    assert_eq!(me.body["ask_persona"], Value::Null);
    assert_eq!(me.body["show_team_step"], false);
    assert_eq!(me.body["show_team_note"], false);
    assert_eq!(
        w.m.bots.calls().len(),
        before,
        "an auditor's requests never read a team"
    );
    // Signed out.
    let r = w.get("", "/api/bots").await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::UNAUTHORIZED, &json!("signed_out"))
    );
    let r = w
        .write(
            "",
            Method::PUT,
            "/api/me/team",
            &json!({"add": ["chief"], "via": "bots_page"}),
            Some(&w.base.clone()),
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_pm_reads_and_changes_only_their_own_team_signed_as_themselves() {
    let w = web().await;
    w.m.put_on_team("ben", "sprout");
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(
        card(&r, "sprout")["on_team"],
        false,
        "ben's team isn't ana's"
    );
    w.put(
        &c,
        "/api/me/team",
        &json!({"add": ["chief"], "via": "onboarding"}),
    )
    .await;
    // Nothing names another person's team: a handle in the body is refused, and there is no
    // route with a handle.
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"add": ["chief"], "via": "onboarding", "handle": "ben"}),
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::BAD_REQUEST, &json!("bad_request"))
    );
    for path in ["/api/team/ben", "/api/me/team/ben", "/api/bots/ben"] {
        let r = w
            .put(&c, path, &json!({"add": ["chief"], "via": "bots_page"}))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(w.m.bots.team_rows("ben"), ["sprout"]);
    let team_calls: Vec<_> =
        w.m.bots
            .calls()
            .into_iter()
            .filter(|c| c.uri.starts_with("/internal/team/"))
            .collect();
    assert!(!team_calls.is_empty());
    for call in &team_calls {
        assert_eq!(call.uri, "/internal/team/ana", "{call:?}");
        assert_eq!(subject(call), "ana");
    }
    // The change is forwarded as the tool server takes it.
    let put = team_calls.iter().find(|c| c.method == "PUT").unwrap();
    let sent: Value = serde_json::from_slice(&put.body).unwrap();
    assert_eq!(
        sent,
        json!({"add": ["chief"], "remove": [], "via": "onboarding"})
    );
}

#[tokio::test]
async fn an_ask_with_a_bot_not_on_the_team_never_reaches_the_runtime() {
    let w = web().await;
    let c = w.login("ana").await;
    let cases = [
        (
            json!({"text": "Prep me", "persona": "chief"}),
            StatusCode::CONFLICT,
            "bot_not_on_team",
        ),
        (
            json!({"text": "Prep me", "persona": "Not A Key"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
    ];
    for (body, status, code) in cases {
        let r = w.post(&c, "/api/tasks", &body).await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (status, code),
            "{}",
            r.text
        );
    }
    // On the team but withdrawn from the catalog: refused too.
    w.m.put_on_team("ana", "ghost");
    let r = w
        .post(
            &c,
            "/api/tasks",
            &json!({"text": "Prep me", "persona": "ghost"}),
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::CONFLICT, &json!("bot_unavailable"))
    );
    // The team can't be read: refused, closed.
    w.m.bots.set_down(true);
    w.m.put_on_team("ana", "chief");
    let r = w
        .post(
            &c,
            "/api/tasks",
            &json!({"text": "Prep me", "persona": "chief"}),
        )
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::SERVICE_UNAVAILABLE, &json!("unavailable"))
    );
    w.m.bots.set_down(false);
    assert!(
        w.m.calls("runtime").iter().all(|c| c.method != "POST"),
        "no task was made: {:?}",
        w.m.calls("runtime")
    );
    // On the team: the runtime gets the persona; Nudge goes as no persona at all.
    let r = w
        .post(
            &c,
            "/api/tasks",
            &json!({"text": "Prep me", "persona": "chief"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["persona"], "chief");
    assert_eq!(r.body["persona_offered"], true);
    for body in [
        json!({"text": "Hi"}),
        json!({"text": "Hi", "persona": "nudge"}),
    ] {
        let r = w.post(&c, "/api/tasks", &body).await;
        assert_eq!(r.status, StatusCode::CREATED);
        assert_eq!(r.body["persona"], Value::Null);
    }
    let posts: Vec<Value> =
        w.m.calls("runtime")
            .into_iter()
            .filter(|c| c.method == "POST")
            .map(|c| c.body)
            .collect();
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[0]["persona"], "chief");
    assert_eq!(posts[0]["bot"], "pm-ana");
    assert!(posts[1].get("persona").is_none() && posts[2].get("persona").is_none());
    // The runtime refusing the persona is `bot_unavailable`, in pmos-web's own words.
    // The runtime's refusals of the persona, in pmos-web's own words.
    for (code, want) in [
        ("unknown_persona", "bot_unavailable"),
        ("persona_needs_personal_bot", "task_refused"),
    ] {
        *w.m.refuse_persona.lock().unwrap() = Some(code);
        let r = w
            .post(
                &c,
                "/api/tasks",
                &json!({"text": "Prep me", "persona": "chief"}),
            )
            .await;
        assert_eq!(
            (r.status, &r.body["code"]),
            (StatusCode::CONFLICT, &json!(want)),
            "{code}"
        );
        assert!(!r.text.contains("SECRET"));
    }
}

#[tokio::test]
async fn the_picker_remembers_the_last_bot_and_falls_back_to_nudge() {
    let w = web().await;
    let c = w.login("ana").await;
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.body["ask_persona"], "nudge");
    assert_eq!(me.body["team"], json!(["nudge"]));
    w.put(
        &c,
        "/api/me/team",
        &json!({"add": ["chief"], "via": "picker"}),
    )
    .await;
    let r = w
        .post(
            &c,
            "/api/tasks",
            &json!({"text": "Prep me", "persona": "chief"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.body["ask_persona"], "chief");
    assert_eq!(me.body["team"], json!(["nudge", "chief"]));
    // Removed from the team: the default falls back to Nudge.
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["chief"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.body["ask_persona"], "nudge");
    // Set directly: only a bot on the team.
    let r = w
        .put(&c, "/api/me/settings", &json!({"ask_persona": "sprout"}))
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::CONFLICT, &json!("bot_not_on_team"))
    );
    w.put(
        &c,
        "/api/me/team",
        &json!({"add": ["sprout"], "via": "bots_page"}),
    )
    .await;
    let r = w
        .put(&c, "/api/me/settings", &json!({"ask_persona": "sprout"}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["ask_persona"], "sprout");
    let r = w
        .put(&c, "/api/me/settings", &json!({"ask_persona": "nudge"}))
        .await;
    assert_eq!(r.body["ask_persona"], "nudge");
    // The catalog withdraws Sprout: it isn't on the offered team, so not the default either.
    w.put(&c, "/api/me/settings", &json!({"ask_persona": "sprout"}))
        .await;
    w.m.offer(vec![chief()]);
    // (attribution may reuse a catalog read for 10 seconds; the team read is fresh)
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.body["team"], json!(["nudge"]));
    assert_eq!(me.body["ask_persona"], "nudge");
}

#[tokio::test]
async fn removing_a_bot_moves_its_routines_before_the_team_changes() {
    let w = web().await;
    w.m.put_on_team("ana", "chief");
    *w.m.routines.lock().unwrap() = vec![
        routine("rtn_1", Some("chief")),
        routine("rtn_2", None),
        routine("rtn_3", Some("sprout")),
    ];
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(card(&r, "chief")["routines"][0]["id"], "rtn_1");
    assert_eq!(card(&r, "nudge")["routines"][0]["id"], "rtn_2");
    assert_eq!(card(&r, "chief")["routines"].as_array().unwrap().len(), 1);

    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["chief"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let patches: Vec<Call> =
        w.m.calls("runtime")
            .into_iter()
            .filter(|c| c.method == "PATCH")
            .collect();
    assert_eq!(patches.len(), 1, "only chief's routine: {patches:?}");
    assert_eq!(patches[0].uri, "/v1/routines/rtn_1");
    assert_eq!(
        patches[0].body,
        json!({"persona": null}),
        "R1: null is Nudge"
    );
    let put = w.m.team_puts();
    assert_eq!(put.len(), 1);
    assert!(
        patches[0].seq < put[0].seq,
        "the routines first, then the team"
    );
    assert!(w.m.bots.team_rows("ana").is_empty());
    // The answer: the routine is Nudge's now.
    assert_eq!(card(&r, "chief")["on_team"], false);
    assert_eq!(card(&r, "nudge")["routines"].as_array().unwrap().len(), 2);

    // Pause instead.
    w.m.put_on_team("ana", "sprout");
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["sprout"], "via": "bots_page", "routines": "pause"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let last =
        w.m.calls("runtime")
            .into_iter()
            .rfind(|c| c.method == "PATCH")
            .unwrap();
    assert_eq!(
        (last.uri.as_str(), &last.body),
        ("/v1/routines/rtn_3", &json!({"enabled": false}))
    );

    // The runtime can't move them: the team is left as it was.
    w.m.put_on_team("ana", "chief");
    w.m.routines.lock().unwrap()[0]["persona"] = json!("chief");
    w.m.routines_down.store(true, Ordering::SeqCst);
    let puts = w.m.team_puts().len();
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["chief"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.text);
    assert_eq!(r.body["code"], "upstream_unavailable");
    assert_eq!(w.m.team_puts().len(), puts, "no team change was sent");
    assert_eq!(w.m.bots.team_rows("ana"), ["chief"]);
    // `routines` is only `nudge` or `pause`.
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["chief"], "via": "bots_page", "routines": "delete"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn team_changes_are_checked_before_anything_is_sent() {
    let w = web().await;
    let c = w.login("ana").await;
    let cases = [
        (
            json!({"add": ["nudge"], "via": "bots_page"}),
            StatusCode::CONFLICT,
            "always_on",
        ),
        (
            json!({"remove": ["nudge"], "via": "bots_page"}),
            StatusCode::CONFLICT,
            "always_on",
        ),
        (
            json!({"add": ["ghost"], "via": "bots_page"}),
            StatusCode::CONFLICT,
            "bot_unavailable",
        ),
        (
            json!({"add": ["chief"], "via": "seed"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!({"add": ["chief"]}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!({"via": "bots_page"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!({"add": ["chief"], "remove": ["chief"], "via": "bots_page"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!({"add": ["Chief!"], "via": "bots_page"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
    ];
    for (body, status, code) in cases {
        let r = w.put(&c, "/api/me/team", &body).await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (status, code),
            "{body}: {}",
            r.text
        );
    }
    assert!(w.m.team_puts().is_empty());
    // The tool server's refusals keep their codes, in pmos-web's words.
    for (status, code) in [
        (429, "rate_limited"),
        (409, "team_full"),
        (409, "bot_unavailable"),
        (503, "assertion_required"),
        (403, "not_a_member"),
    ] {
        *w.m.bots.refuse_change.lock().unwrap() = Some((status, code.into()));
        let r = w
            .put(
                &c,
                "/api/me/team",
                &json!({"add": ["chief"], "via": "bots_page"}),
            )
            .await;
        assert_eq!(
            (r.status.as_u16(), r.body["code"].as_str().unwrap()),
            (status, code),
            "{}",
            r.text
        );
        assert_ne!(
            r.body["error"],
            json!(code),
            "never the tool server's sentence"
        );
    }
}

#[tokio::test]
async fn writes_need_this_origin_and_the_csrf_token() {
    let w = web().await;
    w.m.put_on_team("ana", "chief");
    let c = w.login("ana").await;
    let writes = [
        (
            Method::PUT,
            "/api/me/team",
            json!({"add": ["sprout"], "via": "bots_page"}),
        ),
        (
            Method::PUT,
            "/api/me/settings",
            json!({"ask_persona": "chief"}),
        ),
        (
            Method::POST,
            "/api/tasks",
            json!({"text": "Prep me", "persona": "chief"}),
        ),
    ];
    for (method, path, body) in writes {
        let before = (w.m.bots.calls().len(), w.m.calls("runtime").len());
        let r = w
            .write(
                &c,
                method.clone(),
                path,
                &body,
                Some("https://evil.example"),
                true,
            )
            .await;
        assert_eq!(
            (r.status, &r.body["code"]),
            (StatusCode::FORBIDDEN, &json!("bad_origin")),
            "{path}"
        );
        let r = w.write(&c, method.clone(), path, &body, None, true).await;
        assert_eq!(r.body["code"], "bad_origin", "{path}: no origin at all");
        let base = w.base.clone();
        let r = w.write(&c, method, path, &body, Some(&base), false).await;
        assert_eq!(
            (r.status, &r.body["code"]),
            (StatusCode::FORBIDDEN, &json!("bad_csrf")),
            "{path}"
        );
        // Only the `/api/me` reads for the CSRF token reached the tool server; no write.
        assert!(
            w.m.bots.calls()[before.0..]
                .iter()
                .all(|c| c.method == "GET"),
            "{path}"
        );
        assert_eq!(w.m.calls("runtime").len(), before.1, "{path}");
    }
    assert_eq!(w.m.bots.team_rows("ana"), ["chief"]);
}

#[tokio::test]
async fn catalog_text_passes_through_unchanged() {
    let w = web().await;
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/bots").await;
    let chief = card(&r, "chief");
    assert_eq!(chief["summary"].as_str().unwrap(), SUMMARY);
    assert_eq!(chief["examples"], self::chief()["examples"]);
    assert_eq!(
        card(&r, "sprout")["needs"][0]["label"],
        "an analytics <b>source</b>"
    );
    assert_eq!(card(&r, "sprout")["needs"][0]["met"], false);
    // Only the catalog's own fields: never the persona file's role or owner.
    assert!(
        chief.get("role").is_none() && !r.text.contains("SECRET"),
        "{}",
        r.text
    );
    for k in [
        "key",
        "display_name",
        "title",
        "mascot",
        "always_on",
        "summary",
        "examples",
        "can_do",
        "reads",
        "drafts",
        "needs",
        "on_team",
        "added_at",
        "added_via",
        "new",
        "drafts_this_month",
        "last_used_at",
        "routines",
    ] {
        assert!(chief.get(k).is_some(), "{k}");
    }
    assert_eq!(chief.as_object().unwrap().len(), 18);
    // A persona not in the catalog's shape is left out, never half-shown.
    let mut bad = sprout();
    bad["examples"] = json!("not a list");
    w.m.offer(vec![self::chief(), bad]);
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(r.body["bots"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_bots_page_counts_and_a_withdrawn_bot_on_the_team() {
    let w = web().await;
    w.m.put_on_team("ana", "chief");
    w.m.put_on_team("ana", "ghost");
    w.m.bots
        .drafts
        .lock()
        .unwrap()
        .insert("ana".into(), json!({"nudge": 4, "chief": 6, "ghost": 1}));
    *w.m.tasks.lock().unwrap() = vec![
        json!({"id": "tsk_3", "bot": "pm-ana", "requested_by": "ana", "status": "done", "persona": "chief", "created_at": "2026-11-21T09:00:00Z"}),
        json!({"id": "tsk_2", "bot": "pm-ana", "requested_by": "ana", "status": "done", "persona": null, "created_at": "2026-11-20T09:00:00Z"}),
        json!({"id": "tsk_1", "bot": "pm-ana", "requested_by": "ana", "status": "done", "persona": "chief", "created_at": "2026-11-19T09:00:00Z"}),
    ];
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let chief = card(&r, "chief");
    assert_eq!(chief["drafts_this_month"], 6);
    assert_eq!(chief["last_used_at"], "2026-11-21T09:00:00Z");
    assert_eq!(chief["added_at"], "2026-11-20T09:01:00Z");
    assert_eq!(chief["added_via"], "bots_page");
    assert_eq!(chief["new"], false);
    let nudge = card(&r, "nudge");
    assert_eq!(
        (nudge["on_team"].clone(), nudge["added_at"].clone()),
        (json!(true), Value::Null)
    );
    assert_eq!(nudge["drafts_this_month"], 4);
    assert_eq!(nudge["last_used_at"], "2026-11-20T09:00:00Z");
    let sprout = card(&r, "sprout");
    assert_eq!(
        (
            sprout["on_team"].clone(),
            sprout["drafts_this_month"].clone(),
            sprout["last_used_at"].clone()
        ),
        (json!(false), json!(0), Value::Null)
    );
    assert_eq!(
        r.body["withdrawn"],
        json!([{"key": "ghost", "added_at": "2026-11-20T09:01:00Z",
        "added_via": "bots_page", "drafts_this_month": 1, "last_used_at": null, "routines": []}])
    );
    // A withdrawn bot can only be removed.
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"remove": ["ghost"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["withdrawn"], json!([]));
}

#[tokio::test]
async fn drafts_tasks_and_reviews_say_which_bot_wrote_them() {
    let w = web().await;
    let item = |id: &str, persona: Option<&str>| {
        json!({"id": id, "user": "ana", "bot": "pm-ana", "run_id": "tsk_1", "tool": "jira.create_issue",
               "args": {"project": "PM", "issue_type": "Story", "summary": "Fake"}, "args_hash": HASH,
               "status": "pending", "created_at": format!("2026-11-20T09:0{}:00Z", &id[4..]), "persona": persona})
    };
    *w.m.approvals.lock().unwrap() = vec![
        item("apr_1", None),
        item("apr_2", Some("chief")),
        item("apr_3", Some("ghost")),
    ];
    let c = w.login("ana").await;
    let r = w.get(&c, "/api/drafts").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let by_id = |id: &str| {
        r.body["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == json!(id))
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (
            by_id("apr_1")["persona"].clone(),
            by_id("apr_1")["persona_offered"].clone()
        ),
        (Value::Null, json!(true))
    );
    assert_eq!(
        (
            by_id("apr_2")["persona"].clone(),
            by_id("apr_2")["persona_offered"].clone()
        ),
        (json!("chief"), json!(true))
    );
    assert_eq!(
        (
            by_id("apr_3")["persona"].clone(),
            by_id("apr_3")["persona_offered"].clone()
        ),
        (json!("ghost"), json!(false)),
        "a withdrawn bot's key, flagged"
    );
    // Tasks, in the list (Activity).
    *w.m.tasks.lock().unwrap() = vec![
        json!({"id": "tsk_2", "bot": "pm-ana", "requested_by": "ana", "status": "done", "persona": "ghost", "created_at": "2026-11-21T09:00:00Z"}),
        json!({"id": "tsk_1", "bot": "pm-ana", "requested_by": "ana", "status": "done", "created_at": "2026-11-20T09:00:00Z"}),
    ];
    let r = w.get(&c, "/api/tasks").await;
    let tasks = r.body["tasks"].as_array().unwrap();
    assert_eq!(
        (
            tasks[0]["persona"].clone(),
            tasks[0]["persona_offered"].clone()
        ),
        (json!("ghost"), json!(false))
    );
    assert_eq!(
        (
            tasks[1]["persona"].clone(),
            tasks[1]["persona_offered"].clone()
        ),
        (Value::Null, json!(true))
    );
    // A reviewer's views: metadata, so kept for a restricted draft.
    let c = w.login("rex").await;
    let r = w.get(&c, "/api/reviews").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let drafts = r.body["drafts"].as_array().unwrap();
    assert_eq!(
        (
            drafts[0]["persona"].clone(),
            drafts[0]["persona_offered"].clone()
        ),
        (json!("chief"), json!(true))
    );
    assert_eq!(
        (
            drafts[1]["persona"].clone(),
            drafts[1]["persona_offered"].clone(),
            drafts[1]["restricted"].clone()
        ),
        (json!("ghost"), json!(false), json!(true))
    );
    assert_eq!(drafts[2]["persona"], Value::Null);
    let r = w.get(&c, "/api/reviews/apr_r2").await;
    assert_eq!(
        (r.body["persona"].clone(), r.body["persona_offered"].clone()),
        (json!("ghost"), json!(false))
    );
}

#[tokio::test]
async fn me_shows_the_team_step_and_the_note_on_their_conditions() {
    let w = web().await;
    // A new PM: the step.
    let c = w.login("ana").await;
    let me = w.get(&c, "/api/me").await;
    assert_eq!(
        (
            me.body["show_team_step"].clone(),
            me.body["show_team_note"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(me.body["team_unavailable"], false);
    let r = w
        .post(&c, "/api/me/hints", &json!({"seen": "team_skipped"}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["show_team_step"], false);
    // Reviewers and admins may ask: the step too.
    for who in ["rex", "adam"] {
        let c = w.login(who).await;
        assert_eq!(
            w.get(&c, "/api/me").await.body["show_team_step"],
            true,
            "{who}"
        );
    }
    // Nothing offered besides Nudge: neither.
    w.m.offer(vec![]);
    let c = w.login("ben").await;
    let me = w.get(&c, "/api/me").await;
    assert_eq!(
        (
            me.body["show_team_step"].clone(),
            me.body["show_team_note"].clone()
        ),
        (json!(false), json!(false))
    );
}

#[tokio::test]
async fn the_tool_server_unreachable_fails_closed() {
    let w = web().await;
    let c = w.login("ana").await;
    w.m.bots.set_down(true);
    let r = w.get(&c, "/api/bots").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.text);
    assert_eq!(r.body["code"], "upstream_unavailable");
    let r = w
        .put(
            &c,
            "/api/me/team",
            &json!({"add": ["chief"], "via": "bots_page"}),
        )
        .await;
    assert_eq!(r.body["code"], "upstream_unavailable");
    assert!(w.m.team_puts().is_empty());
    let r = w
        .put(&c, "/api/me/settings", &json!({"ask_persona": "chief"}))
        .await;
    assert_eq!(
        (r.status, &r.body["code"]),
        (StatusCode::SERVICE_UNAVAILABLE, &json!("unavailable"))
    );
    // `/api/me` still answers, says it couldn't read the team, and shows nothing new.
    let me = w.get(&c, "/api/me").await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["team_unavailable"], true);
    assert_eq!(me.body["team"], json!(["nudge"]));
    assert_eq!(me.body["ask_persona"], "nudge");
    assert_eq!(
        (
            me.body["show_team_step"].clone(),
            me.body["show_team_note"].clone()
        ),
        (json!(false), json!(false))
    );
    assert!(w.m.calls("runtime").iter().all(|c| c.method == "GET"));
}
