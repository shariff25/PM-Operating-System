//! `pmos-web` against a mock bot runtime and a mock tool server: sign-in and sessions, CSRF
//! and origin checks, membership, every drafts route, tasks, roles, and that no service token
//! ever reaches the browser.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use pmos_web::followups::BriefConfig;
use pmos_web::session::MemorySessionStore;
use pmos_web::settings::MemorySettingsStore;
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";

const TEAM: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, roles: [pm]}
  - {handle: audrey, slack-id: U0AUD, roles: [auditor]}
";

/// One call an upstream saw.
#[derive(Debug, Clone)]
struct Call {
    method: String,
    uri: String,
    auth: String,
    user: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Upstream {
    calls: Mutex<Vec<Call>>,
    /// Tasks (runtime) or approval items (tool server).
    data: Mutex<Vec<Value>>,
    next: AtomicUsize,
}

impl Upstream {
    fn record(&self, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) {
        let header = |k: &str| {
            headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        self.calls.lock().unwrap().push(Call {
            method: method.to_string(),
            uri: uri.to_string(),
            auth: header("authorization").unwrap_or_default(),
            user: header("x-pmos-user"),
            body: serde_json::from_slice(body).unwrap_or(Value::Null),
        });
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method != "GET")
            .collect()
    }

    fn set(&self, id: &str, key: &str, value: Value) {
        let mut data = self.data.lock().unwrap();
        let item = data.iter_mut().find(|t| t["id"] == json!(id)).unwrap();
        item[key] = value;
    }

    fn find(&self, id: &str) -> Option<Value> {
        self.data
            .lock()
            .unwrap()
            .iter()
            .find(|t| t["id"] == json!(id))
            .cloned()
    }
}

fn query(uri: &Uri, key: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

fn task_json(id: &str, who: &str, text: &str) -> Value {
    json!({"id": id, "bot": format!("pm-{who}"), "requested_by": who, "source": "slack",
           "priority": "interactive", "text": text, "status": "done", "checklist": [],
           "result": "Here it is", "error": null, "approvals": ["apr_conf"], "slack": null,
           "routine_id": null, "attempts": 1,
           "created_at": "2026-11-14T09:00:00Z", "updated_at": "2026-11-14T09:01:00Z"})
}

async fn runtime(
    State(up): State<Arc<Upstream>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    up.record(&method, &uri, &headers, &body);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {BOTS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let user = headers
        .get("x-pmos-user")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let path: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
    match (method.as_str(), path.as_slice()) {
        ("POST", ["v1", "tasks"]) => {
            let req: Value = serde_json::from_slice(&body).unwrap();
            if req["requested_by"] != json!(user) {
                return StatusCode::FORBIDDEN.into_response();
            }
            let n = up.next.fetch_add(1, Ordering::SeqCst);
            let mut task = task_json(&format!("tsk_web{n}"), &user, req["text"].as_str().unwrap());
            task["bot"] = req["bot"].clone();
            task["source"] = req["source"].clone();
            task["status"] = json!("queued");
            task["result"] = Value::Null;
            task["approvals"] = json!([]);
            task["created_at"] = json!(format!("2026-11-16T10:00:0{n}Z"));
            up.data.lock().unwrap().insert(0, task.clone());
            (StatusCode::CREATED, Json(task)).into_response()
        }
        ("GET", ["v1", "tasks"]) => {
            let who = query(&uri, "requested_by").unwrap_or_default();
            let tasks: Vec<Value> = up
                .data
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t["requested_by"] == json!(who))
                .cloned()
                .collect();
            Json(tasks).into_response()
        }
        // Like the console client, this mock serves any task: pmos-web must check the owner.
        ("GET", ["v1", "tasks", id]) => match up.find(id) {
            Some(t) => Json(t).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        // A task's own `_events` when a test set them, else two.
        ("GET", ["v1", "tasks", id, "events"]) => {
            match up.find(id).and_then(|t| t.get("_events").cloned()) {
                Some(events) => Json(events).into_response(),
                None => Json(json!([
                    {"at": "2026-11-14T09:00:01Z", "kind": "milestone", "text": "Reading Jira"},
                    {"at": "2026-11-14T09:00:02Z", "kind": "checklist", "text": ""},
                ]))
                .into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn item(id: &str, user: &str, tool: &str, args: Value, created: &str) -> Value {
    json!({"id": id, "user": user, "skill": null, "bot": format!("pm-{user}"),
           "run_id": "tsk_ana1", "tool": tool, "args": args, "args_hash": "h",
           "status": "pending", "created_at": created})
}

fn items() -> Vec<Value> {
    let mut done = item(
        "apr_done",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "issue_type": "Story", "summary": "Done one"}),
        "2026-11-10T09:00:00Z",
    );
    done["status"] = json!("approved");
    done["decided_at"] = json!("2026-11-10T10:00:00Z");
    done["result"] = json!({"url": "https://acme.atlassian.net/browse/PAY-9"});
    vec![
        item(
            "apr_conf",
            "ana",
            "confluence.create_draft",
            json!({"space": "PM", "title": "PRD: TCB grace period", "body": "# PRD\nOld text",
                   "parent_id": "77"}),
            "2026-11-16T09:06:00Z",
        ),
        item(
            "apr_jira",
            "ana",
            "jira.create_issue",
            json!({"project": "PAY", "issue_type": "Story", "summary": "Grace period",
                   "description": "As a PM…"}),
            "2026-11-16T09:05:00Z",
        ),
        item(
            "apr_slack",
            "ana",
            "slack.post_reply",
            json!({"channel": "C01", "thread_ts": "1731.0002", "text": "On it"}),
            "2026-11-16T09:04:00Z",
        ),
        item(
            "apr_ext",
            "ana",
            "slack.send_message",
            json!({"channel": "C0EXT", "text": "Hello partners"}),
            "2026-11-16T09:03:00Z",
        ),
        item(
            "apr_blocked",
            "ana",
            "slack.send_message",
            json!({"channel": "C0OUT", "text": "Restricted"}),
            "2026-11-16T09:02:00Z",
        ),
        item(
            "apr_expired",
            "ana",
            "slack.post_reply",
            json!({"channel": "C01", "thread_ts": "1.2", "text": "Late"}),
            "2026-11-16T09:01:00Z",
        ),
        item(
            "apr_race",
            "ana",
            "confluence.update_page",
            json!({"page_id": "9", "body": "Race"}),
            "2026-11-16T09:00:30Z",
        ),
        item(
            "apr_bet",
            "ana",
            "bets.record",
            json!({"key": "b1"}),
            "2026-11-16T09:00:10Z",
        ),
        done,
        item(
            "apr_ben",
            "ben",
            "confluence.create_draft",
            json!({"space": "PM", "title": "Ben's", "body": "private"}),
            "2026-11-16T09:00:00Z",
        ),
    ]
}

fn coded(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"code": code, "error": format!("{code} from the tool server")})),
    )
        .into_response()
}

async fn toolserver(
    State(up): State<Arc<Upstream>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    up.record(&method, &uri, &headers, &body);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
    match (method.as_str(), path.as_slice()) {
        ("GET", ["internal", "approvals"]) => {
            let user = query(&uri, "user").unwrap_or_default();
            let mut list: Vec<Value> = up
                .data
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i["user"] == json!(user))
                .cloned()
                .collect();
            list.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
            Json(json!({ "items": list })).into_response()
        }
        // Like the tool server: only for the item's own user.
        ("GET", ["internal", "approvals", id, "sources"]) => {
            let user = query(&uri, "user").unwrap_or_default();
            match up.find(id) {
                Some(i) if i["sources_down"] == json!(true) => {
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
                Some(i) if i["user"] == json!(user) => {
                    let sources = if *id == "apr_conf" {
                        json!([
                            {"system": "jira", "title": "PAY-212", "ref": "PAY-212",
                             "url": "https://acme.atlassian.net/browse/PAY-212",
                             "tool": "jira.read", "read_at": "2026-11-16T09:05:00Z"},
                            {"system": "docs", "title": "grace period", "ref": "https://docs.example.com/grace.md",
                             "url": "https://docs.example.com/grace.md",
                             "tool": "docs.read", "read_at": "2026-11-16T09:04:00Z"},
                            {"system": "slack", "title": "Slack thread in C01", "ref": "C01:1731.0002",
                             "url": "javascript:alert(1)",
                             "tool": "slack.read_thread", "read_at": "2026-11-16T09:03:00Z"},
                            {"system": "context", "title": "Pricing principles",
                             "ref": "context/products/pricing.md", "url": null,
                             "tool": "context.read", "read_at": "2026-11-16T09:02:00Z"},
                        ])
                    } else {
                        json!([])
                    };
                    Json(json!({"sources": sources, "available": true, "truncated": false}))
                        .into_response()
                }
                _ => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "no such approval item"})),
                )
                    .into_response(),
            }
        }
        ("GET", ["internal", "approvals", id]) => match up.find(id) {
            Some(i) => Json(i).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no such approval item"})),
            )
                .into_response(),
        },
        ("POST", ["internal", "approvals", id, "decision"]) => {
            let Some(it) = up.find(id) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let req: Value = serde_json::from_slice(&body).unwrap();
            if it["status"] != json!("pending") {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": "already decided"})),
                )
                    .into_response();
            }
            if req["decided_by"] != it["user"] {
                return StatusCode::FORBIDDEN.into_response();
            }
            let decision = req["decision"].as_str().unwrap_or_default();
            if decision != "reject" {
                match *id {
                    "apr_ext" if req["confirm_external"] != json!(true) => {
                        return coded(StatusCode::CONFLICT, "needs_external_confirm")
                    }
                    "apr_blocked" => return coded(StatusCode::CONFLICT, "audience_blocked"),
                    "apr_expired" => return coded(StatusCode::CONFLICT, "expired"),
                    "apr_race" => {
                        // Decided in Slack a moment ago.
                        up.set(id, "status", json!("approved"));
                        return (StatusCode::CONFLICT, Json(json!({"error": "not pending"})))
                            .into_response();
                    }
                    "apr_dbl" => {
                        // The same PM's other approve won a moment ago, with these args.
                        up.set(id, "status", json!("approved"));
                        up.set(id, "decided_by", req["decided_by"].clone());
                        if decision == "edit" {
                            up.set(id, "args", req["args"].clone());
                        }
                        return (StatusCode::CONFLICT, Json(json!({"error": "not pending"})))
                            .into_response();
                    }
                    _ => {}
                }
            }
            match decision {
                "approve" | "edit" => {
                    if decision == "edit" {
                        up.set(id, "args", req["args"].clone());
                    }
                    up.set(id, "status", json!("approved"));
                    up.set(
                        id,
                        "result",
                        json!({"url": "https://acme.atlassian.net/wiki/pages/1"}),
                    );
                }
                _ => up.set(id, "status", json!("rejected")),
            }
            up.set(id, "decided_by", req["decided_by"].clone());
            up.set(id, "decided_at", json!("2026-11-16T11:00:00Z"));
            Json(up.find(id).unwrap()).into_response()
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

