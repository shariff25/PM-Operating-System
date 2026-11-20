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
use pmos_web::followups::{
    BriefConfig, Changed, Day, FollowUp, FollowUpStore, MemoryFollowUpStore, NewFollowUp,
    StoreError,
};
use pmos_web::session::MemorySessionStore;
use pmos_web::settings::MemorySettingsStore;
use pmos_web::upstream::{Credentials, Signer};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

mod support;
use support::FakeDirectory;

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
    /// The body exactly as sent, and the user assertion, if any.
    raw: Vec<u8>,
    assertion: Option<String>,
}

#[derive(Default)]
struct Upstream {
    calls: Mutex<Vec<Call>>,
    /// Sign-in events (`POST /internal/admin/events`), kept apart from `calls`.
    sign_ins: Mutex<Vec<Call>>,
    /// The sign-in events route answers `503`, as when the audit log is down.
    sign_ins_down: std::sync::atomic::AtomicBool,
    /// Tasks (runtime) or approval items (tool server).
    data: Mutex<Vec<Value>>,
    next: AtomicUsize,
    /// The tool server's user database routes (tool server only).
    directory: FakeDirectory,
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
            raw: body.to_vec(),
            assertion: header("x-pmos-assertion"),
        });
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Calls that change something upstream: tasks made, decisions sent. A saved edit
    /// (`PUT …/draft`, see [`Upstream::saves`]) is not one: it decides and writes nothing.
    fn writes(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method != "GET" && !c.uri.contains("/draft?"))
            .collect()
    }

    /// Decisions sent (`POST …/decision`).
    fn decisions(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == "POST" && c.uri.ends_with("/decision"))
            .collect()
    }

    /// Saved edits sent (`PUT …/draft`).
    fn saves(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == "PUT" && c.uri.contains("/draft?"))
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
    if uri.path() == "/healthz" {
        return "ok".into_response();
    }
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
            json!({"page_id": "9", "version": 3, "body": "Race"}),
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
    if uri.path() == "/healthz" {
        return "ok".into_response();
    }
    // The user database: answered apart, so the tests that count calls count what they
    // always did.
    if let Some(res) = up.directory.handle(&method, &uri, &body) {
        return res;
    }
    if uri.path() == "/internal/admin/events" {
        // Recorded apart, so the tests that count calls count what they always did.
        up.record(&method, &uri, &headers, &body);
        let c = up.calls.lock().unwrap().pop().unwrap();
        up.sign_ins.lock().unwrap().push(c);
        if up.sign_ins_down.load(Ordering::SeqCst) {
            return coded(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        }
        return Json(json!({"recorded": true})).into_response();
    }
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
            // Like the tool server (R12): every pending item, and `truncated` only past its
            // safety cap, which a test marks with `_past_the_cap` on an item.
            let pending = list.iter().filter(|i| i["status"] == "pending").count();
            let truncated = list.iter().any(|i| i["_past_the_cap"] == json!(true));
            let total = if truncated { 1500 } else { pending };
            Json(json!({ "items": list, "truncated": truncated, "total_pending": total }))
                .into_response()
        }
        // Like the tool server: only for the item's own user.
        ("GET", ["internal", "approvals", id, "sources"]) => {
            let user = query(&uri, "user").unwrap_or_default();
            match up.find(id) {
                Some(i) if i["sources_down"] == json!(true) => {
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
                Some(i) if i["user"] == json!(user) => {
                    // Each with the ref its read gave the bot, except where a tool server
                    // from before markers sent none (the docs and Slack ones here).
                    let sources = if *id == "apr_conf" || *id == "apr_cited" {
                        json!([
                            {"system": "jira", "title": "PAY-212", "ref": "PAY-212",
                             "cite": "src-a1b2c3",
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
                             "cite": "src-d4e5f6",
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
        // Like the tool server: `pmos-web` must name the item's own user (S2), else `404`.
        ("GET", ["internal", "approvals", id]) => match up.find(id) {
            Some(i) if query(&uri, "user").is_some_and(|u| i["user"] == json!(u)) => {
                Json(i).into_response()
            }
            _ => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no such approval item"})),
            )
                .into_response(),
        },
        // Like the tool server: the owner saves an edit of a pending item; where it goes
        // can't change.
        ("PUT", ["internal", "approvals", id, "draft"]) => {
            let Some(it) = up.find(id) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if query(&uri, "user").is_none_or(|u| it["user"] != json!(u)) {
                return StatusCode::NOT_FOUND.into_response();
            }
            if it["status"] != json!("pending") {
                return coded(StatusCode::CONFLICT, "not_pending");
            }
            if it["_refuse_save"] == json!(true) {
                return coded(StatusCode::UNPROCESSABLE_ENTITY, "target_changed");
            }
            let req: Value = serde_json::from_slice(&body).unwrap();
            let args = req["args"].clone();
            for key in [
                "channel",
                "thread_ts",
                "space",
                "parent_id",
                "project",
                "epic",
                "page_id",
                "version",
            ] {
                if args.get(key) != it["args"].get(key) {
                    return coded(StatusCode::UNPROCESSABLE_ENTITY, "target_changed");
                }
            }
            if it.get("_original").is_none() {
                up.set(id, "_original", it["args"].clone());
            }
            let original = up.find(id).unwrap()["_original"].clone();
            up.set(id, "args", args.clone());
            up.set(id, "edited", json!(args != original));
            up.set(id, "edited_at", json!("2026-11-16T09:30:00Z"));
            Json(up.find(id).unwrap()).into_response()
        }
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
            // Like the tool server (R4): an approve names the args hash the PM was shown, and
            // a draft saved since (here: in another tab, between pmos-web's read and this
            // decision) is refused as `stale` and left as it is.
            if it["_saved_meanwhile"] == json!(true) {
                up.set(id, "args_hash", json!("h-saved-meanwhile"));
            }
            let now_hash = up.find(id).unwrap()["args_hash"].clone();
            match req["expected_args_hash"].as_str() {
                None if decision == "approve" => {
                    return coded(StatusCode::BAD_REQUEST, "expected_args_hash_required")
                }
                Some(h) if json!(h) != now_hash => return coded(StatusCode::CONFLICT, "stale"),
                _ => {}
            }
            // The answer is lost: a `500` before deciding anything, or after deciding.
            match it["_lose_answer"].as_str() {
                Some("before") => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                Some("after") => {
                    up.set(
                        id,
                        "status",
                        json!(if decision == "reject" {
                            "rejected"
                        } else {
                            "approved"
                        }),
                    );
                    up.set(id, "decided_by", req["decided_by"].clone());
                    return StatusCode::BAD_GATEWAY.into_response();
                }
                _ => {}
            }
            if decision != "reject" {
                match *id {
                    "apr_ext" if req["confirm_external"] != json!(true) => {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({"code": "needs_external_confirm",
                                        "error": "needs_external_confirm from the tool server: the text was 'Hello partners'",
                                        "members": 14, "external": true})),
                        )
                            .into_response();
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
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        password_login: false,
        trusted_proxies: Vec::new(),
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
    tools_up.directory.seed_team(TEAM);
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
    /// Send a write with neither `Origin` nor `Referer` (not something a browser does).
    no_origin: bool,
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
            // As every browser does on a `fetch` write: the page's own origin, unless the test
            // sends its own `Origin` or `Referer`, or none at all.
            let own = b
                .extra
                .iter()
                .any(|(k, _)| matches!(*k, "origin" | "referer"));
            if !own && !b.no_origin {
                req = req.header("origin", self.base.clone());
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

// Password sign-in (docs/password-sign-in.md; the flows run against the real tool server in
// crates/stack-tests).

#[tokio::test]
async fn password_sign_in_is_off_unless_turned_on() {
    let w = web().await;
    let r = w.get(&Browser::default(), "/api/auth/providers").await;
    assert_eq!(r.body["providers"], json!(["dev"]));
    for path in [
        "/api/auth/password/sign-in",
        "/api/auth/password/second-step",
        "/api/auth/password/setup/begin",
        "/api/auth/password/setup/password",
        "/api/auth/password/setup/authenticator",
        "/api/auth/password/setup/confirm",
    ] {
        let r = w.post(&Browser::default(), path, json!({})).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{path}"
        );
    }
    let ana = w.login("ana").await;
    for path in [
        "/api/auth/password/change",
        "/api/auth/password/recovery-codes",
    ] {
        let r = w.post(&ana, path, json!({})).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        w.get(&ana, "/api/auth/password/status").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        !w.tools
            .calls()
            .iter()
            .any(|c| c.uri.contains("/internal/password")),
        "nothing reached the tool server's password routes"
    );
}

#[tokio::test]
async fn password_sign_in_is_listed_and_its_writes_need_the_origin_and_json() {
    let w = web_with(|c| c.password_login = true).await;
    let r = w.get(&Browser::default(), "/api/auth/providers").await;
    assert_eq!(r.body["providers"], json!(["password", "dev"]));
    let elsewhere = Browser {
        extra: vec![("origin", "https://evil.example".into())],
        ..Browser::default()
    };
    let r = w
        .post(
            &elsewhere,
            "/api/auth/password/sign-in",
            json!({"sign_in_name": "ana@example.com", "password": "x"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (StatusCode::FORBIDDEN, "bad_origin"));
    let r = w
        .send(
            &Browser {
                extra: vec![("content-type", "text/plain".into())],
                ..Browser::default()
            },
            Method::POST,
            "/api/auth/password/sign-in",
            None,
        )
        .await;
    assert_eq!(r.code(), "bad_content_type");
    assert!(
        !w.tools
            .calls()
            .iter()
            .any(|c| c.uri.contains("/internal/password")),
        "refused before the tool server was asked"
    );
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

/// Sign-ins, sign-outs and refused sign-ins go to the tool server's audit log
/// (`POST /internal/admin/events`): fixed actions and codes, the member as `user=`, and for a
/// refusal no one, with only a short keyed hash of the name tried.
#[tokio::test]
async fn sign_ins_and_sign_outs_are_audited() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.post(&b, "/api/auth/logout", json!(null)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    for tried in ["mallory", " mallory ", "eve"] {
        let r = w
            .post(
                &Browser::default(),
                "/api/auth/dev-login",
                json!({ "handle": tried }),
            )
            .await;
        assert_eq!(r.code(), "not_a_member");
    }
    let events = w.tools.sign_ins.lock().unwrap().clone();
    assert_eq!(events.len(), 5, "{events:?}");
    for c in &events {
        assert_eq!(c.method, "POST");
        assert_eq!(c.auth, format!("Bearer {TOOLS_TOKEN}"));
        assert!(c.uri.starts_with("/internal/admin/events"));
    }
    assert_eq!(events[0].uri, "/internal/admin/events?user=ana");
    assert_eq!(
        events[0].body,
        json!({"action": "web.sign_in", "provider": "dev"})
    );
    assert_eq!(events[1].uri, "/internal/admin/events?user=ana");
    assert_eq!(
        events[1].body,
        json!({"action": "web.sign_out", "provider": "dev"})
    );
    let attempts: Vec<String> = events[2..]
        .iter()
        .map(|c| {
            assert_eq!(c.uri, "/internal/admin/events", "a refusal names no one");
            assert_eq!(c.body["action"], "web.sign_in_refused");
            assert_eq!(c.body["provider"], "dev");
            assert_eq!(c.body["reason"], "not_a_member");
            let raw = String::from_utf8_lossy(&c.raw).to_lowercase();
            assert!(
                !raw.contains("mallory") && !raw.contains("eve") && !raw.contains('@'),
                "never the name tried: {raw}"
            );
            let a = c.body["attempt"].as_str().unwrap().to_string();
            assert!(
                a.len() == 12 && a.bytes().all(|b| b.is_ascii_hexdigit()),
                "{a}"
            );
            a
        })
        .collect();
    assert_eq!(attempts[0], attempts[1], "the same name, the same hash");
    assert_ne!(attempts[0], attempts[2]);
    assert_eq!(
        w.get(&Browser::default(), "/readyz").await.body["unrecorded_sign_in_events"],
        0
    );
    w.assert_no_tokens();
}

/// The audit log down: sign-in and sign-out still work, and `/readyz` counts what wasn't
/// recorded.
#[tokio::test]
async fn sign_in_and_out_go_ahead_when_the_audit_log_is_down() {
    let w = web().await;
    w.tools.sign_ins_down.store(true, Ordering::SeqCst);
    let b = w.login("ana").await;
    assert_eq!(w.get(&b, "/api/me").await.status, StatusCode::OK);
    let r = w.post(&b, "/api/auth/logout", json!(null)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(w.get(&b, "/api/me").await.status, StatusCode::UNAUTHORIZED);
    let r = w.get(&Browser::default(), "/readyz").await;
    assert_eq!(r.status, StatusCode::OK, "not a readiness failure");
    assert_eq!(r.body["unrecorded_sign_in_events"], 2);
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
    // Development sign-in is by handle only (it resolves in the user database by handle);
    // an email is malformed.
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "ana@example.com"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    w.login("ana").await;
}

#[tokio::test]
async fn removing_a_member_ends_their_access_at_once() {
    let w = web().await;
    let b = w.login("ana").await;
    let other = w.login("ana").await;
    let ana = w.tools.directory.get("ana").unwrap();
    w.tools.directory.remove("ana");
    // Within the 10 seconds an answer is cached for, at the latest.
    w.advance(Duration::seconds(10));
    let r = w.get(&b, "/api/drafts").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "not_a_member");
    // Every session of theirs is gone for good, even if they are added back.
    w.tools.directory.add(ana);
    w.advance(Duration::seconds(10));
    assert_eq!(w.get(&b, "/api/me").await.code(), "signed_out");
    assert_eq!(w.get(&other, "/api/me").await.code(), "signed_out");
    assert!(w.tools.calls().is_empty());
    // `team.yaml` decides nothing: someone only in the file can't sign in.
    std::fs::write(
        w.dir.path().join("team.yaml"),
        "members:\n  - {handle: mallory, slack-id: U0MAL, roles: [pm, admin]}\n",
    )
    .unwrap();
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "mallory"}),
        )
        .await;
    assert_eq!(r.code(), "not_a_member");
}

#[tokio::test]
async fn a_deactivated_user_loses_access_within_ten_seconds() {
    let w = web().await;
    let b = w.login("ana").await;
    w.tools
        .directory
        .update("ana", |u| u.status = "deactivated".into());
    // Still the cached answer for up to 10 seconds...
    w.advance(Duration::seconds(9));
    assert_eq!(w.get(&b, "/api/me").await.status, StatusCode::OK);
    // ...and not a moment longer.
    w.advance(Duration::seconds(1));
    let r = w.get(&b, "/api/drafts").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "not_a_member");
    // A new sign-in is refused, and recorded as `deactivated`.
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "ana"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.headers.get("set-cookie").is_none());
    let last = w.tools.sign_ins.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.body["reason"], "deactivated");
    assert!(last.body.get("attempt").is_none());
    // Reactivated: they sign in again (their old session stays ended).
    w.tools
        .directory
        .update("ana", |u| u.status = "active".into());
    assert_eq!(w.get(&b, "/api/me").await.code(), "signed_out");
    w.advance(Duration::seconds(10));
    w.login("ana").await;
}

#[tokio::test]
async fn me_and_roles_come_from_the_user_database() {
    let w = web().await;
    w.tools
        .directory
        .update("ana", |u| u.display_name = "Ana <b>Silva</b>".into());
    let b = w.login("ana").await;
    let me = w.get(&b, "/api/me").await.body;
    assert_eq!(me["handle"], "ana");
    // Untrusted text, passed on as text: the app renders it plainly.
    assert_eq!(me["display_name"], "Ana <b>Silva</b>");
    assert_eq!(me["name"], "Ana <b>Silva</b>");
    assert_eq!(me["roles"], json!(["pm"]));
    // A role granted in the database is seen within 10 seconds; the file is never read.
    w.tools
        .directory
        .update("ana", |u| u.roles = vec!["pm".into(), "admin".into()]);
    w.advance(Duration::seconds(10));
    let me = w.get(&b, "/api/me").await.body;
    assert_eq!(me["roles"], json!(["pm", "admin"]));
    assert!(me["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("manage-connectors")));
    // With no display name, `name` is the handle.
    let ben = w.login("ben").await;
    let me = w.get(&ben, "/api/me").await.body;
    assert_eq!(me["display_name"], "");
    assert_eq!(me["name"], "ben");
}

#[tokio::test]
async fn sign_in_fails_closed_when_the_tool_server_is_unreachable() {
    let w = web().await;
    w.tools.directory.set_down(true);
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "ana"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.code(), "users_unavailable");
    assert!(r.headers.get("set-cookie").is_none());
    let last = w.tools.sign_ins.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.body["reason"], "users_unavailable");
    w.tools.directory.set_down(false);
    w.login("ana").await;
}

/// Spec §2, "When the store is unavailable": an existing session may read pmos-web's own
/// store on its last-known-good answer for at most 5 minutes, with Slack items withheld;
/// every write, and every route that goes to the tool server or the runtime, fails closed.
#[tokio::test]
async fn on_the_last_known_good_path_slack_is_withheld_and_writes_fail() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    let r = w
        .fill(
            "ana",
            json!({
                "date": "2026-11-16",
                "changed": [
                    {"system": "jira", "title": "PAY-212 moved to Blocked", "summary": "by Ben",
                     "url": "https://jira.example.com/PAY-212"},
                    {"system": "slack", "title": "A thread in #pricing", "summary": "3 replies",
                     "url": "https://slack.example.com/t/1"},
                ],
                "waiting_on_you": [follow("slack", "Ben", "asked about the pricing page", 1),
                                   follow("jira", "Cy", "a review", 3)],
                "you_wait_on": [follow("slack", "Legal", "the DPA question", 2)],
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let fresh = w.get(&b, "/api/brief").await.body;
    assert_eq!(fresh["slack_withheld"], false);
    assert!(fresh.get("note").is_none());
    assert_eq!(fresh["changed"].as_array().unwrap().len(), 2);
    let slack_id = fresh["waiting_on_you"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    w.tools.directory.set_down(true);
    w.advance(Duration::seconds(30));
    // Reads from pmos-web's own store: on the last-known-good answer.
    let me = w.get(&b, "/api/me").await;
    assert_eq!(me.status, StatusCode::OK, "{:?}", me.body);
    assert_eq!(me.body["roles"], json!(["pm"]));
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["slack_withheld"], true);
    assert_eq!(
        r.body["note"],
        "Slack items are hidden until PM OS can check your access"
    );
    let systems = |list: &str| -> Vec<String> {
        r.body[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["system"].as_str().unwrap().to_lowercase())
            .collect()
    };
    assert_eq!(systems("changed"), ["jira"]);
    assert_eq!(systems("waiting_on_you"), ["jira"]);
    assert!(systems("you_wait_on").is_empty());
    assert!(!r.body.to_string().contains("pricing"));
    assert!(!r.body.to_string().contains("DPA"));
    // Everything that reaches another service, and every write, fails closed.
    let calls = w.tools.calls().len();
    for path in ["/api/drafts", "/api/tasks", "/api/admin/audit"] {
        let r = w.get(&b, path).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(r.code(), "users_unavailable", "{path}");
    }
    for (method, path, body) in [
        (
            Method::PUT,
            "/api/me/settings".to_string(),
            json!({"nudges": false}),
        ),
        (
            Method::POST,
            "/api/me/hints".to_string(),
            json!({"seen": "ask"}),
        ),
        (
            Method::POST,
            format!("/api/followups/{slack_id}/dismiss"),
            json!(null),
        ),
        (
            Method::POST,
            "/api/tasks".to_string(),
            json!({"text": "hi"}),
        ),
        (
            Method::POST,
            "/api/drafts/practice".to_string(),
            json!(null),
        ),
    ] {
        let r = w.send(&b, method, &path, Some(body)).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(r.code(), "users_unavailable", "{path}");
    }
    assert_eq!(
        w.tools.calls().len(),
        calls,
        "nothing reached the tool server"
    );
    assert!(w.runtime.calls().is_empty(), "nothing reached the runtime");
    // At five minutes the last-known-good answer still reads...
    w.advance(Duration::seconds(270));
    assert_eq!(w.get(&b, "/api/me").await.status, StatusCode::OK);
    // ...and past them, reads fail too.
    w.advance(Duration::seconds(1));
    for path in ["/api/me", "/api/brief"] {
        let r = w.get(&b, path).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(r.code(), "users_unavailable");
    }
    // Back: everything again, Slack items included.
    w.tools.directory.set_down(false);
    let r = w.get(&b, "/api/brief").await;
    assert_eq!(r.body["slack_withheld"], false);
    assert_eq!(r.body["changed"].as_array().unwrap().len(), 2);
    let r = w.post(&b, "/api/me/hints", json!({"seen": "ask"})).await;
    assert_eq!(r.status, StatusCode::OK);
}

/// A user deactivated while the tool server can't be reached keeps, at worst, the
/// last-known-good reads (spec §2's stated bound); one whose last answer said deactivated
/// gets nothing.
#[tokio::test]
async fn a_deactivated_last_known_answer_reads_nothing() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    w.tools
        .directory
        .update("ana", |u| u.status = "deactivated".into());
    w.advance(Duration::seconds(10));
    // This answer says deactivated...
    assert_eq!(w.get(&b, "/api/me").await.code(), "not_a_member");
    // ...and the session is gone, so an outage after it serves nothing.
    w.tools.directory.set_down(true);
    assert_eq!(w.get(&b, "/api/brief").await.code(), "signed_out");
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
    // A write with neither `Origin` nor `Referer` is refused, even with the token, and so
    // is a sign-in.
    let bare = Browser {
        no_origin: true,
        ..ana.clone()
    };
    for (method, path, body) in [
        (Method::POST, "/api/tasks", Some(ask.clone())),
        (
            Method::PUT,
            "/api/drafts/apr_conf",
            Some(json!({"body": "x"})),
        ),
        (Method::DELETE, "/api/me/hints", None),
        (Method::POST, "/api/auth/logout", None),
    ] {
        let r = w.send(&bare, method, path, body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.code(), "bad_origin", "{path}");
    }
    let bare_login = Browser {
        no_origin: true,
        ..Browser::default()
    };
    assert_eq!(
        w.post(&bare_login, "/api/auth/dev-login", json!({"handle": "ana"}))
            .await
            .code(),
        "bad_origin"
    );
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
    assert_eq!(me["mascot"], true);
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
    // The guided start's marks are kept with the hints, and cleared with them.
    let r = w
        .post(&b, "/api/me/hints", json!({"seen": "tour_skipped"}))
        .await;
    assert_eq!(
        r.body["hints_seen"],
        json!(["ask", "sources", "tour_skipped"])
    );
    // Hiding Nudge is its own setting: it leaves nudges, and hints, as they were.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/me/settings",
            Some(json!({"mascot": false})),
        )
        .await;
    assert_eq!(r.body["mascot"], false);
    assert_eq!(r.body["nudges"], false);
    let r = w.send(&b, Method::DELETE, "/api/me/hints", None).await;
    assert_eq!(r.body["hints_seen"], json!([]));
    assert_eq!(r.body["nudges"], false);
    assert_eq!(r.body["mascot"], false);

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
    // Nothing left out (R12).
    assert_eq!(r.body["truncated"], false);
    // As the tool server counts them (one pending item has no summary here, so isn't listed).
    assert_eq!(r.body["total_pending"], 8);
    // Asked for this PM only.
    assert!(w
        .tools
        .calls()
        .iter()
        .all(|c| c.uri == "/internal/approvals?user=ana"
            && c.auth == format!("Bearer {TOOLS_TOKEN}")));
    // An inbox the tool server cut at its safety cap says so, with the total.
    w.tools.set("apr_conf", "_past_the_cap", json!(true));
    let r = w.get(&b, "/api/drafts").await;
    assert_eq!(r.body["truncated"], true);
    assert_eq!(r.body["total_pending"], 1500);
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

/// R4: a save that lands between pmos-web's read and the decision (another tab) is caught by
/// the tool server, which refuses the stale approve; pmos-web answers `409 stale` and nothing
/// is approved. A PM's typed edit names no revision run.
#[tokio::test]
async fn a_save_between_the_read_and_the_approve_is_stale() {
    let w = web().await;
    let b = w.login("ana").await;
    let d = w.get(&b, "/api/drafts/apr_jira").await.body;
    w.tools.set("apr_jira", "_saved_meanwhile", json!(true));
    let r = w
        .post(
            &b,
            "/api/drafts/apr_jira/approve",
            json!({"version": d["version"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
    assert_eq!(r.code(), "stale");
    let decisions = w.tools.decisions();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].body["expected_args_hash"], "h");
    assert_eq!(w.tools.find("apr_jira").unwrap()["status"], "pending");

    // A typed edit carries no revision: the draft keeps its own level.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_jira",
            Some(json!({"body": "Typed by Ana"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let save = w.tools.saves().pop().unwrap();
    assert!(save.body.get("revision_run").is_none(), "{}", save.body);
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
    // With the args hash of the draft as read (R4).
    assert_eq!(
        writes[0].body,
        json!({"decided_by": "ana", "decision": "approve", "expected_args_hash": "h"})
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
    assert_eq!(r.body["edited_at"], "2026-11-16T09:30:00Z");
    assert_eq!(r.body["body"], "# PRD\nNew text");
    assert_eq!(r.body["title"], "PRD: grace period v2");
    let edited = json!({"space": "PM", "title": "PRD: grace period v2",
                        "body": "# PRD\nNew text", "parent_id": "77"});
    // Saving stores the edit on the item at the tool server, for the PM, with an assertion
    // for them; no decision is made and nothing goes to Confluence.
    let saves = w.tools.saves();
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0].uri, "/internal/approvals/apr_conf/draft?user=ana");
    assert_eq!(saves[0].body, json!({ "args": edited }));
    assert!(w.tools.decisions().is_empty());
    assert_eq!(w.tools.find("apr_conf").unwrap()["args"], edited);
    // The edit shows in the list and on reload.
    let list = w.get(&b, "/api/drafts").await.body;
    assert_eq!(list["drafts"][0]["title"], "PRD: grace period v2");
    let shown = w.get(&b, "/api/drafts/apr_conf").await.body;
    assert_eq!(shown["edited"], true);
    // A body-only edit keeps the saved title.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_conf",
            Some(json!({"body": "# PRD\nNewer text"})),
        )
        .await;
    assert_eq!(r.body["title"], "PRD: grace period v2");
    assert_ne!(
        r.body["version"], shown["version"],
        "a new edit, a new version"
    );
    let edited = json!({"space": "PM", "title": "PRD: grace period v2",
                        "body": "# PRD\nNewer text", "parent_id": "77"});

    let r = w.post(&b, "/api/drafts/apr_conf/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["body"], "# PRD\nNewer text");
    let decisions = w.tools.decisions();
    assert_eq!(decisions.len(), 1);
    // Exactly the arguments the PM saw.
    assert_eq!(
        decisions[0].body,
        json!({"decided_by": "ana", "decision": "edit", "args": edited,
               "expected_args_hash": "h"})
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
        w.tools.decisions()[1].body["args"],
        json!({"channel": "C01", "thread_ts": "1731.0002", "text": "On it, today"})
    );
}

#[tokio::test]
async fn an_edit_saved_in_the_web_app_is_what_slack_shows_and_approves() {
    let w = web().await;
    let b = w.login("ana").await;
    let opened = w.get(&b, "/api/drafts/apr_slack").await.body;
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_slack",
            Some(json!({"body": "On it, edited in the web app"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let version = r.body["version"].clone();
    assert_ne!(version, opened["version"]);
    // What the Slack card reads from the tool server is now the edit.
    let stored = w.tools.find("apr_slack").unwrap();
    assert_eq!(stored["args"]["text"], "On it, edited in the web app");
    assert_eq!(stored["edited"], true);
    // The PM approves it on the Slack card: the tool server writes the stored arguments.
    w.tools.set("apr_slack", "status", json!("approved"));
    w.tools.set("apr_slack", "decided_by", json!("ana"));
    w.tools.set(
        "apr_slack",
        "result",
        json!({"url": "https://slack.com/archives/C01/p17310002"}),
    );
    // The web app shows it decided, with the edited text.
    let d = w.get(&b, "/api/drafts/apr_slack").await.body;
    assert_eq!(d["status"], "approved");
    assert_eq!(d["body"], "On it, edited in the web app");
    // An approve of the version the web app showed is the same approval: nothing is sent.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_slack/approve",
            json!({"version": version}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    // The version shown before the edit is stale.
    let r = w
        .post(
            &b,
            "/api/drafts/apr_slack/approve",
            json!({"version": opened["version"]}),
        )
        .await;
    assert_eq!(r.code(), "not_pending");
    assert!(w.tools.decisions().is_empty());
    // Saving once decided is not_pending; the tool server refused it.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_slack",
            Some(json!({"body": "Too late"})),
        )
        .await;
    assert_eq!(r.code(), "not_pending");
}

#[tokio::test]
async fn an_edit_that_moves_the_draft_is_refused_and_no_edit_is_kept_here() {
    let w = web().await;
    let b = w.login("ana").await;
    // The tool server refuses (here: as if it had moved); the app gets `bad_edit`.
    w.tools.set("apr_jira", "_refuse_save", json!(true));
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_jira",
            Some(json!({"body": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.code(), "bad_edit");
    assert!(!r.body["error"].as_str().unwrap().contains("tool server"));
    let d = w.get(&b, "/api/drafts/apr_jira").await.body;
    assert_eq!(d["edited"], false, "nothing saved anywhere");
    assert_eq!(d["body"], "As a PM…");
}

#[tokio::test]
async fn passes_through_the_tool_servers_conflicts() {
    let w = web().await;
    let b = w.login("ana").await;
    let r = w.post(&b, "/api/drafts/apr_ext/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "needs_external_confirm");
    // A fixed sentence: the tool server's own text is never passed on.
    assert!(!r.body["error"]
        .as_str()
        .unwrap()
        .contains("from the tool server"));
    assert!(r.body["error"]
        .as_str()
        .unwrap()
        .contains("Nothing was sent"));
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
    // R2: the prompt carries the draft's text, so the revision starts at the draft's level
    // (unrecorded counts as restricted).
    let level = w.tools.find("apr_conf").unwrap()["sources_sensitivity"]
        .as_str()
        .unwrap_or("restricted")
        .to_string();
    assert_eq!(call.body["sensitivity_floor"], json!(level));
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
        // A marker the bot copied into its answer: a suggestion is the PM's text, so it has
        // none.
        json!("```markdown\n# PRD\nShort text [[src-a1b2c3]]\n```"),
    );
    let r = w.get(&b, "/api/drafts/apr_conf/suggestion").await;
    assert_eq!(r.body["suggestion"]["status"], "ready");
    assert_eq!(r.body["suggestion"]["body"], "# PRD\nShort text");

    // The revision read something restricted; the runtime recorded it.
    w.runtime
        .set(task_id, "sources_sensitivity", json!("restricted"));
    let r = w
        .post(&b, "/api/drafts/apr_conf/suggestion/accept", json!(null))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["body"], "# PRD\nShort text");
    assert_eq!(r.body["edited"], true);
    // R2: the save names the revision task, so the tool server raises the draft to what it
    // read; the runtime's level goes along as a floor only.
    let save = w.tools.saves().pop().unwrap();
    assert_eq!(save.body["revision_run"], task_id);
    assert_eq!(save.body["revision_sensitivity"], "restricted");
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
        json!({"decided_by": "ana", "decision": "approve", "expected_args_hash": "h"})
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
    // Nothing listens on the runtime's port; the tool server answers who is signed in.
    let w = web_with(|c| c.bots_url = "http://127.0.0.1:9".into()).await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/tasks").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.code(), "upstream_unavailable");
    let r = w.post(&b, "/api/tasks", json!({"text": "hi"})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(r.body["error"].as_str().unwrap().contains("bot runtime"));
    w.assert_no_tokens();
    // Nothing listens on the tool server's either: nobody can be signed in at all.
    let dir = tempfile::tempdir().unwrap();
    let cfg = config("http://127.0.0.1:9", "http://127.0.0.1:9", dir.path());
    let (base, _stop) = serve(pmos_web::build(cfg, Deps::in_memory()).unwrap()).await;
    let r = reqwest::Client::new()
        .post(format!("{base}/api/auth/dev-login"))
        .header("origin", base.clone())
        .json(&json!({"handle": "ana"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["code"], "users_unavailable");
    assert!(!body.to_string().contains(TOOLS_TOKEN));
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
             "url": "https://acme.atlassian.net/browse/PAY-212", "excerpt": null,
             "cited": false},
            {"id": "s2", "system": "docs", "title": "grace period",
             "url": "https://docs.example.com/grace.md", "excerpt": null, "cited": false},
            // A link that isn't https is dropped; the source stays.
            {"id": "s3", "system": "slack", "title": "Slack thread", "url": null,
             "excerpt": null, "cited": false},
            {"id": "s4", "system": "context", "title": "Pricing principles", "url": null,
             "excerpt": null, "cited": false},
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

/// The bot's source markers, resolved to the sources its task read (`docs/web-api.md`,
/// Citations): `[[sN]]` in the body and `cited` on each source; none after an edit.
#[tokio::test]
async fn a_drafts_source_markers_become_citations_until_the_pm_edits_it() {
    let w = web().await;
    let b = w.login("ana").await;
    let raw = "# Grace [[src-a1b2c3]]\n\nSeven days [[src-a1b2c3]] [[src-d4e5f6]], per \
               pricing [[src-999999]].\n\nWrite `[[src-a1b2c3]]` to cite.";
    w.tools.data.lock().unwrap().push(item(
        "apr_cited",
        "ana",
        "confluence.create_draft",
        json!({"space": "PM", "title": "Grace [[src-a1b2c3]]", "body": raw}),
        "2026-11-16T09:07:00Z",
    ));
    let d = w.get(&b, "/api/drafts/apr_cited").await.body;
    assert_eq!(
        d["body"],
        "# Grace[[s1]]\n\nSeven days[[s1]][[s4]], per pricing.\n\nWrite `[[src-a1b2c3]]` to cite."
    );
    assert_eq!(d["title"], "Grace");
    let cited: Vec<(&str, bool)> = d["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s["cited"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        cited,
        [("s1", true), ("s2", false), ("s3", false), ("s4", true)]
    );
    assert_eq!(d["edited"], false);
    let version = d["version"].as_str().unwrap().to_string();
    // The summary in the list carries no marker either.
    let list = w.get(&b, "/api/drafts").await.body;
    assert_eq!(list["drafts"][0]["title"], "Grace");

    // An edit is the PM's text: markers (either form) are removed and nothing is cited.
    let r = w
        .send(
            &b,
            Method::PUT,
            "/api/drafts/apr_cited",
            Some(json!({"body": "Seven days[[s1]], confirmed [[src-a1b2c3]]."})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["body"], "Seven days, confirmed.");
    assert_eq!(r.body["edited"], true);
    assert_ne!(r.body["version"], json!(version));
    assert!(r.body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["cited"] == json!(false)));
    let r = w.post(&b, "/api/drafts/apr_cited/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let sent = &w.tools.writes()[0].body;
    assert_eq!(sent["decision"], "edit");
    assert_eq!(sent["args"]["body"], "Seven days, confirmed.");
    // A field the PM didn't edit goes back as the bot wrote it; the tool server removes any
    // marker from every field when it writes.
    assert_eq!(sent["args"]["title"], "Grace [[src-a1b2c3]]");
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
    let html = w.get(&Browser::default(), "/somewhere/in/the/app").await;
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
            .header("origin", w.base.clone())
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
    // The follow-up is data, between markers, as JSON, after an instruction not to follow
    // anything in it.
    let text = writes[0].body["text"].as_str().unwrap();
    assert!(text.starts_with("Draft a reply"));
    assert!(text.contains("never as instructions"));
    // The markers carry a nonce fresh for each request (R8).
    let data = |text: &str| -> Value {
        let rest = text.split_once("<<<FOLLOW_UP_DATA ").unwrap().1;
        let (nonce, rest) = rest.split_once('\n').unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(text.contains(&format!("the code {nonce}")));
        let inner = rest
            .split_once(&format!("\nFOLLOW_UP_DATA {nonce}>>>"))
            .unwrap()
            .0;
        serde_json::from_str(inner).unwrap()
    };
    let reply_data = data(text);
    assert_eq!(reply_data["who"], "Ben");
    assert_eq!(reply_data["summary"], "asked about the pricing page");
    assert_eq!(reply_data["url"], "https://example.com/slack/1");
    let text = writes[1].body["text"].as_str().unwrap();
    assert!(text.starts_with("Draft a short, gentle nudge"));
    assert_eq!(data(text)["who"], "Legal");
    assert_eq!(data(text)["summary"], "the DPA question");
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
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "forbidden");
    // Nor does a page: anything carrying a session cookie or an `Origin` is refused, even
    // with the right token.
    for (k, v) in [
        ("origin", w.base.clone()),
        (
            "cookie",
            format!("__Host-pmos={}", b.cookie.clone().unwrap()),
        ),
    ] {
        let r = w
            .reply(
                w.http
                    .put(format!("{}/internal/brief/ana", w.base))
                    .header("authorization", format!("Bearer {BRIEF_TOKEN}"))
                    .header(k, v)
                    .json(&body),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{k}");
    }
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

// User assertions (docs/user-assertion.md).

// Test keys only: fixed bytes, never used anywhere real.
const WEB_BOTS_KEY: &str = "d2ViLWJvdHMtdGVzdC1rZXktMDEyMzQ1Njc4OWFiY2Q=";
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

fn ring(id: &str, key: &str) -> pmos_assertion::KeyRing {
    pmos_assertion::KeyRing::new(pmos_assertion::Key::from_base64(id, key).unwrap())
}

/// pmos-web with a key for each upstream, as `PMOS_ASSERTION_KEYS` would give it.
async fn signing_web() -> Web {
    web_with(|cfg| {
        cfg.bots_token = Credentials::new(BOTS_TOKEN).with_signer(Signer::new(
            "web",
            "bots",
            ring("wb1", WEB_BOTS_KEY),
        ));
        cfg.toolserver_token = Credentials::new(TOOLS_TOKEN).with_signer(Signer::new(
            "web",
            "toolserver",
            ring("wt1", WEB_TOOLS_KEY),
        ));
    })
    .await
}

/// Verify `call`'s assertion as `audience` would: the web key, issuer `web`, this exact
/// request, once. Returns the subject.
fn verify(
    call: &Call,
    audience: &str,
    key: (&str, &str),
    cache: &pmos_assertion::MemoryReplayCache,
) -> Result<String, pmos_assertion::VerifyError> {
    verified(call, audience, key, cache).map(|a| a.subject().to_string())
}

/// As [`verify`], returning the whole assertion.
fn verified(
    call: &Call,
    audience: &str,
    key: (&str, &str),
    cache: &pmos_assertion::MemoryReplayCache,
) -> Result<pmos_assertion::Assertion, pmos_assertion::VerifyError> {
    let header = call.assertion.as_deref().expect("an assertion");
    let policy = pmos_assertion::VerifyPolicy::new(audience, &["web"]).unwrap();
    let binding = pmos_assertion::RequestBinding::new(&call.method, &call.uri, &call.raw).unwrap();
    let now = pmos_assertion::unix_now();
    let a = ring(key.0, key.1).verify(header, &policy, now, Some(&binding), cache)?;
    assert!(a.expires_at() - a.issued_at() <= 60, "lifetime");
    assert_eq!(a.roles(), &[pmos_contract::model::Role::Pm]);
    Ok(a)
}

/// The `sid` pmos-web signs for the browser `b`'s session.
fn session_ref(b: &Browser) -> String {
    let cookie = b.cookie.as_deref().expect("signed in");
    pmos_web::session::assertion_ref(&pmos_web::session::hash_id(cookie))
}

#[tokio::test]
async fn assertions_carry_a_session_reference_never_the_session() {
    let w = signing_web().await;
    let ana = w.login("ana").await;
    let again = w.login("ana").await;
    assert_ne!(session_ref(&ana), session_ref(&again));
    assert_eq!(w.get(&ana, "/api/tasks").await.status, StatusCode::OK);
    assert_eq!(w.get(&again, "/api/drafts").await.status, StatusCode::OK);
    let cache = pmos_assertion::MemoryReplayCache::default();
    let runtime = w.runtime.calls();
    let tools = w.tools.calls();
    let a = verified(&runtime[0], "bots", ("wb1", WEB_BOTS_KEY), &cache).unwrap();
    assert_eq!(a.session(), session_ref(&ana));
    let t = verified(&tools[0], "toolserver", ("wt1", WEB_TOOLS_KEY), &cache).unwrap();
    assert_eq!(t.session(), session_ref(&again));
    for (sid, b) in [(a.session(), &ana), (t.session(), &again)] {
        assert_eq!(sid.len(), 16);
        assert_ne!(sid, "unknown");
        let cookie = b.cookie.as_deref().unwrap();
        assert!(!cookie.contains(sid) && !sid.contains(cookie));
    }
    // The reference stays between pmos-web and its upstreams.
    for s in w.seen.lock().unwrap().iter() {
        assert!(!s.contains(&session_ref(&ana)) && !s.contains(&session_ref(&again)));
    }
}

/// A sign-in or sign-out event carries an assertion for the member, in the session it starts
/// or ends; a refusal carries none (it is no one's).
#[tokio::test]
async fn sign_in_events_are_signed_for_the_member() {
    let w = signing_web().await;
    let ana = w.login("ana").await;
    let r = w.post(&ana, "/api/auth/logout", json!(null)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = w
        .post(
            &Browser::default(),
            "/api/auth/dev-login",
            json!({"handle": "nobody"}),
        )
        .await;
    assert_eq!(r.code(), "not_a_member");
    let events = w.tools.sign_ins.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    let cache = pmos_assertion::MemoryReplayCache::default();
    for c in &events[..2] {
        let a = verified(c, "toolserver", ("wt1", WEB_TOOLS_KEY), &cache).unwrap();
        assert_eq!(a.subject(), "ana");
        assert_eq!(a.session(), session_ref(&ana));
    }
    assert!(events[2].assertion.is_none());
}

#[tokio::test]
async fn a_conflicting_decision_is_read_again_with_an_assertion() {
    let w = signing_web().await;
    let b = w.login("ana").await;
    // Decided in Slack a moment ago, and the same PM's other approve winning: both read the
    // item again after the tool server's `409`.
    let r = w.post(&b, "/api/drafts/apr_race/approve", json!({})).await;
    assert_eq!(r.code(), "not_pending");
    w.tools.data.lock().unwrap().push(item(
        "apr_dbl",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "issue_type": "Task", "summary": "Twice"}),
        "2026-11-16T09:07:00Z",
    ));
    let r = w.post(&b, "/api/drafts/apr_dbl/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    let cache = pmos_assertion::MemoryReplayCache::default();
    let calls = w.tools.calls();
    for id in ["apr_race", "apr_dbl"] {
        let path = format!("/internal/approvals/{id}?user=ana");
        // Every read names the PM and carries an assertion for them: the first (finding the
        // draft) and every read after the decision.
        let reads: Vec<&Call> = calls
            .iter()
            .filter(|c| c.method == "GET" && c.uri == path)
            .collect();
        assert!(reads.len() >= 2, "{id}: {reads:?}");
        for call in &reads[..1] {
            let a = verified(call, "toolserver", ("wt1", WEB_TOOLS_KEY), &cache)
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(a.subject(), "ana");
        }
        let decision = calls
            .iter()
            .position(|c| {
                c.method == "POST" && c.uri == format!("/internal/approvals/{id}/decision")
            })
            .expect("a decision");
        let after: Vec<&Call> = calls[decision..]
            .iter()
            .filter(|c| c.method == "GET" && c.uri == path)
            .collect();
        assert!(!after.is_empty(), "{id}: no read after the conflict");
        for call in after {
            let a = verified(call, "toolserver", ("wt1", WEB_TOOLS_KEY), &cache)
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(a.subject(), "ana");
            assert_eq!(a.session(), session_ref(&b));
        }
    }
}

#[tokio::test]
async fn every_upstream_call_for_a_pm_carries_a_valid_assertion() {
    let w = signing_web().await;
    let b = w.login("ana").await;
    // The runtime: a task, the list, one task and its events.
    let r = w
        .post(&b, "/api/tasks", json!({"text": "What did PAY ship?"}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(w.get(&b, "/api/tasks").await.status, StatusCode::OK);
    assert_eq!(
        w.get(&b, "/api/tasks/tsk_ana1").await.status,
        StatusCode::OK
    );
    // The tool server: the inbox, one draft with its sources, a decision each way.
    assert_eq!(w.get(&b, "/api/drafts").await.status, StatusCode::OK);
    assert_eq!(
        w.get(&b, "/api/drafts/apr_conf").await.status,
        StatusCode::OK
    );
    let r = w.post(&b, "/api/drafts/apr_jira/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = w
        .post(
            &b,
            "/api/drafts/apr_slack/reject",
            json!({"reason": "tone"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    let cache = pmos_assertion::MemoryReplayCache::default();
    let runtime = w.runtime.calls();
    assert!(runtime.len() >= 4, "{runtime:?}");
    for call in &runtime {
        assert_eq!(
            verify(call, "bots", ("wb1", WEB_BOTS_KEY), &cache).as_deref(),
            Ok("ana"),
            "{} {}",
            call.method,
            call.uri
        );
        // X-Pmos-User is still sent, and agrees.
        assert_eq!(call.user.as_deref(), Some("ana"));
    }
    let tools = w.tools.calls();
    assert!(tools.iter().any(|c| c.method == "POST"), "{tools:?}");
    for call in &tools {
        // Every call names the PM, a single item's read included (`?user=`), and carries an
        // assertion for them.
        if call.uri.starts_with("/internal/approvals/") && call.method == "GET" {
            assert!(call.uri.contains("user=ana"), "{}", call.uri);
        }
        assert_eq!(
            verify(call, "toolserver", ("wt1", WEB_TOOLS_KEY), &cache).as_deref(),
            Ok("ana"),
            "{} {}",
            call.method,
            call.uri
        );
    }
    // Each assertion is fresh: verifying them all again finds every one a replay.
    for call in runtime.iter().take(1) {
        assert_eq!(
            verify(call, "bots", ("wb1", WEB_BOTS_KEY), &cache),
            Err(pmos_assertion::VerifyError::Replay)
        );
    }
    // A runtime assertion is no good at the tool server, and the reverse.
    let fresh = pmos_assertion::MemoryReplayCache::default();
    assert_eq!(
        verify(&runtime[0], "toolserver", ("wb1", WEB_BOTS_KEY), &fresh),
        Err(pmos_assertion::VerifyError::WrongAudience)
    );
    // Assertions never reach the browser, and neither do keys.
    for s in w.seen.lock().unwrap().iter() {
        assert!(!s.contains("sig=") && !s.contains(WEB_BOTS_KEY) && !s.contains(WEB_TOOLS_KEY));
    }
    w.assert_no_tokens();
}

#[tokio::test]
async fn without_keys_no_assertion_is_sent() {
    let w = web().await;
    let b = w.login("ana").await;
    w.post(&b, "/api/tasks", json!({"text": "x"})).await;
    w.get(&b, "/api/drafts").await;
    assert!(w
        .runtime
        .calls()
        .iter()
        .chain(w.tools.calls().iter())
        .all(|c| c.assertion.is_none()));
}

// Review fixes: old links, what the approve dialog shows, unknown outcomes, the brief's
// delivery, readiness, HSTS, the internal listener and the periodic purge.

#[tokio::test]
async fn old_slack_links_redirect_to_the_apps_draft_route() {
    let w = web().await;
    let b = Browser::default();
    for (path, to) in [
        ("/drafts/apr_0123abcdef", "/#/d/apr_0123abcdef"),
        ("/d/apr_0123abcdef", "/#/d/apr_0123abcdef"),
        // Not an approval id: the app's home, never somewhere else.
        ("/drafts/prc_1", "/"),
        ("/drafts/apr_", "/"),
        ("/d/apr_x%2F..%2Fevil", "/"),
        ("/d/https:%2F%2Fevil.example", "/"),
    ] {
        let r = w.get(&b, path).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(r.headers["location"], to, "{path}");
    }
}

#[tokio::test]
async fn the_approve_dialog_has_what_the_slack_card_shows() {
    let w = web().await;
    let b = w.login("ana").await;
    w.tools
        .set("apr_conf", "sources_sensitivity", json!("internal"));
    let d = w.get(&b, "/api/drafts/apr_conf").await.body;
    assert_eq!(d["restricted_sources"], false);
    assert_eq!(d["audience"], Value::Null, "not a Slack draft");
    assert_eq!(d["marker"], "Drafted with PM OS · run tsk_ana1");
    assert_eq!(d["edited_at"], Value::Null);
    // A Slack draft: restricted when unrecorded; audience unknown until checked.
    let d = w.get(&b, "/api/drafts/apr_ext").await.body;
    assert_eq!(d["restricted_sources"], true);
    assert_eq!(d["marker"], "Drafted with PM OS");
    assert_eq!(d["audience"], json!({"members": null, "external": null}));
    assert_eq!(d["title"], "Message to a Slack channel");
    assert_eq!(d["target"]["label"], "A Slack channel");
    // The tool server's audience check counted 14 members, outside the company.
    let r = w.post(&b, "/api/drafts/apr_ext/approve", json!({})).await;
    assert_eq!(r.code(), "needs_external_confirm");
    assert!(!r.body["error"].as_str().unwrap().contains("Hello partners"));
    let d = w.get(&b, "/api/drafts/apr_ext").await.body;
    assert_eq!(d["audience"], json!({"members": 14, "external": true}));
    assert_eq!(d["external"], true);
}

#[tokio::test]
async fn a_lost_answer_says_the_outcome_is_unknown_unless_the_draft_shows_it() {
    let w = web().await;
    let b = w.login("ana").await;
    // Lost before anything was decided: the app can't say nothing was sent.
    w.tools.set("apr_jira", "_lose_answer", json!("before"));
    let r = w.post(&b, "/api/drafts/apr_jira/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.code(), "unknown_outcome");
    assert!(!r.body["error"].as_str().unwrap().contains("Nothing was"));
    // Lost after the tool server approved it: the draft read again shows the approval.
    w.tools.set("apr_conf", "_lose_answer", json!("after"));
    let r = w.post(&b, "/api/drafts/apr_conf/approve", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "approved");
    // The same for a rejection.
    w.tools.set("apr_slack", "_lose_answer", json!("after"));
    let r = w.post(&b, "/api/drafts/apr_slack/reject", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "rejected");
}

#[tokio::test]
async fn the_brief_says_whether_one_was_ever_delivered() {
    let w = web_brief().await;
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await.body;
    assert_eq!(r["status"], "never");
    assert_eq!(r["delivered_at"], Value::Null);
    // One that found nothing is still a delivery.
    assert_eq!(w.fill("ana", json!({})).await.status, StatusCode::OK);
    let r = w.get(&b, "/api/brief").await.body;
    assert_eq!(r["status"], "today");
    assert_eq!(r["delivered_at"], "2026-11-16T09:00:00Z");
    assert_eq!(r["changed"], json!([]));
    // A day later, with the day's items gone (and purged), it is stale, not "never".
    w.advance(Duration::hours(25));
    let b = w.login("ana").await;
    let r = w.get(&b, "/api/brief").await.body;
    assert_eq!(r["status"], "stale");
    assert_eq!(r["delivered_at"], "2026-11-16T09:00:00Z");
    // Another PM's is their own.
    let ben = w.login("ben").await;
    assert_eq!(w.get(&ben, "/api/brief").await.body["status"], "never");
}

#[tokio::test]
async fn readyz_checks_the_store_and_both_upstreams() {
    let w = web().await;
    let r = w.get(&Browser::default(), "/readyz").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        r.body,
        json!({"ready": true, "checks": {"store": "ok",
                                         "bot_runtime": "ok", "tool_server": "ok"},
               "unrecorded_sign_in_events": 0})
    );
    // `team.yaml` isn't what readiness depends on any more.
    std::fs::write(w.dir.path().join("team.yaml"), "members: [").unwrap();
    let r = w.get(&Browser::default(), "/readyz").await;
    assert_eq!(r.status, StatusCode::OK);
    // Upstreams that don't answer.
    let w = web_with(|c| {
        c.bots_url = "http://127.0.0.1:9".into();
        c.toolserver_url = "http://127.0.0.1:9".into();
    })
    .await;
    let r = w.get(&Browser::default(), "/readyz").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.body["checks"]["bot_runtime"], "down");
    assert_eq!(r.body["checks"]["tool_server"], "down");
    w.assert_no_tokens();
}

#[tokio::test]
async fn hsts_only_with_an_https_public_url() {
    let w = web_with(|c| c.public_url = Some("https://pmos.example.com".into())).await;
    let r = w.get(&Browser::default(), "/api/auth/providers").await;
    assert_eq!(r.headers["strict-transport-security"], "max-age=31536000");
    let w = web().await;
    let r = w.get(&Browser::default(), "/api/auth/providers").await;
    assert!(r.headers.get("strict-transport-security").is_none());
}

#[tokio::test]
async fn the_internal_route_can_have_its_own_listener() {
    let dir = tempfile::tempdir().unwrap();
    // A tool server that answers who is a user.
    let tools_up = Arc::new(Upstream::default());
    tools_up.directory.seed_team(TEAM);
    let (tools_url, _t) = serve(Router::new().fallback(toolserver).with_state(tools_up)).await;
    let mut cfg = config("http://127.0.0.1:9", &tools_url, dir.path());
    cfg.internal_bind = Some("127.0.0.1:0".parse().unwrap());
    let deps = Deps {
        brief: BriefConfig {
            enabled: true,
            token: Some(BRIEF_TOKEN.into()),
        },
        ..Deps::in_memory()
    };
    let server = pmos_web::app::build_server(cfg, deps).unwrap();
    let (public, _a) = serve(server.public).await;
    let (internal, _b) = serve(server.internal.expect("an internal router")).await;
    let http = reqwest::Client::new();
    let put = |base: &str| {
        http.put(format!("{base}/internal/brief/ana"))
            .header("authorization", format!("Bearer {BRIEF_TOKEN}"))
            .json(&json!({}))
            .send()
    };
    // Not on the browser-facing listener at all.
    let res = put(&public).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // On its own.
    let res = put(&internal).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // Which serves nothing else of the app.
    let res = http
        .get(format!("{internal}/api/auth/providers"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[test]
fn development_sign_in_never_builds_on_a_reachable_address() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let mut cfg = config("http://127.0.0.1:9", "http://127.0.0.1:9", dir.path());
    cfg.bind = "0.0.0.0:8770".parse().unwrap();
    let err = pmos_web::build(cfg, Deps::in_memory()).unwrap_err();
    assert!(err.contains("loopback"), "{err}");
}

/// Counts the purges it is asked for; otherwise memory.
#[derive(Default)]
struct CountingFollowUps {
    inner: MemoryFollowUpStore,
    purges: AtomicUsize,
}

#[async_trait::async_trait]
impl FollowUpStore for CountingFollowUps {
    async fn list(&self, handle: &str, now: DateTime<Utc>) -> Result<Vec<FollowUp>, StoreError> {
        self.inner.list(handle, now).await
    }
    async fn open(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<FollowUp>, StoreError> {
        self.inner.open(handle, id, now).await
    }
    async fn dismiss(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        self.inner.dismiss(handle, id, now).await
    }
    async fn replace(
        &self,
        handle: &str,
        date: chrono::NaiveDate,
        changed: Vec<Changed>,
        followups: Vec<NewFollowUp>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .replace(handle, date, changed, followups, now)
            .await
    }
    async fn day(&self, handle: &str, now: DateTime<Utc>) -> Result<Option<Day>, StoreError> {
        self.inner.day(handle, now).await
    }
    async fn delivered(&self, handle: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        self.inner.delivered(handle).await
    }
    async fn purge(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        self.purges.fetch_add(1, Ordering::SeqCst);
        self.inner.purge(now).await
    }
}

#[tokio::test]
async fn expired_things_are_purged_on_a_timer_until_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let cfg = config("http://127.0.0.1:9", "http://127.0.0.1:9", dir.path());
    let followups = Arc::new(CountingFollowUps::default());
    let deps = Deps {
        followups: followups.clone(),
        ..Deps::in_memory()
    };
    let server = pmos_web::app::build_server(cfg, deps).unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(
        server
            .housekeeping
            .run(std::time::Duration::from_millis(20), stop.clone()),
    );
    // Nobody signs in; the purge runs anyway, at once and then on each tick.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while followups.purges.load(Ordering::SeqCst) < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the purge never ran"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("it stops on shutdown")
        .unwrap();
}