fn start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-16T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

struct Web {
    base: String,
    runtime: Arc<Upstream>,
    tools: Arc<Upstream>,
    http: reqwest::Client,
    clock: Arc<Mutex<DateTime<Utc>>>,
    dir: tempfile::TempDir,
    /// Every response body and header value the browser got.
    seen: Mutex<Vec<String>>,
    _stop: Vec<CancellationToken>,
}

fn config(bots_url: &str, tools_url: &str, root: &std::path::Path) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url: bots_url.into(),
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url.into(),
        toolserver_token: TOOLS_TOKEN.into(),
        content_root: root.to_path_buf(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
    }
}

async fn web() -> Web {
    web_with(|_| {}).await
}

async fn web_with(change: impl FnOnce(&mut Config)) -> Web {
    web_full(change, BriefConfig::default()).await
}

async fn web_full(change: impl FnOnce(&mut Config), brief: BriefConfig) -> Web {
    let runtime_up = Arc::new(Upstream::default());
    runtime_up.data.lock().unwrap().extend([
        task_json("tsk_ana1", "ana", "What did PAY ship?"),
        task_json("tsk_ben1", "ben", "Ben's private question"),
    ]);
    let tools_up = Arc::new(Upstream::default());
    tools_up.data.lock().unwrap().extend(items());
    let (bots_url, stop_bots) = serve(
        Router::new()
            .fallback(runtime)
            .with_state(runtime_up.clone()),
    )
    .await;
    let (tools_url, stop_tools) = serve(
        Router::new()
            .fallback(toolserver)
            .with_state(tools_up.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let mut cfg = config(&bots_url, &tools_url, dir.path());
    change(&mut cfg);
    let clock = Arc::new(Mutex::new(start_time()));
    let c = clock.clone();
    let deps = Deps {
        sessions: Arc::new(MemorySessionStore::new()),
        settings: Arc::new(MemorySettingsStore::new()),
        brief,
        clock: Arc::new(move || *c.lock().unwrap()),
        ..Deps::in_memory()
    };
    let (base, stop_web) = serve(pmos_web::build(cfg, deps).unwrap()).await;
    Web {
        base,
        runtime: runtime_up,
        tools: tools_up,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        clock,
        dir,
        seen: Mutex::new(Vec::new()),
        _stop: vec![stop_bots, stop_tools, stop_web],
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }
}

/// A browser: its cookie and CSRF token, and any extra headers it sends.
#[derive(Clone, Default)]
struct Browser {
    cookie: Option<String>,
    csrf: Option<String>,
    extra: Vec<(&'static str, String)>,
}

impl Web {
    async fn send(&self, b: &Browser, method: Method, path: &str, body: Option<Value>) -> Reply {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base));
        if let Some(c) = &b.cookie {
            req = req.header("cookie", format!("__Host-pmos={c}"));
        }
        if method != Method::GET {
            if let Some(t) = &b.csrf {
                req = req.header("x-pmos-csrf", t);
            }
        }
        for (k, v) in &b.extra {
            req = req.header(*k, v);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        {
            let mut seen = self.seen.lock().unwrap();
            seen.push(text.clone());
            for v in headers.values() {
                seen.push(String::from_utf8_lossy(v.as_bytes()).into_owned());
            }
        }
        Reply {
            status,
            headers,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }

    async fn get(&self, b: &Browser, path: &str) -> Reply {
        self.send(b, Method::GET, path, None).await
    }

    async fn post(&self, b: &Browser, path: &str, body: Value) -> Reply {
        self.send(b, Method::POST, path, Some(body)).await
    }

    /// Sign in as `handle` from a fresh browser.
    async fn login(&self, handle: &str) -> Browser {
        self.login_from(&Browser::default(), handle).await
    }

    async fn login_from(&self, b: &Browser, handle: &str) -> Browser {
        let r = self
            .post(b, "/api/auth/dev-login", json!({ "handle": handle }))
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{:?}", r.body);
        let cookie = r.headers["set-cookie"].to_str().unwrap().to_string();
        let value = cookie
            .strip_prefix("__Host-pmos=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let mut b = Browser {
            cookie: Some(value),
            ..Browser::default()
        };
        let me = self.get(&b, "/api/me").await;
        assert_eq!(me.status, StatusCode::OK);
        b.csrf = Some(me.body["csrf_token"].as_str().unwrap().to_string());
        b
    }

    fn advance(&self, by: Duration) {
        *self.clock.lock().unwrap() += by;
    }

    fn assert_no_tokens(&self) {
        for s in self.seen.lock().unwrap().iter() {
            assert!(
                !s.contains(BOTS_TOKEN) && !s.contains(TOOLS_TOKEN),
                "a token in {s}"
            );
        }
    }
}

// Start-up.

#[test]
fn refuses_to_start_without_a_sign_in_provider() {
    let err = Config::from_lookup(|k| match k {
        "PMOS_WEB_BOTS_TOKEN" => Some(BOTS_TOKEN.into()),
        "PMOS_WEB_TOOLSERVER_TOKEN" => Some(TOOLS_TOKEN.into()),
        _ => None,
    })
    .unwrap_err();
    assert!(err.contains("no sign-in provider"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config("http://127.0.0.1:1", "http://127.0.0.1:2", dir.path());
    cfg.dev_login = false;
    let err = pmos_web::build(cfg, Deps::in_memory()).unwrap_err();
    assert!(err.contains("no sign-in provider"), "{err}");
}

#[tokio::test]
async fn serves_the_app_and_its_index_for_unknown_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config("http://127.0.0.1:1", "http://127.0.0.1:2", dir.path());
    cfg.static_dir = Some(dir.path().join("dist"));
    std::fs::create_dir_all(dir.path().join("dist/assets")).unwrap();
    assert!(pmos_web::build(cfg.clone(), Deps::in_memory())
        .unwrap_err()
        .contains("index.html"));
    std::fs::write(dir.path().join("dist/index.html"), "<!doctype html>app").unwrap();
    std::fs::write(dir.path().join("dist/assets/app.js"), "console.log(1)").unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let (base, _stop) = serve(pmos_web::build(cfg, Deps::in_memory()).unwrap()).await;
    let http = reqwest::Client::new();
    let res = http
        .get(format!("{base}/drafts/apr_1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("script-src 'self'"));
    assert_eq!(res.text().await.unwrap(), "<!doctype html>app");
    let res = http
        .get(format!("{base}/assets/app.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    let res = http.get(format!("{base}/api/nope")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>().await.unwrap()["code"], "not_found");
    let res = http.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(res.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn serves_the_icon_and_never_the_app_for_api_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config("http://127.0.0.1:1", "http://127.0.0.1:2", dir.path());
    cfg.static_dir = Some(dir.path().join("dist"));
    std::fs::create_dir_all(dir.path().join("dist/assets")).unwrap();
    std::fs::write(dir.path().join("dist/index.html"), "<!doctype html>app").unwrap();
    let icon = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"/>"#;
    std::fs::write(dir.path().join("dist/favicon.svg"), icon).unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let (base, _stop) = serve(pmos_web::build(cfg, Deps::in_memory()).unwrap()).await;
    let http = reqwest::Client::new();

    let res = http
        .get(format!("{base}/favicon.svg"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "image/svg+xml");
    assert_eq!(res.headers()["cache-control"], "public, max-age=604800");
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert!(res.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("img-src 'self'"));
    assert_eq!(res.text().await.unwrap(), icon);
    // A missing icon or script is a 404, not the app's page.
    for path in ["/favicon.ico", "/assets/missing.js"] {
        let res = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        assert_ne!(res.text().await.unwrap(), "<!doctype html>app", "{path}");
    }
    // A client route still gets the app.
    let res = http
        .get(format!("{base}/tasks/tsk_1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().await.unwrap(), "<!doctype html>app");

    // Under /api, anything unknown is JSON, never index.html, whatever the method.
    for (method, path) in [
        (Method::GET, "/api"),
        (Method::GET, "/api/"),
        (Method::GET, "/api/nope/deeper"),
        (Method::GET, "/api/favicon.svg"),
        (Method::GET, "/api/index.html"),
        (Method::POST, "/api/nope"),
        (Method::DELETE, "/api/drafts/apr_1/nope"),
    ] {
        let res = http
            .request(method.clone(), format!("{base}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(res.headers()["content-type"], "application/json");
        assert_eq!(res.headers()["cache-control"], "no-store");
        assert_eq!(
            res.json::<Value>().await.unwrap()["code"],
            "not_found",
            "{method} {path}"
        );
    }
    // A known route with a method it doesn't take: JSON too.
    let res = http
        .request(Method::PATCH, format!("{base}/api/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(res.headers()["content-type"], "application/json");
    assert_eq!(
        res.json::<Value>().await.unwrap()["code"],
        "method_not_allowed"
    );
}

// Sessions.

#[tokio::test]
async fn sign_in_sets_the_host_cookie_and_rotates_it() {
    let w = web().await;
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "ana"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let cookie = r.headers["set-cookie"].to_str().unwrap();
    let (value, attrs) = cookie
        .strip_prefix("__Host-pmos=")
        .unwrap()
        .split_once(';')
        .unwrap();
    assert_eq!(value.len(), 43, "256 bits, base64url");
    assert_eq!(attrs, " Path=/; Secure; HttpOnly; SameSite=Lax");
    assert!(!cookie.to_ascii_lowercase().contains("domain"));

    let first = w.login("ana").await;
    // Signing in again from the same browser makes a new id and ends the old one.
    let second = w.login_from(&first, "ana").await;
    assert_ne!(first.cookie, second.cookie);
    assert_eq!(w.get(&first, "/api/me").await.code(), "signed_out");
    assert_eq!(w.get(&second, "/api/me").await.status, StatusCode::OK);

    // Logout ends the session and clears the cookie.
    let r = w.post(&second, "/api/auth/logout", json!(null)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(r.headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        w.get(&second, "/api/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    // Logout without a session is fine.
    let r = w
        .post(&Browser::default(), "/api/auth/logout", json!(null))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn every_route_needs_a_session() {
    let w = web().await;
    let nobody = Browser::default();
    let forged = Browser {
        cookie: Some("A".repeat(43)),
        ..Browser::default()
    };
    for b in [&nobody, &forged] {
        for path in [
            "/api/me",
            "/api/drafts",
            "/api/drafts/apr_conf",
            "/api/tasks",
            "/api/tasks/tsk_ana1",
            "/api/brief",
        ] {
            let r = w.get(b, path).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(r.code(), "signed_out");
        }
        let r = w.post(b, "/api/tasks", json!({"text": "hi"})).await;
        assert_eq!(r.code(), "signed_out");
    }
    assert!(w.runtime.calls().is_empty() && w.tools.calls().is_empty());
}

#[tokio::test]
async fn sessions_expire_when_idle_and_after_a_day() {
    let w = web().await;
    let b = w.login("ana").await;
    w.advance(Duration::hours(8));
    assert_eq!(w.get(&b, "/api/me").await.code(), "signed_out");

    let b = w.login("ana").await;
    for _ in 0..3 {
        w.advance(Duration::hours(7));
        assert_eq!(w.get(&b, "/api/me").await.status, StatusCode::OK);
    }
    w.advance(Duration::hours(3)); // 24 hours since sign-in
    assert_eq!(w.get(&b, "/api/me").await.code(), "signed_out");
}

#[tokio::test]
async fn non_members_cannot_sign_in() {
    let w = web().await;
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "mallory"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "not_a_member");
    assert!(r.headers.get("set-cookie").is_none());
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "Ana <x>"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"nope": 1}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // By email works too.
    w.login("ANA@example.com").await;
}

#[tokio::test]
async fn removing_a_member_ends_their_access_at_once() {
    let w = web().await;
    let b = w.login("ana").await;
    let team = w.dir.path().join("team.yaml");
    std::fs::write(
        &team,
        TEAM.replace(
            "  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}\n",
            "",
        ),
    )
    .unwrap();
    let r = w.get(&b, "/api/drafts").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "not_a_member");
    // The session is gone for good, even if they are added back.
    std::fs::write(&team, TEAM).unwrap();
    assert_eq!(w.get(&b, "/api/me").await.code(), "signed_out");
    assert!(w.tools.calls().is_empty());
}

#[tokio::test]
async fn writes_need_the_csrf_token_and_the_apps_origin() {
    let w = web().await;
    let ana = w.login("ana").await;
    let ben = w.login("ben").await;
    let ask = json!({"text": "What's blocked?"});
    for csrf in [None, Some("wrong".to_string()), ben.csrf.clone()] {
        let b = Browser {
            csrf,
            ..ana.clone()
        };
        let r = w.post(&b, "/api/tasks", ask.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.code(), "bad_csrf");
    }
    for extra in [
        vec![("origin", "https://evil.example".to_string())],
        vec![("origin", "null".to_string())],
        vec![("sec-fetch-site", "cross-site".to_string())],
        vec![("referer", "https://evil.example/x".to_string())],
    ] {
        let b = Browser {
            extra,
            ..ana.clone()
        };
        let r = w.post(&b, "/api/tasks", ask.clone()).await;
        assert_eq!(r.code(), "bad_origin");
        let r = w
            .send(
                &b,
                Method::PUT,
                "/api/drafts/apr_conf",
                Some(json!({"body": "x"})),
            )
            .await;
        assert_eq!(r.code(), "bad_origin");
        let r = w.send(&b, Method::DELETE, "/api/me/hints", None).await;
        assert_eq!(r.code(), "bad_origin");
    }
    // Signing in from another site is refused too.
    let evil = Browser {
        extra: vec![("origin", "https://evil.example".to_string())],
        ..Browser::default()
    };
    assert_eq!(
        w.post(&evil, "/api/auth/dev-login", json!({"handle": "ana"}))
            .await
            .code(),
        "bad_origin"
    );
    // Logout needs the token when there is a session.
    let r = w
        .post(
            &Browser {
                csrf: None,
                ..ana.clone()
            },
            "/api/auth/logout",
            json!(null),
        )
        .await;
    assert_eq!(r.code(), "bad_csrf");
    assert!(w.runtime.writes().is_empty() && w.tools.writes().is_empty());

    // The app's own origin, with the token, works.
    let own = Browser {
        extra: vec![
            ("origin", w.base.clone()),
            ("sec-fetch-site", "same-origin".into()),
        ],
        ..ana.clone()
    };
    assert_eq!(
        w.post(&own, "/api/tasks", ask).await.status,
        StatusCode::CREATED
    );
}

// Me.

#[tokio::test]
async fn me_settings_and_hints() {
    let w = web().await;
    let b = w.login("ana").await;
    let me = w.get(&b, "/api/me").await.body;
    assert_eq!(me["handle"], "ana");
    assert_eq!(me["roles"], json!(["pm"]));
    assert_eq!(me["first_visit"], true);
    assert_eq!(me["nudges"], true);
    assert_eq!(me["hints_seen"], json!([]));
    assert_eq!(me["csrf_token"].as_str().unwrap().len(), 43);

    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/me/settings",
            Some(json!({"nudges": false})),
        )
        .await;
    assert_eq!(r.body["nudges"], false);
    w.post(&b, "/api/me/hints", json!({"seen": "sources"}))
        .await;
    let r = w.post(&b, "/api/me/hints", json!({"seen": "ask"})).await;
    assert_eq!(r.body["hints_seen"], json!(["ask", "sources"]));
    let r = w
        .post(&b, "/api/me/hints", json!({"seen": "everything"}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = w.send(&b, Method::DELETE, "/api/me/hints", None).await;
    assert_eq!(r.body["hints_seen"], json!([]));
    assert_eq!(r.body["nudges"], false);

    // first_visit ends when a draft is opened.
    w.get(&b, "/api/drafts/apr_conf").await;
    assert_eq!(w.get(&b, "/api/me").await.body["first_visit"], false);
    // Each PM's settings are their own.
    let ben = w.login("ben").await;
    assert_eq!(w.get(&ben, "/api/me").await.body["nudges"], true);

    let r = w.get(&b, "/api/me").await;
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.headers["x-frame-options"], "DENY");
    assert!(r.headers["content-security-policy"]
        .to_str()
        .unwrap()
        .starts_with("default-src 'none'"));
}

// Drafts.

#[tokio::test]
async fn lists_the_pms_own_drafts_pending_first() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/drafts").await;
    assert_eq!(r.status, StatusCode::OK);
    let ids: Vec<&str> = r.body["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "apr_conf",
            "apr_jira",
            "apr_slack",
            "apr_ext",
            "apr_blocked",
            "apr_expired",
            "apr_race",
            "apr_done"
        ]
    );
    let first = &r.body["drafts"][0];
    assert_eq!(first["kind"], "confluence_page");
    assert_eq!(first["title"], "PRD: TCB grace period");
    assert_eq!(first["bot"], "pm-ana");
    assert_eq!(first["status"], "pending");
    assert_eq!(first["task_id"], "tsk_ana1");
    assert_eq!(first["practice"], false);
    assert_eq!(first["expires_at"], Value::Null);
    assert!(first.get("body").is_none(), "a summary has no body");
    let slack = &r.body["drafts"][2];
    assert_eq!(slack["kind"], "slack_reply");
    assert_eq!(slack["expires_at"], "2026-11-17T09:04:00Z");
    assert_eq!(r.body["drafts"][3]["kind"], "slack_message");
    assert_eq!(r.body["drafts"][1]["kind"], "jira_issue");
    assert_eq!(r.body["drafts"][7]["status"], "approved");
    // Asked for this PM only.
    assert!(w
        .tools
        .calls()
        .iter()
        .all(|c| c.uri == "/internal/approvals?user=ana"
            && c.auth == format!("Bearer {TOOLS_TOKEN}")));
}

#[tokio::test]
async fn reads_a_draft_and_hides_everyone_elses() {
    let w = web().await;
    let b = w.login("ana").await;
    let d = w.get(&b, "/api/drafts/apr_conf").await.body;
    assert_eq!(d["body"], "# PRD\nOld text");
    assert_eq!(d["edited"], false);
    assert_eq!(
        d["target"],
        json!({"system": "confluence", "label": "Space PM, under page 77", "url": null})
    );
    assert_eq!(d["sources"].as_array().unwrap().len(), 4);
    assert_eq!(
        w.get(&b, "/api/drafts/apr_jira").await.body["sources"],
        json!([])
    );
    assert_eq!(d["result_url"], Value::Null);
    assert_eq!(d["external"], false);
    let done = w.get(&b, "/api/drafts/apr_done").await.body;
    assert_eq!(
        done["result_url"],
        "https://acme.atlassian.net/browse/PAY-9"
    );

    for id in ["apr_ben", "apr_missing", "apr_bet", "..%2Fx", "prc_notmine"] {
        let r = w.get(&b, &format!("/api/drafts/{id}")).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(r.code(), "not_found");
    }
    // Writes to someone else's draft are 404 too, and send nothing.
    for path in ["/api/drafts/apr_ben/approve", "/api/drafts/apr_ben/reject"] {
        assert_eq!(w.post(&b, path, json!({})).await.code(), "not_found");
    }
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_ben",
            Some(json!({"body": "x"})),
        )
        .await;
    assert_eq!(r.code(), "not_found");
    assert_eq!(
        w.post(
            &b,
            "/api/drafts/apr_ben/changes",
            json!({"instruction": "x"})
        )
        .await
        .code(),
        "not_found"
    );
    assert!(w.tools.writes().is_empty() && w.runtime.writes().is_empty());
}

#[tokio::test]
async fn approve_without_an_edit_sends_approve() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.post(&b, "/api/drafts/apr_jira/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    assert_eq!(
        r.body["result_url"],
        "https://acme.atlassian.net/wiki/pages/1"
    );
    let writes = w.tools.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].uri, "/internal/approvals/apr_jira/decision");
    assert_eq!(
        writes[0].body,
        json!({"decided_by": "ana", "decision": "approve"})
    );
    // Once approved: approving again is a retry that answers the approved draft and sends
    // nothing; editing is not_pending.
    let again = w.post(&b, "/api/drafts/apr_jira/approve", json!({})).await;
    assert_eq!(again.status, StatusCode::OK, "{:?}", again.body);
    assert_eq!(again.body["status"], "approved");
    assert_eq!(w.tools.writes().len(), 1);
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_jira",
            Some(json!({"body": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "not_pending");
    assert_eq!(w.tools.writes().len(), 1);
}

#[tokio::test]
async fn approve_with_a_stored_edit_sends_edit() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_conf",
            Some(json!({"title": "PRD: grace period v2", "body": "# PRD\nNew text"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["edited"], true);
    assert_eq!(r.body["body"], "# PRD\nNew text");
    assert_eq!(r.body["title"], "PRD: grace period v2");
    // Saving sends nothing.
    assert!(w.tools.writes().is_empty());
    // The edit shows in the list and on reload.
    let list = w.get(&b, "/api/drafts").await.body;
    assert_eq!(list["drafts"][0]["title"], "PRD: grace period v2");
    assert_eq!(w.get(&b, "/api/drafts/apr_conf").await.body["edited"], true);

    let r = w.post(&b, "/api/drafts/apr_conf/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["edited"], false, "the edit is now the draft");
    assert_eq!(r.body["body"], "# PRD\nNew text");
    let writes = w.tools.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].body,
        json!({"decided_by": "ana", "decision": "edit",
               "args": {"space": "PM", "title": "PRD: grace period v2",
                        "body": "# PRD\nNew text", "parent_id": "77"}})
    );

    // A Slack edit changes `text` and never the channel.
    w.send(
        &b,
        Method::PUT,
        "/api/drafts/apr_slack",
        Some(json!({"title": "ignored", "body": "On it, today"})),
    )
    .await;
    w.post(&b, "/api/drafts/apr_slack/approve", json!(null))
        .await;
    assert_eq!(
        w.tools.writes()[1].body["args"],
        json!({"channel": "C01", "thread_ts": "1731.0002", "text": "On it, today"})
    );
}

#[tokio::test]
async fn passes_through_the_tool_servers_conflicts() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.post(&b, "/api/drafts/apr_ext/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "needs_external_confirm");
    assert_eq!(
        r.body["error"],
        "needs_external_confirm from the tool server"
    );
    // The draft now says it is external, so the app asks before confirming.
    assert_eq!(
        w.get(&b, "/api/drafts/apr_ext").await.body["external"],
        true
    );
    let r = w
        .post(
            &b,
            "/api/drafts/apr_ext/approve",
            json!({"confirm_external": true}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(w.tools.writes()[1].body["confirm_external"], true);

    for (id, code) in [
        ("apr_blocked", "audience_blocked"),
        ("apr_expired", "expired"),
    ] {
        let r = w
            .post(&b, &format!("/api/drafts/{id}/approve"), json!({}))
            .await;
        assert_eq!(r.status, StatusCode::CONFLICT);
        assert_eq!(r.code(), code);
    }
    // A conflict without a code because it was just decided elsewhere.
    let r = w.post(&b, "/api/drafts/apr_race/approve", json!({})).await;
    assert_eq!(r.code(), "not_pending");
}

#[tokio::test]
async fn rejects_with_the_fixed_reasons() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/reject",
            json!({"reason": "because"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(w.tools.writes().is_empty());
    let r = w
        .post(&b, "/api/drafts/apr_jira/reject", json!({"reason": "tone"}))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "rejected");
    assert_eq!(
        w.tools.writes()[0].body,
        json!({"decided_by": "ana", "decision": "reject", "reason": "tone"})
    );
    let r = w.post(&b, "/api/drafts/apr_conf/reject", json!(null)).await;
    assert_eq!(r.body["status"], "rejected");
    assert_eq!(
        w.tools.writes()[1].body,
        json!({"decided_by": "ana", "decision": "reject"})
    );
    // Rejecting is never audience-checked, so an external-channel draft can be rejected.
    let r = w.post(&b, "/api/drafts/apr_ext/reject", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn asks_the_bot_for_a_change_and_offers_its_text() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w
        .post(
            &b,
            "/api/drafts/apr_conf/changes",
            json!({"instruction": "shorten the risks section"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    assert_eq!(
        r.body["suggestion"],
        json!({"status": "working", "instruction": "shorten the risks section",
               "body": null, "error": null})
    );
    let created = w.runtime.writes();
    assert_eq!(created.len(), 1);
    let call = &created[0];
    assert_eq!(call.user.as_deref(), Some("ana"));
    assert_eq!(call.body["bot"], "pm-ana");
    assert_eq!(call.body["requested_by"], "ana");
    assert_eq!(call.body["source"], "web");
    let prompt = call.body["text"].as_str().unwrap();
    assert!(prompt.contains("Answer with the revised text only"));
    assert!(prompt.contains("make no new draft or approval"));
    assert!(prompt.contains("shorten the risks section"));
    assert!(prompt.contains("# PRD\nOld text"));
    let task_id = "tsk_web0";

    // One at a time.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_conf/changes",
            json!({"instruction": "again"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "change_in_progress");
    assert_eq!(w.runtime.writes().len(), 1);
    // Not ready yet: accepting is refused.
    assert_eq!(
        w.post(&b, "/api/drafts/apr_conf/suggestion/accept", json!(null))
            .await
            .code(),
        "no_suggestion"
    );

    let r = w.get(&b, "/api/drafts/apr_conf/suggestion").await;
    assert_eq!(r.body["suggestion"]["status"], "working");
    w.runtime.set(task_id, "status", json!("done"));
    w.runtime.set(
        task_id,
        "result",
        json!("```markdown\n# PRD\nShort text\n```"),
    );
    let r = w.get(&b, "/api/drafts/apr_conf/suggestion").await;
    assert_eq!(r.body["suggestion"]["status"], "ready");
    assert_eq!(r.body["suggestion"]["body"], "# PRD\nShort text");

    let r = w
        .post(&b, "/api/drafts/apr_conf/suggestion/accept", json!(null))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["body"], "# PRD\nShort text");
    assert_eq!(r.body["edited"], true);
    assert_eq!(
        w.get(&b, "/api/drafts/apr_conf/suggestion").await.body["suggestion"],
        Value::Null
    );
    // The revision task isn't one of the PM's asks.
    let tasks = w.get(&b, "/api/tasks").await.body;
    assert!(tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|t| t["id"] != json!(task_id)));
    // Approving sends the accepted text.
    w.post(&b, "/api/drafts/apr_conf/approve", json!({})).await;
    let decision = &w.tools.writes()[0].body;
    assert_eq!(decision["decision"], "edit");
    assert_eq!(decision["args"]["body"], "# PRD\nShort text");
}

#[tokio::test]
async fn a_change_that_drafts_a_write_fails_and_can_be_dismissed() {
    let w = web().await;
    let b = w.login("ana").await;
    w.post(
        &b,
        "/api/drafts/apr_jira/changes",
        json!({"instruction": "add AC"}),
    )
    .await;
    w.runtime.set("tsk_web0", "status", json!("needs_approval"));
    let r = w.get(&b, "/api/drafts/apr_jira/suggestion").await;
    assert_eq!(r.body["suggestion"]["status"], "failed");
    assert!(r.body["suggestion"]["error"]
        .as_str()
        .unwrap()
        .contains("instead of answering"));
    let r = w
        .post(&b, "/api/drafts/apr_jira/suggestion/reject", json!(null))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["body"], "As a PM…", "unchanged");
    assert_eq!(
        w.get(&b, "/api/drafts/apr_jira/suggestion").await.body["suggestion"],
        Value::Null
    );
    // A failed one doesn't block a new request.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/changes",
            json!({"instruction": "add AC"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/changes",
            json!({"instruction": " "}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn practice_drafts_create_nothing_upstream() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.post(&b, "/api/drafts/practice", json!(null)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["practice"], true);
    assert_eq!(r.body["status"], "pending");
    assert_eq!(r.body["kind"], "confluence_page");
    let id = r.body["id"].as_str().unwrap().to_string();
    // Idempotent while one exists.
    let again = w.post(&b, "/api/drafts/practice", json!(null)).await;
    assert_eq!(again.body["id"], json!(id));
    // In the list, read, edited, changed, approved.
    let list = w.get(&b, "/api/drafts").await.body;
    assert!(list["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["id"] == json!(id) && d["practice"] == true));
    assert_eq!(
        w.get(&b, &format!("/api/drafts/{id}")).await.status,
        StatusCode::OK
    );
    let r = w
        .send(
            &b,
            Method::PUT,
            &format!("/api/drafts/{id}"),
            Some(json!({"title": "My practice", "body": "Edited"})),
        )
        .await;
    assert_eq!(r.body["title"], "My practice");
    let r = w
        .post(
            &b,
            &format!("/api/drafts/{id}/changes"),
            json!({"instruction": "shorter"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.body["suggestion"]["status"], "ready");
    let r = w
        .post(
            &b,
            &format!("/api/drafts/{id}/suggestion/accept"),
            json!(null),
        )
        .await;
    assert!(r.body["body"].as_str().unwrap().starts_with("Edited"));
    let r = w
        .post(&b, &format!("/api/drafts/{id}/approve"), json!({}))
        .await;
    assert_eq!(r.body["status"], "approved");
    assert_eq!(r.body["result_url"], Value::Null);
    assert_eq!(r.body["target"]["label"], "Practice only: nothing is sent");
    // Approving again is a retry; rejecting once approved is not.
    let again = w
        .post(&b, &format!("/api/drafts/{id}/approve"), json!({}))
        .await;
    assert_eq!(again.body["status"], "approved");
    assert_eq!(
        w.post(&b, &format!("/api/drafts/{id}/reject"), json!({}))
            .await
            .code(),
        "not_pending"
    );
    // Nothing was written anywhere, and the tool server never heard of it.
    assert!(w.tools.writes().is_empty() && w.runtime.writes().is_empty());
    assert!(w.tools.calls().iter().all(|c| !c.uri.contains(&id)));

    // Another PM can't see it.
    let ben = w.login("ben").await;
    assert_eq!(
        w.get(&ben, &format!("/api/drafts/{id}")).await.code(),
        "not_found"
    );
    // Only practice drafts can be deleted.
    let r = w
        .send(&b, Method::DELETE, "/api/drafts/apr_conf", None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "not_practice");
    let r = w
        .send(&b, Method::DELETE, &format!("/api/drafts/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        w.get(&b, &format!("/api/drafts/{id}")).await.code(),
        "not_found"
    );
    let fresh = w.post(&b, "/api/drafts/practice", json!(null)).await;
    assert_ne!(fresh.body["id"], json!(id));
    assert_eq!(fresh.body["status"], "pending");
}

// Tasks.

#[tokio::test]
async fn tasks_go_to_the_personal_bot_as_the_pm() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w
        .post(&b, "/api/tasks", json!({"text": "What did PAY ship?"}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["bot"], "pm-ana");
    assert_eq!(r.body["status"], "queued");
    assert_eq!(r.body["draft_ids"], json!([]));
    assert_eq!(r.body["events"], json!([]));
    let call = &w.runtime.writes()[0];
    assert_eq!(call.uri, "/v1/tasks");
    assert_eq!(call.user.as_deref(), Some("ana"));
    assert_eq!(call.auth, format!("Bearer {BOTS_TOKEN}"));
    assert_eq!(
        call.body,
        json!({"bot": "pm-ana", "requested_by": "ana", "text": "What did PAY ship?",
               "source": "web"})
    );
    assert_eq!(
        w.post(&b, "/api/tasks", json!({"text": "  "})).await.status,
        StatusCode::BAD_REQUEST
    );

    let t = w.get(&b, "/api/tasks/tsk_ana1").await.body;
    assert_eq!(t["draft_ids"], json!(["apr_conf"]));
    assert_eq!(
        t["events"],
        json!([{"at": "2026-11-14T09:00:01Z", "kind": "milestone", "text": "Reading Jira"}])
    );
    let list = w.get(&b, "/api/tasks").await.body;
    let ids: Vec<&str> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["tsk_web0", "tsk_ana1"]);
    assert!(list["tasks"][0].get("events").is_none());
    // Every runtime call names the PM.
    assert!(w
        .runtime
        .calls()
        .iter()
        .all(|c| c.user.as_deref() == Some("ana")));
    assert!(w
        .runtime
        .calls()
        .iter()
        .any(|c| c.uri == "/v1/tasks?requested_by=ana&limit=20"));

    // Someone else's task is 404.
    let r = w.get(&b, "/api/tasks/tsk_ben1").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.code(), "not_found");
    assert_eq!(w.get(&b, "/api/tasks/tsk_nope").await.code(), "not_found");
}

#[tokio::test]
async fn a_task_shows_its_milestones_progress_and_retry_but_no_worker() {
    let w = web().await;
    let b = w.login("ana").await;
    let t = w.get(&b, "/api/tasks/tsk_ana1").await.body;
    assert_eq!(t["progress"], Value::Null, "no checklist");
    assert_eq!(t["retrying"], false);

    // What the real run sent: bookkeeping, checklists and milestones, some twice.
    w.runtime.set("tsk_ana1", "status", json!("working"));
    w.runtime.set(
        "tsk_ana1",
        "_events",
        json!([
            {"at": "2026-11-16T10:00:00Z", "kind": "status", "text": "queued"},
            {"at": "2026-11-16T10:00:01Z", "kind": "status", "text": "claimed by worker vm-16344"},
            {"at": "2026-11-16T10:00:02Z", "kind": "status", "text": "started on vm-16344"},
            {"at": "2026-11-16T10:00:03Z", "kind": "milestone", "text": "Working on it"},
            {"at": "2026-11-16T10:00:04Z", "kind": "checklist", "text": "0 of 1 done"},
            {"at": "2026-11-16T10:00:05Z", "kind": "milestone", "text": "Reading Jira"},
            {"at": "2026-11-16T10:00:06Z", "kind": "milestone", "text": "Reading Jira"},
            {"at": "2026-11-16T10:00:07Z", "kind": "checklist", "text": "2 of 3 done"},
            {"at": "2026-11-16T10:00:08Z", "kind": "milestone", "text": "Drafting in Confluence"},
        ]),
    );
    let r = w.get(&b, "/api/tasks/tsk_ana1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body["events"],
        json!([
            {"at": "2026-11-16T10:00:03Z", "kind": "milestone", "text": "Working on it"},
            {"at": "2026-11-16T10:00:05Z", "kind": "milestone", "text": "Reading Jira"},
            {"at": "2026-11-16T10:00:08Z", "kind": "milestone", "text": "Drafting in Confluence"},
        ])
    );
    assert_eq!(r.body["progress"], json!({"done": 2, "total": 3}));
    assert_eq!(r.body["retrying"], false);
    let text = r.body.to_string();
    assert!(
        !text.contains("vm-16344") && !text.contains("claimed"),
        "{text}"
    );

    // The task's own checklist wins over the event text; a retried task says so while it
    // runs, in the list too.
    w.runtime.set(
        "tsk_ana1",
        "checklist",
        json!([{"item": "Search", "done": true}, {"item": "Draft", "done": false}]),
    );
    w.runtime.set("tsk_ana1", "attempts", json!(2));
    let r = w.get(&b, "/api/tasks/tsk_ana1").await;
    assert_eq!(r.body["progress"], json!({"done": 1, "total": 2}));
    assert_eq!(r.body["retrying"], true);
    let list = w.get(&b, "/api/tasks").await.body;
    let listed = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "tsk_ana1")
        .unwrap();
    assert_eq!(listed["progress"], json!({"done": 1, "total": 2}));
    assert_eq!(listed["retrying"], true);
    assert!(listed.get("events").is_none());

    // Requeued after a restart, before a worker claims it again: the runtime's event says so.
    w.runtime.set("tsk_ana1", "status", json!("queued"));
    w.runtime.set("tsk_ana1", "attempts", json!(1));
    w.runtime.set(
        "tsk_ana1",
        "_events",
        json!([{"at": "2026-11-16T10:00:09Z", "kind": "status",
                "text": "interrupted by a restart; retrying"}]),
    );
    let r = w.get(&b, "/api/tasks/tsk_ana1").await;
    assert_eq!(r.body["retrying"], true);
    assert_eq!(r.body["events"], json!([]));
    // Settled: no longer retrying.
    w.runtime.set("tsk_ana1", "status", json!("done"));
    assert_eq!(
        w.get(&b, "/api/tasks/tsk_ana1").await.body["retrying"],
        false
    );
}

#[tokio::test]
async fn a_one_line_body_with_escaped_line_breaks_reads_as_lines_but_sends_as_drafted() {
    let w = web().await;
    let b = w.login("ana").await;
    let raw = r"We ship the grace period in 4.2 after the launch.\n\nSource: PM-101";
    let args = json!({"space": "PM", "title": "PRD", "body": raw, "parent_id": "77"});
    w.tools.set("apr_conf", "args", args.clone());
    let d = w.get(&b, "/api/drafts/apr_conf").await.body;
    assert_eq!(
        d["body"],
        "We ship the grace period in 4.2 after the launch.\n\nSource: PM-101"
    );
    assert_eq!(d["edited"], false);
    // An approve of what the PM saw, without an edit, sends the bot's own text.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_conf/approve",
            json!({"version": d["version"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        w.tools.writes()[0].body,
        json!({"decided_by": "ana", "decision": "approve"})
    );

    // With an edit saved, the edit (the text the PM saw) is what is sent.
    w.tools.set(
        "apr_jira",
        "args",
        json!({"project": "PAY", "issue_type": "Story", "summary": "Grace", "description": raw}),
    );
    let d = w.get(&b, "/api/drafts/apr_jira").await.body;
    let shown = d["body"].as_str().unwrap().to_string();
    assert!(shown.contains("\n\nSource") && !shown.contains(r"\n"));
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_jira",
            Some(json!({ "body": shown })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/approve",
            json!({"version": r.body["version"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let sent = &w.tools.writes()[1].body;
    assert_eq!(sent["decision"], "edit");
    assert_eq!(sent["args"]["description"], json!(shown));
}

#[tokio::test]
async fn an_auditor_only_member_may_not_use_drafts_or_tasks() {
    let w = web().await;
    let b = w.login("audrey").await;
    assert_eq!(w.get(&b, "/api/me").await.body["roles"], json!(["auditor"]));
    let gets = [
        "/api/drafts",
        "/api/drafts/apr_conf",
        "/api/drafts/apr_conf/suggestion",
        "/api/tasks",
        "/api/tasks/tsk_ana1",
    ];
    for path in gets {
        let r = w.get(&b, path).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.code(), "not_allowed");
    }
    let posts = [
        "/api/drafts/practice",
        "/api/drafts/apr_conf/changes",
        "/api/drafts/apr_conf/suggestion/accept",
        "/api/drafts/apr_conf/suggestion/reject",
        "/api/drafts/apr_conf/approve",
        "/api/drafts/apr_conf/reject",
        "/api/tasks",
    ];
    for path in posts {
        let r = w
            .post(&b, path, json!({"instruction": "x", "text": "x"}))
            .await;
        assert_eq!(r.code(), "not_allowed", "{path}");
    }
    for method in [Method::PUT, Method::DELETE] {
        let r = w
            .send(
                &b,
                method,
                "/api/drafts/apr_conf",
                Some(json!({"body": "x"})),
            )
            .await;
        assert_eq!(r.code(), "not_allowed");
    }
    assert!(w.runtime.calls().is_empty() && w.tools.calls().is_empty());
}

#[tokio::test]
async fn brief_and_follow_ups_are_not_built() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(r.code(), "not_built");
    for path in ["/api/followups/fu_1/dismiss", "/api/followups/fu_1/draft"] {
        let r = w.post(&b, path, json!(null)).await;
        assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(r.code(), "not_built");
    }
}

#[tokio::test]
async fn no_token_ever_reaches_the_browser() {
    let w = web().await;
    let b = w.login("ana").await;
    for path in [
        "/api/me",
        "/api/drafts",
        "/api/drafts/apr_conf",
        "/api/tasks",
        "/api/tasks/tsk_ana1",
        "/api/drafts/apr_ben",
    ] {
        w.get(&b, path).await;
    }
    w.post(&b, "/api/tasks", json!({"text": "hi"})).await;
    w.post(&b, "/api/drafts/apr_ext/approve", json!({})).await;
    w.post(&b, "/api/drafts/apr_jira/approve", json!({})).await;
    w.post(
        &b,
        "/api/drafts/apr_conf/changes",
        json!({"instruction": "x"}),
    )
    .await;
    w.assert_no_tokens();
    // Upstream errors say which service, never what it was sent.
    assert!(w
        .runtime
        .calls()
        .iter()
        .all(|c| c.auth == format!("Bearer {BOTS_TOKEN}")));
    assert!(w
        .tools
        .calls()
        .iter()
        .all(|c| c.auth == format!("Bearer {TOOLS_TOKEN}")));
}

#[tokio::test]
async fn an_upstream_that_is_down_is_a_plain_502() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    // Nothing listens on these ports.
    let cfg = config("http://127.0.0.1:9", "http://127.0.0.1:9", dir.path());
    let (base, _stop) = serve(pmos_web::build(cfg, Deps::in_memory()).unwrap()).await;
    let w = Web {
        base,
        runtime: Arc::default(),
        tools: Arc::default(),
        http: reqwest::Client::new(),
        clock: Arc::new(Mutex::new(start_time())),
        dir,
        seen: Mutex::new(Vec::new()),
        _stop: vec![],
    };
    let b = w.login("ana").await;
    for path in ["/api/drafts", "/api/tasks"] {
        let r = w.get(&b, path).await;
        assert_eq!(r.status, StatusCode::BAD_GATEWAY);
        assert_eq!(r.code(), "upstream_unavailable");
    }
    let r = w.post(&b, "/api/tasks", json!({"text": "hi"})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(r.body["error"].as_str().unwrap().contains("bot runtime"));
    w.assert_no_tokens();
}

// What the draft was built from, and what the tool server knows about its target.

#[tokio::test]
async fn a_draft_shows_the_sources_the_tool_server_found_for_its_own_pm() {
    let w = web().await;
    let b = w.login("ana").await;
    let d = w.get(&b, "/api/drafts/apr_conf").await.body;
    assert_eq!(
        d["sources"],
        json!([
            {"id": "s1", "system": "jira", "title": "PAY-212",
             "url": "https://acme.atlassian.net/browse/PAY-212", "excerpt": null},
            {"id": "s2", "system": "docs", "title": "grace period",
             "url": "https://docs.example.com/grace.md", "excerpt": null},
            // A link that isn't https is dropped; the source stays.
            {"id": "s3", "system": "slack", "title": "Slack thread in C01", "url": null,
             "excerpt": null},
            {"id": "s4", "system": "context", "title": "Pricing principles", "url": null,
             "excerpt": null},
        ])
    );
    // Asked for this PM's own item, with the service token, and only after the ownership
    // check: someone else's draft never reaches the sources route.
    let calls = w.tools.calls();
    assert!(calls
        .iter()
        .any(|c| c.uri == "/internal/approvals/apr_conf/sources?user=ana"
            && c.auth == format!("Bearer {TOOLS_TOKEN}")));
    w.get(&b, "/api/drafts/apr_ben").await;
    assert!(w
        .tools
        .calls()
        .iter()
        .all(|c| !c.uri.starts_with("/internal/approvals/apr_ben/sources")));
    // Edits and approvals answer with the sources too.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_conf",
            Some(json!({"body": "x"})),
        )
        .await;
    assert_eq!(r.body["sources"].as_array().unwrap().len(), 4);
    w.assert_no_tokens();

    // A tool server that can't say: the draft still loads, with no sources.
    w.tools.set("apr_conf", "sources_down", json!(true));
    let r = w.get(&b, "/api/drafts/apr_conf").await;
    assert_eq!((r.status, &r.body["sources"]), (StatusCode::OK, &json!([])));
    assert_eq!(r.body["body"], "x");
}

#[tokio::test]
async fn expiry_links_and_the_audience_come_from_the_tool_server() {
    let w = web().await;
    let b = w.login("ana").await;
    // The tool server's own TTL (here 12 hours), link and audience result.
    w.tools
        .set("apr_slack", "expires_at", json!("2026-11-16T21:04:00Z"));
    w.tools.set(
        "apr_slack",
        "target_url",
        json!("https://slack.com/archives/C01/p17310002"),
    );
    w.tools.set("apr_ext", "external", json!(true));
    let d = w.get(&b, "/api/drafts/apr_slack").await.body;
    assert_eq!(d["expires_at"], "2026-11-16T21:04:00Z");
    assert_eq!(
        d["target"]["url"],
        "https://slack.com/archives/C01/p17310002"
    );
    assert_eq!(d["external"], false);
    let list = w.get(&b, "/api/drafts").await.body;
    assert_eq!(list["drafts"][2]["expires_at"], "2026-11-16T21:04:00Z");
    // Known before any approve: the app asks first.
    assert_eq!(
        w.get(&b, "/api/drafts/apr_ext").await.body["external"],
        true
    );
    assert!(w.tools.writes().is_empty());
    // A link that isn't https is never shown; a label is never the bot's text.
    w.tools
        .set("apr_jira", "target_url", json!("javascript:alert(1)"));
    w.tools.set(
        "apr_jira",
        "args",
        json!({"project": "PAY (approved by legal)", "issue_type": "Story", "summary": "x"}),
    );
    let d = w.get(&b, "/api/drafts/apr_jira").await.body;
    assert_eq!(d["target"]["url"], Value::Null);
    assert_eq!(
        d["target"]["label"],
        "Project an unrecognised project, a new Story"
    );
}

#[tokio::test]
async fn an_approve_of_a_changed_draft_is_stale() {
    let w = web().await;
    let b = w.login("ana").await;
    let seen = w.get(&b, "/api/drafts/apr_conf").await.body["version"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(seen.len(), 64);
    // An edit in another tab changes the version.
    let other_tab = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_conf",
            Some(json!({"body": "# PRD\nChanged elsewhere"})),
        )
        .await
        .body;
    let now = other_tab["version"].as_str().unwrap().to_string();
    assert_ne!(seen, now);
    let r = w
        .post(&b, "/api/drafts/apr_conf/approve", json!({"version": seen}))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "stale");
    assert!(w.tools.writes().is_empty(), "nothing was sent");
    // So does a change at the tool server (the item's arguments).
    let jira = w.get(&b, "/api/drafts/apr_jira").await.body["version"].clone();
    w.tools.set(
        "apr_jira",
        "args",
        json!({"project": "PAY", "issue_type": "Story", "summary": "Grace period",
               "description": "Changed"}),
    );
    let r = w
        .post(&b, "/api/drafts/apr_jira/approve", json!({"version": jira}))
        .await;
    assert_eq!(r.code(), "stale");
    assert!(w.tools.writes().is_empty());
    // With the version shown now, it goes, with the text that version is of.
    let r = w
        .post(&b, "/api/drafts/apr_conf/approve", json!({"version": now}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        w.tools.writes()[0].body["args"]["body"],
        "# PRD\nChanged elsewhere"
    );
    // The approved draft keeps that version: what was published is what was seen.
    assert_eq!(r.body["version"], json!(now));
    // A practice draft checks it too.
    let p = w.post(&b, "/api/drafts/practice", json!(null)).await.body;
    let pid = p["id"].as_str().unwrap();
    let r = w
        .post(
            &b,
            &format!("/api/drafts/{pid}/approve"),
            json!({"version": "0".repeat(64)}),
        )
        .await;
    assert_eq!(r.code(), "stale");
    let r = w
        .post(
            &b,
            &format!("/api/drafts/{pid}/approve"),
            json!({"version": p["version"]}),
        )
        .await;
    assert_eq!(r.body["status"], "approved");
}

#[tokio::test]
async fn approving_again_is_safe() {
    let w = web().await;
    let b = w.login("ana").await;
    let v = w.get(&b, "/api/drafts/apr_jira").await.body["version"].clone();
    let first = w
        .post(&b, "/api/drafts/apr_jira/approve", json!({"version": v}))
        .await;
    assert_eq!(first.body["status"], "approved");
    // A retry after a lost answer: the approved draft, and no second write.
    for _ in 0..2 {
        let r = w
            .post(&b, "/api/drafts/apr_jira/approve", json!({"version": v}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        assert_eq!(r.body["status"], "approved");
        assert_eq!(r.body["result_url"], first.body["result_url"]);
    }
    assert_eq!(w.tools.writes().len(), 1);
    // A retry naming other text than was published is not the same approval.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/approve",
            json!({"version": "f".repeat(64)}),
        )
        .await;
    assert_eq!(r.code(), "not_pending");

    // Two approves at once: the other one won with the same text, so this one answers the
    // approved draft too.
    w.tools.data.lock().unwrap().push(item(
        "apr_dbl",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "issue_type": "Task", "summary": "Twice"}),
        "2026-11-16T09:07:00Z",
    ));
    w.send(
        &b,
        Method::PUT,
        "/api/drafts/apr_dbl",
        Some(json!({"title": "Twice, edited", "body": "Body"})),
    )
    .await;
    let r = w.post(&b, "/api/drafts/apr_dbl/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    assert_eq!(r.body["title"], "Twice, edited");
    // Decided by someone else's hand (in Slack, by another approver): not ours to answer.
    let r = w.post(&b, "/api/drafts/apr_race/approve", json!({})).await;
    assert_eq!(r.code(), "not_pending");
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("dist")).unwrap();
    std::fs::write(dir.path().join("dist/index.html"), "<!doctype html>app").unwrap();
    let w = web_with(|c| c.static_dir = Some(dir.path().join("dist"))).await;
    let b = w.login("ana").await;
    let api = [
        w.get(&b, "/api/me").await,
        w.get(&b, "/api/drafts/apr_conf").await,
        w.get(&b, "/api/drafts/apr_ben").await,
        w.get(&Browser::default(), "/api/me").await,
        w.get(&b, "/api/nope").await,
        w.post(&b, "/api/tasks", json!({"text": "hi"})).await,
    ];
    for r in &api {
        assert_eq!(r.headers["x-content-type-options"], "nosniff");
        assert_eq!(r.headers["referrer-policy"], "no-referrer");
        assert_eq!(r.headers["x-frame-options"], "DENY");
        assert_eq!(r.headers["cache-control"], "no-store", "{:?}", r.body);
        assert_eq!(
            r.headers["content-security-policy"],
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'"
        );
    }
    let html = w.get(&Browser::default(), "/drafts/apr_conf").await;
    assert_eq!(html.status, StatusCode::OK);
    assert_eq!(html.headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(html.headers["x-content-type-options"], "nosniff");
    assert_eq!(html.headers["referrer-policy"], "no-referrer");
    assert_eq!(html.headers["cache-control"], "no-cache");
    let csp = html.headers["content-security-policy"].to_str().unwrap();
    let directives: Vec<&str> = csp.split(';').map(str::trim).collect();
    for d in [
        "default-src 'self'",
        "img-src 'self'",
        "style-src 'self'",
        "script-src 'self'",
        "connect-src 'self'",
        "form-action 'self'",
        "frame-ancestors 'none'",
        "base-uri 'none'",
    ] {
        assert!(directives.contains(&d), "{d} missing from {csp}");
    }
    assert!(!csp.contains("unsafe") && !csp.contains("data:") && !csp.contains("https:"));
}

#[tokio::test]
async fn writes_must_be_json_including_sign_in() {
    let w = web().await;
    let b = w.login("ana").await;
    let raw = |path: &str, content_type: Option<&str>, body: &str, b: &Browser| {
        let mut req = w
            .http
            .post(format!("{}{path}", w.base))
            .body(body.to_string());
        if let Some(c) = &b.cookie {
            req = req.header("cookie", format!("__Host-pmos={c}"));
        }
        if let Some(t) = &b.csrf {
            req = req.header("x-pmos-csrf", t);
        }
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        req
    };
    // A cross-site form can send these without a preflight; none gets in.
    for ct in [
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
        Some("multipart/form-data; boundary=x"),
        None,
    ] {
        let res = raw("/api/tasks", ct, r#"{"text": "hi"}"#, &b)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct:?}");
        assert_eq!(
            res.headers()["cache-control"],
            "no-store",
            "an error is an /api answer too"
        );
        assert_eq!(
            res.json::<Value>().await.unwrap()["code"],
            "bad_content_type"
        );
        let res = raw(
            "/api/auth/dev-login",
            ct,
            r#"{"handle": "ana"}"#,
            &Browser::default(),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct:?}");
        assert!(res.headers().get("set-cookie").is_none());
    }
    // Sign-in with no body at all still needs JSON.
    let res = raw("/api/auth/dev-login", None, "", &Browser::default())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(w.runtime.writes().is_empty() && w.tools.writes().is_empty());
    // JSON with a charset is JSON; a write with no body needs no type.
    let res = raw(
        "/api/tasks",
        Some("application/json; charset=utf-8"),
        r#"{"text": "hi"}"#,
        &b,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let r = w.send(&b, Method::DELETE, "/api/me/hints", None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn practice_drafts_never_reach_the_tool_server() {
    let w = web().await;
    let b = w.login("ana").await;
    let p = w.post(&b, "/api/drafts/practice", json!(null)).await.body;
    let id = p["id"].as_str().unwrap().to_string();
    assert_eq!(p["target"]["label"], "Practice only: nothing is sent");
    assert_eq!(p["sources"], json!([]));
    assert_eq!(p["external"], false);
    w.send(
        &b,
        Method::PUT,
        &format!("/api/drafts/{id}"),
        Some(json!({"body": "Edited practice"})),
    )
    .await;
    let r = w
        .post(
            &b,
            &format!("/api/drafts/{id}/approve"),
            json!({"confirm_external": true}),
        )
        .await;
    assert_eq!(r.body["status"], "approved");
    assert_eq!(r.body["result_url"], Value::Null);
    w.get(&b, &format!("/api/drafts/{id}")).await;
    w.get(&b, &format!("/api/drafts/{id}/suggestion")).await;
    // Not a single call upstream, read or write, for any of it.
    assert!(w.tools.calls().is_empty(), "{:?}", w.tools.calls());
    assert!(w.runtime.calls().is_empty());
    // An item the tool server sends with a practice-looking id is never decided either.
    w.tools.data.lock().unwrap().push(item(
        "prc_fromupstream",
        "ana",
        "slack.send_message",
        json!({"channel": "C01", "text": "x"}),
        "2026-11-16T09:08:00Z",
    ));
    let r = w
        .post(&b, "/api/drafts/prc_fromupstream/approve", json!({}))
        .await;
    assert_eq!(r.code(), "not_found");
    assert!(w.tools.writes().is_empty());
}

// The daily brief and follow-ups.

const BRIEF_TOKEN: &str = "brief-producer-token-0123456789abcdef";

async fn web_brief() -> Web {
    web_full(
        |_| {},
        BriefConfig {
            enabled: true,
            token: Some(BRIEF_TOKEN.into()),
        },
    )
    .await
}

impl Web {
    /// The producer's call, with `token` as its bearer token.
    async fn fill_with(&self, handle: &str, token: Option<&str>, body: Value) -> Reply {
        let mut req = self
            .http
            .put(format!("{}/internal/brief/{handle}", self.base))
            .json(&body);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        self.reply(req).await
    }

    async fn fill(&self, handle: &str, body: Value) -> Reply {
        self.fill_with(handle, Some(BRIEF_TOKEN), body).await
    }

    async fn reply(&self, req: reqwest::RequestBuilder) -> Reply {
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        Reply {
            status,
            headers,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }
}

fn follow(system: &str, who: &str, summary: &str, n: u32) -> Value {
    json!({
        "system": system, "who": who, "summary": summary,
        "url": format!("https://example.com/{system}/{n}"),
    })
}

fn ids(r: &Reply, list: &str) -> Vec<String> {
    r.body[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_empty_brief_is_200_with_empty_lists() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["date"], json!("2026-11-16"));
    for list in ["changed", "waiting_on_you", "you_wait_on"] {
        assert_eq!(r.body[list], json!([]), "{list}");
    }
}

#[tokio::test]
async fn the_producer_fills_a_brief_and_the_pm_reads_it() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    let r = w
        .fill(
            "ana",
            json!({
                "date": "2026-11-16",
                "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked",
                             "summary": "by Ben", "url": "https://jira.example.com/PAY-212"}],
                "waiting_on_you": [follow("slack", "Ben", "asked about the pricing page", 1)],
                "you_wait_on": [follow("jira", "Legal", "the DPA question", 2)],
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body, json!({"changed": 1, "followups": 2}));
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.body["date"], json!("2026-11-16"));
    assert_eq!(
        r.body["changed"][0]["title"],
        json!("PAY-212 moved to Blocked")
    );
    assert_eq!(r.body["changed"][0]["summary"], json!("by Ben"));
    assert_eq!(r.body["waiting_on_you"][0]["age_days"], json!(0));
    assert_eq!(
        r.body["waiting_on_you"][0]["url"],
        json!("https://example.com/slack/1")
    );
    assert!(r.body["you_wait_on"][0]["id"]
        .as_str()
        .unwrap()
        .starts_with("fu_"));
    // Two days on: the Slack one expired after a day, and so did the day's changed items.
    w.advance(Duration::hours(30));
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.body["waiting_on_you"], json!([]));
    assert_eq!(r.body["changed"], json!([]));
    assert_eq!(r.body["you_wait_on"][0]["who"], json!("Legal"));
    assert_eq!(r.body["you_wait_on"][0]["age_days"], json!(1));
    // No token of any kind reaches the browser.
    w.assert_no_tokens();
    assert!(!r.body.to_string().contains(BRIEF_TOKEN));
}

#[tokio::test]
async fn another_users_follow_up_is_404() {
    let w = web_brief().await;
    w.fill(
        "ana",
        json!({"waiting_on_you": [follow("jira", "Ben", "needs a decision", 1)]}),
    )
    .await;
    let ana = w.login("ana").await;
    let id = ids(&w.get(&ana, "/api/brief").await, "waiting_on_you")[0].clone();
    let ben = w.login("ben").await;
    // Ben's own brief has nothing of Ana's.
    let r = w.get(&ben, "/api/brief").await;
    assert_eq!(r.body["waiting_on_you"], json!([]));
    for action in ["dismiss", "draft"] {
        let r = w
            .post(&ben, &format!("/api/followups/{id}/{action}"), json!(null))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{action}");
        assert_eq!(r.code(), "not_found");
    }
    // Nothing was asked of any bot, and Ana's follow-up is still open.
    assert!(w.runtime.writes().is_empty());
    assert_eq!(
        ids(&w.get(&ana, "/api/brief").await, "waiting_on_you"),
        [id]
    );
    // An id that doesn't exist, or isn't shaped like one, is the same answer.
    for path in [
        "/api/followups/fu_nope/dismiss",
        "/api/followups/a.b/dismiss",
    ] {
        assert_eq!(w.post(&ben, path, json!(null)).await.code(), "not_found");
    }
}

#[tokio::test]
async fn dismiss_is_idempotent_and_dismissed_ones_are_not_listed() {
    let w = web_brief().await;
    w.fill(
        "ana",
        json!({"you_wait_on": [follow("jira", "Legal", "a", 1), follow("jira", "Ops", "b", 2)]}),
    )
    .await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await;
    let legal = r.body["you_wait_on"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["who"] == json!("Legal"))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = legal.clone();
    for _ in 0..2 {
        let r = w
            .post(&b, &format!("/api/followups/{id}/dismiss"), json!(null))
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    let left = ids(&w.get(&b, "/api/brief").await, "you_wait_on");
    assert_eq!(left.len(), 1);
    assert!(!left.contains(&id));
    // A dismissed one can't be drafted for.
    let r = w
        .post(&b, &format!("/api/followups/{id}/draft"), json!(null))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The producer sending it again doesn't bring it back, and the other keeps its place.
    w.fill(
        "ana",
        json!({"you_wait_on": [follow("jira", "Legal", "a, updated", 1), follow("jira", "Ops", "b", 2)]}),
    )
    .await;
    assert_eq!(ids(&w.get(&b, "/api/brief").await, "you_wait_on"), left);
    // A follow-up the producer stopped listing is gone, for good.
    w.fill(
        "ana",
        json!({"you_wait_on": [follow("jira", "Legal", "a", 1)]}),
    )
    .await;
    assert_eq!(w.get(&b, "/api/brief").await.body["you_wait_on"], json!([]));
}

#[tokio::test]
async fn expired_follow_ups_are_not_listed_and_cannot_be_acted_on() {
    let w = web_brief().await;
    w.fill(
        "ana",
        json!({"waiting_on_you": [follow("slack", "Ben", "slack one", 1),
                                  follow("jira", "Ben", "jira one", 2)]}),
    )
    .await;
    let b = w.login("ana").await;
    let both = ids(&w.get(&b, "/api/brief").await, "waiting_on_you");
    assert_eq!(both.len(), 2);
    w.advance(Duration::hours(7));
    let b = w.login("ana").await;
    // Kept signed in by use (8 idle hours), so the follow-ups are 28 hours old at the end.
    for _ in 0..2 {
        w.advance(Duration::hours(7));
        assert_eq!(w.get(&b, "/api/brief").await.status, StatusCode::OK);
    }
    w.advance(Duration::hours(7));
    let r = w.get(&b, "/api/brief").await;
    let left = ids(&r, "waiting_on_you");
    assert_eq!(left.len(), 1);
    assert_eq!(r.body["waiting_on_you"][0]["summary"], json!("jira one"));
    let gone = both.iter().find(|i| !left.contains(i)).unwrap();
    for action in ["dismiss", "draft"] {
        let r = w
            .post(&b, &format!("/api/followups/{gone}/{action}"), json!(null))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{action}");
    }
    // The rest goes after a week.
    w.advance(Duration::days(7));
    let b = w.login("ana").await;
    assert_eq!(
        w.get(&b, "/api/brief").await.body["waiting_on_you"],
        json!([])
    );
}

#[tokio::test]
async fn a_draft_asks_the_pms_personal_bot() {
    let w = web_brief().await;
    w.fill(
        "ana",
        json!({"waiting_on_you": [follow("slack", "Ben", "asked about the pricing page", 1)],
               "you_wait_on": [follow("jira", "Legal", "the DPA question", 2)]}),
    )
    .await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await;
    let reply = ids(&r, "waiting_on_you")[0].clone();
    let nudge = ids(&r, "you_wait_on")[0].clone();
    let r = w
        .post(&b, &format!("/api/followups/{reply}/draft"), json!(null))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["bot"], json!("pm-ana"));
    let r2 = w
        .post(&b, &format!("/api/followups/{nudge}/draft"), json!(null))
        .await;
    assert_eq!(r2.status, StatusCode::CREATED);
    let writes = w.runtime.writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].user.as_deref(), Some("ana"));
    assert_eq!(writes[0].body["source"], json!("web"));
    assert_eq!(writes[0].body["requested_by"], json!("ana"));
    let text = writes[0].body["text"].as_str().unwrap();
    assert!(text.contains("reply to Ben") && text.contains("asked about the pricing page"));
    assert!(text.contains("https://example.com/slack/1"));
    let text = writes[1].body["text"].as_str().unwrap();
    assert!(text.contains("nudge to Legal") && text.contains("the DPA question"));
    // The tool server hears nothing: the draft arrives later, as an ordinary approval.
    assert!(w.tools.calls().is_empty());
    // Asking is not dismissing.
    assert_eq!(
        ids(&w.get(&b, "/api/brief").await, "waiting_on_you"),
        [reply]
    );
}

#[tokio::test]
async fn follow_up_routes_need_sign_in_the_write_checks_and_the_right_role() {
    let w = web_brief().await;
    w.fill(
        "ana",
        json!({"waiting_on_you": [follow("jira", "Ben", "x", 1)]}),
    )
    .await;
    let ana = w.login("ana").await;
    let id = ids(&w.get(&ana, "/api/brief").await, "waiting_on_you")[0].clone();
    let nobody = Browser::default();
    for action in ["dismiss", "draft"] {
        let r = w
            .post(
                &nobody,
                &format!("/api/followups/{id}/{action}"),
                json!(null),
            )
            .await;
        assert_eq!(r.code(), "signed_out");
        let no_csrf = Browser {
            csrf: None,
            ..ana.clone()
        };
        let r = w
            .post(
                &no_csrf,
                &format!("/api/followups/{id}/{action}"),
                json!(null),
            )
            .await;
        assert_eq!(r.code(), "bad_csrf");
    }
    let audrey = w.login("audrey").await;
    let r = w
        .post(&audrey, &format!("/api/followups/{id}/draft"), json!(null))
        .await;
    assert_eq!(r.code(), "not_allowed");
    assert!(w.runtime.writes().is_empty());
    assert_eq!(
        ids(&w.get(&ana, "/api/brief").await, "waiting_on_you").len(),
        1
    );
}

#[tokio::test]
async fn the_internal_route_wants_the_service_token() {
    let w = web_brief().await;
    let body = json!({"waiting_on_you": [follow("jira", "Ben", "x", 1)]});
    for token in [
        None,
        Some(""),
        Some("wrong"),
        // Other services' tokens are not it, nor is a token one character short.
        Some(BOTS_TOKEN),
        Some(TOOLS_TOKEN),
        Some(&BRIEF_TOKEN[..BRIEF_TOKEN.len() - 1]),
    ] {
        let r = w.fill_with("ana", token, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{token:?}");
        assert_eq!(r.code(), "bad_token");
    }
    // A signed-in PM's cookie doesn't open it either.
    let b = w.login("ana").await;
    let r = w
        .reply(
            w.http
                .put(format!("{}/internal/brief/ana", w.base))
                .header(
                    "cookie",
                    format!("__Host-pmos={}", b.cookie.clone().unwrap()),
                )
                .header("x-pmos-csrf", b.csrf.clone().unwrap())
                .json(&body),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        w.get(&b, "/api/brief").await.body["waiting_on_you"],
        json!([])
    );
    // The right one works, only for a member, and only as JSON.
    assert_eq!(w.fill("ana", body.clone()).await.status, StatusCode::OK);
    assert_eq!(
        w.fill("nobody", body.clone()).await.status,
        StatusCode::NOT_FOUND
    );
    let r = w
        .reply(
            w.http
                .put(format!("{}/internal/brief/ana", w.base))
                .header("authorization", format!("Bearer {BRIEF_TOKEN}"))
                .header("content-type", "text/plain")
                .body("{}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn the_internal_route_refuses_oversize_and_bad_input_and_stores_nothing() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    let ok = follow("jira", "Ben", "fine", 1);
    // A body over the cap.
    let r = w
        .fill(
            "ana",
            json!({"waiting_on_you": [follow("jira", "Ben", &"x".repeat(200_000), 1)]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let many: Vec<Value> = (0..51).map(|n| follow("jira", "Ben", "x", n)).collect();
    let mut bad: Vec<(&str, Value)> = vec![
        ("too many", json!({"waiting_on_you": many})),
        (
            "long summary",
            json!({"waiting_on_you": [follow("jira", "Ben", &"x".repeat(201), 1)]}),
        ),
        (
            "empty summary",
            json!({"waiting_on_you": [follow("jira", "Ben", "  ", 1)]}),
        ),
        (
            "two lines",
            json!({"waiting_on_you": [follow("jira", "Ben", "one\ntwo", 1)]}),
        ),
        (
            "long who",
            json!({"waiting_on_you": [follow("jira", &"w".repeat(81), "x", 1)]}),
        ),
        (
            "bad system",
            json!({"waiting_on_you": [follow("Jira!", "Ben", "x", 1)]}),
        ),
        (
            "unknown field",
            json!({"waiting_on_you": [ok.clone()], "extra": 1}),
        ),
        ("bad date", json!({"date": "16/11/2026"})),
        (
            "future",
            json!({"you_wait_on": [{"system": "jira", "who": "B", "summary": "x",
            "url": "https://e.com/1", "created_at": "2026-11-17T09:00:00Z"}]}),
        ),
        (
            "changed url",
            json!({"changed": [{"system": "jira", "title": "t", "url": "javascript:alert(1)"}]}),
        ),
    ];
    for url in [
        "javascript:alert(1)",
        "ftp://example.com/a",
        "file:///etc/passwd",
        "data:text/html,hi",
        "//example.com/a",
        "/relative",
        "example.com/a",
        "https://user:pw@example.com/a",
        "https://",
        "https://exa mple.com",
        "",
    ] {
        bad.push((
            "bad url",
            json!({"waiting_on_you": [ok.clone(), {"system": "jira", "who": "B", "summary": "x", "url": url}]}),
        ));
    }
    for (what, body) in bad {
        let r = w.fill("ana", body).await;
        assert!(
            r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::BAD_REQUEST,
            "{what}: {} {:?}",
            r.status,
            r.body
        );
    }
    // Every refusal stored nothing, even next to a good item.
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.body["waiting_on_you"], json!([]));
    assert_eq!(r.body["changed"], json!([]));
    // A url with a query string and port is fine.
    let r = w
        .fill(
            "ana",
            json!({"waiting_on_you": [{"system": "slack", "who": "B", "summary": "x",
                "url": "http://localhost:8080/a?b=c#d"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
}

#[tokio::test]
async fn with_the_brief_off_everything_is_501_not_built() {
    // Flag off (no token either): the app hides the section.
    let w = web().await;
    let b = w.login("ana").await;
    assert_eq!(
        w.get(&b, "/api/brief").await.status,
        StatusCode::NOT_IMPLEMENTED
    );
    let r = w.fill_with("ana", Some(BRIEF_TOKEN), json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::NOT_IMPLEMENTED, "not_built")
    );
    // Flag on but no token configured: nothing can fill it, and the route says so the same.
    let w = web_full(
        |_| {},
        BriefConfig {
            enabled: true,
            token: None,
        },
    )
    .await;
    let b = w.login("ana").await;
    assert_eq!(w.get(&b, "/api/brief").await.status, StatusCode::OK);
    let r = w.fill_with("ana", Some(BRIEF_TOKEN), json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::NOT_IMPLEMENTED, "not_built")
    );
    // Token set but flag off.
    let w = web_full(
        |_| {},
        BriefConfig {
            enabled: false,
            token: Some(BRIEF_TOKEN.into()),
        },
    )
    .await;
    let r = w.fill("ana", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::NOT_IMPLEMENTED, "not_built")
    );
}
