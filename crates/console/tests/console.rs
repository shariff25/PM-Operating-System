//! The console against a mock bot runtime and a mock tool server: each screen, escaping,
//! auth and visibility, routine actions and form protection.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pmos_console::config::{Config, Identity};
use pmos_console::upstream::{Credentials, Signer};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "console-client-token-0123456789abcdef";
/// The tool server's read-only `console` client token (`PMOS_CONSOLE_TS_TOKEN`).
const TOOLS_TOKEN: &str = "toolserver-console-token-0123456789ab";
/// What the sign-in proxy sends in `X-Pmos-Proxy-Secret`.
const PROXY_SECRET: &str = "proxy-secret-0123456789abcdef0123456";
const HOSTILE: &str = "<script>alert('x')</script>";

/// One seeded user: (handle, sign-in name, confirmed, status, roles).
type Seed = (
    &'static str,
    Option<&'static str>,
    bool,
    &'static str,
    &'static [&'static str],
);

/// One user as the mock keeps it, in the order of [`Seed`].
type Row = (String, Option<String>, bool, String, Vec<String>);

/// The user database as the mock tool server's `resolve` answers it.
const USERS: &[Seed] = &[
    ("ana", Some("ana@example.com"), true, "active", &["pm"]),
    ("ben", None, false, "active", &["pm"]),
    (
        "root",
        Some("root@example.com"),
        true,
        "active",
        &["pm", "admin"],
    ),
    // An Auditor only: no console.
    (
        "audrey",
        Some("audrey@example.com"),
        true,
        "active",
        &["auditor"],
    ),
    // A PM who is also an Auditor: a PM here, not an admin.
    (
        "dot",
        Some("dot@example.com"),
        true,
        "active",
        &["pm", "auditor"],
    ),
    // A Reviewer only.
    ("rae", None, false, "active", &["reviewer"]),
    // Joined through the Slack bot client: sign-in name not confirmed yet.
    (
        "newbie",
        Some("newbie@example.com"),
        false,
        "active",
        &["pm"],
    ),
    // Deactivated, with the roles kept.
    (
        "olga",
        Some("olga@example.com"),
        true,
        "deactivated",
        &["pm", "admin"],
    ),
];

/// A hostile `team.yaml`: `mallory` as an admin with Ana's address, and Ana made an admin.
/// The console reads no team file, so none of it counts.
const HOSTILE_TEAM: &str = "members:
  - {handle: mallory, slack-id: U0ANA, email: ana@example.com, roles: [pm, admin]}
  - {handle: ana, slack-id: U0ANA2, email: mallory@example.com, roles: [pm, admin]}
  - {handle: audrey, slack-id: U0AUD, email: audrey@example.com, roles: [pm, admin]}
  - {handle: olga, slack-id: U0OLGA, email: olga@example.com, roles: [pm, admin]}
";

/// The mock tool server's user database: the rows `resolve` answers from, and whether the
/// store is down (`503`).
struct Directory {
    users: Mutex<Vec<Row>>,
    down: std::sync::atomic::AtomicBool,
    seen: Seen,
}

impl Default for Directory {
    fn default() -> Self {
        Self {
            users: Mutex::new(
                USERS
                    .iter()
                    .map(|(h, n, c, s, r)| {
                        (
                            h.to_string(),
                            n.map(String::from),
                            *c,
                            s.to_string(),
                            r.iter().map(|r| r.to_string()).collect(),
                        )
                    })
                    .collect(),
            ),
            down: Default::default(),
            seen: Seen::default(),
        }
    }
}

impl Directory {
    fn set(&self, handle: &str, status: &str, roles: &[&str]) {
        let mut users = self.users.lock().unwrap();
        let row = users.iter_mut().find(|u| u.0 == handle).unwrap();
        row.3 = status.to_string();
        row.4 = roles.iter().map(|r| r.to_string()).collect();
    }

    fn down(&self, down: bool) {
        self.down.store(down, std::sync::atomic::Ordering::SeqCst);
    }

    /// The resolve calls so far: their queries.
    fn lookups(&self) -> Vec<String> {
        self.seen
            .calls()
            .into_iter()
            .map(|(_, uri, ..)| {
                uri.split_once('?')
                    .map(|q| q.1.to_string())
                    .unwrap_or_default()
            })
            .collect()
    }
}

async fn resolve(
    State(dir): State<Arc<Directory>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    dir.seen.record(&method, &uri, &headers, &[]);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if dir.down.load(std::sync::atomic::Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"code": "unavailable"})),
        )
            .into_response();
    }
    let users = dir.users.lock().unwrap();
    let found = match (
        q.get("by").map(String::as_str),
        q.get("handle"),
        q.get("name"),
    ) {
        (Some("handle"), Some(h), _) => users.iter().find(|u| &u.0 == h),
        (Some("sign_in_name"), _, Some(n)) => users
            .iter()
            .find(|u| u.1.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(n))),
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    match found {
        Some((handle, _, confirmed, status, roles)) => Json(json!({
            "handle": handle, "display_name": handle, "status": status, "roles": roles,
            "entra": "none", "sign_in_name_confirmed": confirmed, "dual_role": false,
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"code": "not_found"}))).into_response(),
    }
}

/// Every call an upstream saw: method, path and query, the bearer token, and the body.
/// Method, path and query, raw body, and the user assertion if any, of one call.
type Asserted = (String, String, Vec<u8>, Option<String>);

#[derive(Default)]
struct Seen {
    calls: Mutex<Vec<(String, String, String, String)>>,
    asserted: Mutex<Vec<Asserted>>,
}

impl Seen {
    fn record(&self, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        self.calls.lock().unwrap().push((
            method.to_string(),
            uri.to_string(),
            auth,
            String::from_utf8_lossy(body).into_owned(),
        ));
        self.asserted.lock().unwrap().push((
            method.to_string(),
            uri.to_string(),
            body.to_vec(),
            headers
                .get("x-pmos-assertion")
                .and_then(|v| v.to_str().ok())
                .map(String::from),
        ));
    }

    fn calls(&self) -> Vec<(String, String, String, String)> {
        self.calls.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<(String, String, String)> {
        self.calls()
            .into_iter()
            .filter(|(m, ..)| m != "GET")
            .map(|(m, p, _, b)| (m, p, b))
            .collect()
    }
}

fn routines_data() -> Vec<Value> {
    vec![
        json!({"id": "rtn_ana1", "bot": "pm-ana", "owner": "ana", "enabled": true,
               "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
               "prompt": "Summarize overnight #launch", "output": {"kind": "dm"},
               "approval": "approve", "next_run_at": "2026-11-16T08:00:00Z", "last_run_at": null}),
        json!({"id": "rtn_ben1", "bot": "pm-ben", "owner": "ben", "enabled": false,
               "schedule": {"cron": "0 17 * * 5", "tz": "America/New_York"},
               "prompt": "Weekly status for ben", "output": {"kind": "dm"},
               "approval": "approve", "next_run_at": "2026-11-20T22:00:00Z",
               "last_run_at": "2026-11-13T22:00:00Z"}),
    ]
}

fn task_json(id: &str, bot: &str, who: &str, text: &str) -> Value {
    json!({"id": id, "bot": bot, "requested_by": who, "source": "slack",
           "priority": "interactive", "text": text, "status": "done",
           "checklist": [{"item": "Search the docs", "done": true}],
           "result": format!("Answer with {HOSTILE}"), "error": null,
           "approvals": ["apr_1"], "slack": {"channel": "D0ANA", "thread_ts": "1731.000200"},
           "routine_id": null, "attempts": 1,
           "created_at": "2026-11-14T09:00:00Z", "updated_at": "2026-11-14T09:01:00Z"})
}

fn tasks_data() -> Vec<Value> {
    vec![
        task_json(
            "tsk_ana1",
            "docs",
            "ana",
            &format!("What ports does Acme Vault need? {HOSTILE}"),
        ),
        task_json("tsk_ben1", "pm-ben", "ben", "Ben's private question"),
    ]
}

async fn runtime(
    State(seen): State<Arc<Seen>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    seen.record(&method, &uri, &headers, &body);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {BOTS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let query: Vec<(String, String)> = uri
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let param = |k: &str| {
        query
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    let path: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
    match (method.as_str(), path.as_slice()) {
        ("GET", ["v1", "bots"]) => Json(json!([
            {"name": "docs", "role": "docs-helper", "scope": "team", "owner": "root",
             "description": format!("The Docs bot {HOSTILE}")},
            {"name": "pm-ana", "role": "pm-assistant", "scope": "personal", "owner": "ana",
             "description": "Ana's assistant"},
            {"name": "pm-ben", "role": "pm-assistant", "scope": "personal", "owner": "ben",
             "description": "Ben's assistant"},
        ]))
        .into_response(),
        ("GET", ["v1", "bots", "docs"]) => Json(json!({
            "name": "docs", "role": "docs-helper", "scope": "team", "owner": "root",
            "reads": {"jira_projects": ["Acme Vault"], "confluence-spaces": ["SUP"], "slack_channels": []},
            "tools": ["docs.search", "docs.get"], "actions": {}
        }))
        .into_response(),
        ("GET", ["v1", "bots", "pm-ana"]) => Json(json!({
            "name": "pm-ana", "role": {"name": "pm-assistant"}, "scope": "personal",
            "owner": "ana", "tools": ["jira.search", "confluence.create_draft"],
            "actions": {"confluence.create_draft": "approve", "slack.post_message": "never"}
        }))
        .into_response(),
        ("GET", ["v1", "bots", "pm-ben"]) => Json(json!({
            "name": "pm-ben", "role": "pm-assistant", "tools": ["jira.create_issue"],
            "actions": [{"tool": "jira.create_issue", "setting": "approve"}]
        }))
        .into_response(),
        ("GET", ["v1", "tasks"]) => {
            let mut tasks = tasks_data();
            match param("requested_by") {
                Some(who) => tasks.retain(|t| t["requested_by"] == json!(who)),
                // An admin's view of everyone (root's, who has no tasks of their own): as the
                // runtime answers it, metadata only (R1).
                None => {
                    for t in &mut tasks {
                        t["text"] = json!("");
                        t["result"] = Value::Null;
                        t["checklist"] = json!([]);
                        t["hidden"] = json!(true);
                    }
                }
            }
            Json(json!({"items": tasks})).into_response()
        }
        ("GET", ["v1", "tasks", id]) => match tasks_data().into_iter().find(|t| t["id"] == json!(id)) {
            Some(t) => Json(t).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        ("GET", ["v1", "tasks", _, "events"]) => Json(json!([
            {"at": "2026-11-14T09:00:01Z", "kind": "status", "text": "Working on it"},
            {"at": "2026-11-14T09:00:02Z", "kind": "milestone", "text": format!("Searching {HOSTILE}")},
            {"at": "2026-11-14T09:00:03Z", "kind": "checklist",
             "checklist": [{"item": "Search", "done": true}, {"item": "Answer", "done": false}]},
        ]))
        .into_response(),
        ("GET", ["v1", "tasks", _, "trace"]) => Json(json!([
            {"at": "2026-11-14T09:00:01Z", "kind": "prompt", "body": {"messages": [{"role": "user", "content": HOSTILE}]}},
            {"at": "2026-11-14T09:00:02Z", "kind": "tool_call", "body": {"tool": "docs.search", "args": {"query": "Acme Vault ports"}}},
            {"at": "2026-11-14T09:00:03Z", "kind": "model_reply", "body": "Port 443 </pre><b>bold</b>"},
        ]))
        .into_response(),
        ("GET", ["v1", "routines"]) => {
            let mut items = routines_data();
            match param("owner") {
                Some(owner) => items.retain(|r| r["owner"] == json!(owner)),
                // The admin's view: no prompt of anyone else's (R1).
                None => {
                    for r in &mut items {
                        r["prompt"] = json!("");
                        r["hidden"] = json!(true);
                    }
                }
            }
            Json(items).into_response()
        }
        ("PATCH", ["v1", "routines", id]) => {
            let mut r = routines_data()
                .into_iter()
                .find(|r| r["id"] == json!(id))
                .unwrap();
            let patch: Value = serde_json::from_slice(&body).unwrap();
            r["enabled"] = patch["enabled"].clone();
            Json(r).into_response()
        }
        ("DELETE", ["v1", "routines", _]) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn toolserver(
    State(seen): State<Arc<Seen>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    seen.record(&method, &uri, &headers, &[]);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let user = q.get("user").cloned().unwrap_or_default();
    let items: Vec<Value> = match user.as_str() {
        "ana" => vec![json!({
            "id": "apr_1", "user": "ana", "skill": "prd-draft", "run_id": "tsk_ana1",
            "tool": "confluence.create_draft",
            "args": {"title": format!("Launch plan {HOSTILE}"), "space": "SUP"},
            "args_hash": "abc", "status": "pending", "created_at": "2026-11-14T09:02:00Z"})],
        "ben" => vec![json!({
            "id": "apr_2", "user": "ben", "skill": "ask", "run_id": "tsk_ben1",
            "tool": "slack.post_message", "args": {"channel": "C01", "text": "ben's draft"},
            "args_hash": "def", "status": "pending", "created_at": "2026-11-14T10:00:00Z"})],
        _ => vec![],
    };
    // Ben's inbox is past the tool server's safety cap (R12).
    let truncated = user == "ben";
    let total_pending = if truncated { 1200 } else { items.len() };
    Json(json!({ "items": items, "truncated": truncated, "total_pending": total_pending }))
        .into_response()
}

/// The tool server's admin listing for the console (`GET /internal/console/members`): handles
/// only, for an admin (`root` here). `cy` has no personal bot and no recent task.
async fn members(
    State(seen): State<Arc<Seen>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    seen.record(&method, &uri, &headers, &[]);
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if q.get("user").map(String::as_str) != Some("root") {
        return (StatusCode::FORBIDDEN, Json(json!({"code": "forbidden"}))).into_response();
    }
    Json(json!({"handles": ["ana", "ben", "cy", "root"]})).into_response()
}

/// A URL nothing listens on.
async fn closed_port() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
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

struct Console {
    base: String,
    /// The proxy's secret, sent on every request when the console trusts the proxy.
    secret: Option<String>,
    runtime: Arc<Seen>,
    tools: Arc<Seen>,
    directory: Arc<Directory>,
    /// The mock runtime's and tool server's URLs.
    bots_url: String,
    tools_url: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

struct Options {
    identity: Identity,
    runtime_up: bool,
    audit: bool,
    slack: bool,
    /// Keys for both upstreams, as `PMOS_ASSERTION_KEYS` would give them.
    sign: bool,
    /// The resolve client's clock; the system clock when `None`.
    clock: Option<pmos_directory::resolve::Clock>,
    /// The tool server's `resolve` can't be reached at all (a closed port).
    directory_unreachable: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            identity: Identity::Proxy {
                secret: PROXY_SECRET.into(),
            },
            runtime_up: true,
            audit: true,
            slack: true,
            sign: false,
            clock: None,
            directory_unreachable: false,
        }
    }
}

/// A clock tests move by hand.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<chrono::DateTime<chrono::Utc>>>);

impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(
            chrono::DateTime::parse_from_rfc3339("2026-11-18T09:00:00Z")
                .unwrap()
                .into(),
        )))
    }

    fn clock(&self) -> pmos_directory::resolve::Clock {
        let now = self.0.clone();
        Arc::new(move || *now.lock().unwrap())
    }

    fn advance(&self, secs: i64) {
        *self.0.lock().unwrap() += chrono::Duration::seconds(secs);
    }
}

async fn console(opts: Options) -> Console {
    let runtime_seen = Arc::new(Seen::default());
    let tools_seen = Arc::new(Seen::default());
    let directory = Arc::new(Directory::default());
    let (bots_url, stop_bots) = serve(
        Router::new()
            .fallback(runtime)
            .with_state(runtime_seen.clone()),
    )
    .await;
    let (tools_url, stop_tools) = serve(
        Router::new()
            .route("/internal/approvals", get(toolserver))
            .route("/internal/console/members", get(members))
            .with_state(tools_seen.clone())
            .merge(
                Router::new()
                    .route("/internal/users/resolve", get(resolve))
                    .with_state(directory.clone()),
            ),
    )
    .await;
    if !opts.runtime_up {
        stop_bots.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    {
        let mut f = std::fs::File::create(&audit_path).unwrap();
        writeln!(
            f,
            "{}",
            json!({"at": "2026-11-14T09:00:02Z", "client": "mcp", "run_id": "tsk_ana1",
                   "user": "ana", "skill": "docs", "tool": "docs.search", "outcome": "ok",
                   "sources": ["docs.example.com/vault/ports"], "detail": HOSTILE})
        )
        .unwrap();
        writeln!(
            f,
            "{}",
            json!({"at": "2026-11-14T09:00:05Z", "client": "mcp", "run_id": "tsk_ben1",
                   "user": "ben", "skill": "ask", "tool": "slack.search", "outcome": "denied"})
        )
        .unwrap();
    }
    let secret = match &opts.identity {
        Identity::Proxy { secret } => Some(secret.clone()),
        Identity::Dev(_) => None,
    };
    let (mut bots_token, mut toolserver_token): (Credentials, Credentials) =
        (BOTS_TOKEN.into(), TOOLS_TOKEN.into());
    if opts.sign {
        bots_token = bots_token.with_signer(Signer::new(
            "console",
            "bots",
            ring("cb1", CONSOLE_BOTS_KEY),
        ));
        toolserver_token = toolserver_token.with_signer(Signer::new(
            "console",
            "toolserver",
            ring("ct1", CONSOLE_TOOLS_KEY),
        ));
    }
    let resolve_url = if opts.directory_unreachable {
        closed_port().await
    } else {
        tools_url.clone()
    };
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        identity: opts.identity,
        bots_url: bots_url.clone(),
        bots_token,
        toolserver_url: tools_url.clone(),
        toolserver_token,
        audit_log: opts.audit.then_some(audit_path),
        slack_url: opts.slack.then(|| "https://acme.slack.com".to_string()),
        public_url: None,
    };
    let transport = pmos_directory::resolve::HttpTransport::new(
        reqwest::Client::new(),
        &resolve_url,
        TOOLS_TOKEN,
    );
    let clock = opts.clock.unwrap_or_else(|| Arc::new(chrono::Utc::now));
    let resolver = pmos_directory::resolve::ResolveClient::new(Arc::new(transport), clock);
    let (base, stop_console) = serve(pmos_console::app::router_with_directory(
        config,
        Arc::new(resolver),
    ))
    .await;
    Console {
        base,
        secret,
        runtime: runtime_seen,
        tools: tools_seen,
        directory,
        bots_url,
        tools_url,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        _dir: dir,
        _stop: vec![stop_bots, stop_tools, stop_console],
    }
}

impl Console {
    /// A request as the sign-in proxy sends it: with its secret, and no user yet.
    fn raw(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(s) = &self.secret {
            req = req.header("x-pmos-proxy-secret", s);
        }
        req
    }

    async fn get(&self, user: Option<&str>, path: &str) -> (StatusCode, String) {
        let mut req = self.raw(Method::GET, path);
        if let Some(u) = user {
            req = req.header("x-forwarded-user", u);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        (status, res.text().await.unwrap())
    }

    async fn post(
        &self,
        user: &str,
        path: &str,
        csrf: &str,
        origin: Option<&str>,
    ) -> reqwest::Response {
        let mut req = self
            .raw(Method::POST, path)
            .header("x-forwarded-user", user)
            .form(&[("csrf", csrf)]);
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        req.send().await.unwrap()
    }

    /// The form token on the Routines page for `user`.
    async fn csrf(&self, user: &str) -> String {
        let (_, body) = self.get(Some(user), "/routines").await;
        let marker = "name=\"csrf\" value=\"";
        let start = body.find(marker).expect("a form on the page") + marker.len();
        body[start..start + 64].to_string()
    }
}

fn assert_escaped(body: &str) {
    assert!(!body.contains("<script>"), "raw script tag in:\n{body}");
    assert!(!body.contains("<b>bold</b>"), "raw markup in:\n{body}");
}

#[tokio::test]
async fn auth_needs_a_user() {
    let c = console(Options::default()).await;
    // No user at all.
    let (status, body) = c.get(None, "/bots").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("Not signed in"));
    // A hostile name that is neither a handle nor a sign-in name: refused without a lookup,
    // and never echoed.
    let (status, body) = c.get(Some("mallory<script>"), "/activity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!body.contains("mallory"));
    assert_escaped(&body);
    assert!(c.directory.lookups().is_empty(), "junk costs no miss");
    // Someone the user database doesn't know.
    let (status, body) = c.get(Some("mallory"), "/activity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("Not a PM OS user"), "{body}");
    // Refused requests never reach an upstream.
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());
    // A user, by handle or by sign-in name.
    assert_eq!(c.get(Some("ana"), "/bots").await.0, StatusCode::OK);
    let (status, body) = c.get(Some("ROOT@example.com"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("root · admin"));
    // Liveness needs nobody.
    assert_eq!(c.get(None, "/healthz").await, (StatusCode::OK, "ok".into()));
    assert!(c
        .runtime
        .calls()
        .iter()
        .all(|(_, _, auth, _)| auth == &format!("Bearer {BOTS_TOKEN}")));
}

/// Everything any test logs, at every level: the log-secrecy test reads it.
fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || Sink(sink.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
        logs
    })
    .clone()
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A request straight to the console's port, not through the proxy: no secret, or a wrong one.
async fn bypass(c: &Console, secrets: &[&str], user: &str, path: &str) -> (StatusCode, String) {
    let mut req = reqwest::Client::new()
        .get(format!("{}{path}", c.base))
        .header("x-forwarded-user", user);
    for secret in secrets {
        req = req.header("x-pmos-proxy-secret", *secret);
    }
    let res = req.send().await.unwrap();
    let status = res.status();
    (status, res.text().await.unwrap())
}

#[tokio::test]
async fn without_the_proxy_secret_the_forwarded_user_is_ignored() {
    let logs = captured_logs();
    let c = console(Options::default()).await;
    let wrong = "not-the-proxy-secret-0123456789abcdef";
    let short = &PROXY_SECRET[..PROXY_SECRET.len() - 1];
    let longer = format!("{PROXY_SECRET}0");
    // A member, an admin, and a name only an attacker would send.
    for user in ["ana", "root", "bypass-mallory-7f3a"] {
        for secrets in [
            vec![],
            vec![""],
            vec![wrong],
            vec![short],
            vec![longer.as_str()],
            // A client's own copy next to the real one.
            vec![wrong, PROXY_SECRET],
            vec![PROXY_SECRET, PROXY_SECRET],
        ] {
            for path in ["/bots", "/routines", "/activity", "/approvals"] {
                let (status, body) = bypass(&c, &secrets, user, path).await;
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{user} {secrets:?} {path}"
                );
                assert!(body.contains("Not signed in"), "{body}");
                // Nobody is named on the page: the user header was never read.
                assert!(!body.contains("class=\"who\""), "{body}");
                assert!(!body.contains("bypass-mallory"), "{body}");
            }
        }
    }
    // A form post without the secret changes nothing, even with a good form token.
    let ana = c.csrf("ana").await;
    let res = reqwest::Client::new()
        .post(format!("{}/routines/rtn_ana1/pause", c.base))
        .header("x-forwarded-user", "ana")
        .form(&[("csrf", ana.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // Refused requests never reach an upstream (the csrf fetch above read /routines).
    assert!(c.runtime.writes().is_empty());
    assert!(c.tools.calls().is_empty());
    // With the secret, everything works as before.
    let (status, body) = bypass(&c, &[PROXY_SECRET], "root", "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("root · admin"));
    assert_eq!(
        bypass(&c, &[PROXY_SECRET], "mallory", "/bots").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bypass(&c, &[PROXY_SECRET], "", "/bots").await.0,
        StatusCode::UNAUTHORIZED
    );
    // Liveness needs nobody.
    assert_eq!(bypass(&c, &[], "", "/healthz").await.0, StatusCode::OK);

    // Sign-in names, for the log check below: looked up, found or refused.
    for user in [
        "ana@example.com",
        "audrey@example.com",
        "olga@example.com",
        "newbie@example.com",
        "nobody-7f3a@example.com",
    ] {
        c.get(Some(user), "/bots").await;
    }

    // Nothing logged, by any test so far, holds the secret, a wrong secret, the user named by
    // a request that failed the check, a token, or a sign-in name.
    tracing::info!("log capture is working");
    let logs = String::from_utf8_lossy(&logs.lock().unwrap()).to_string();
    assert!(logs.contains("log capture is working"), "{logs}");
    assert!(logs.contains("no valid X-Pmos-Proxy-Secret"), "{logs}");
    assert!(logs.contains("refused a console viewer"), "{logs}");
    for needle in [
        PROXY_SECRET,
        wrong,
        short,
        "bypass-mallory-7f3a",
        TOOLS_TOKEN,
        BOTS_TOKEN,
        "@example.com",
        "nobody-7f3a",
    ] {
        assert!(!logs.contains(needle), "logs contain {needle}:\n{logs}");
    }
}

/// W8: the proxy secret is checked on every route and method but `GET /healthz`, including
/// the task page, form posts and paths no route has; and nothing near `/healthz` slips past.
#[tokio::test]
async fn every_route_but_healthz_needs_the_proxy_secret() {
    let c = console(Options::default()).await;
    let ana = c.csrf("ana").await;
    c.runtime.calls.lock().unwrap().clear();
    for (method, path) in [
        (Method::GET, "/"),
        (Method::GET, "/bots"),
        (Method::GET, "/routines"),
        (Method::GET, "/approvals"),
        (Method::GET, "/activity"),
        (Method::GET, "/tasks/tsk_1"),
        (Method::GET, "/no-such-page"),
        (Method::GET, "/healthz/"),
        (Method::GET, "//healthz"),
        (Method::GET, "/healthz/../bots"),
        (Method::POST, "/routines/rtn_ana1/pause"),
        (Method::POST, "/healthz"),
        (Method::PUT, "/bots"),
        (Method::DELETE, "/routines/rtn_ana1/delete"),
    ] {
        let res = reqwest::Client::new()
            .request(method.clone(), format!("{}{path}", c.base))
            .header("x-forwarded-user", "root")
            .header("origin", c.base.as_str())
            .form(&[("csrf", ana.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    assert!(
        c.runtime.calls().is_empty(),
        "no refused request reached the runtime"
    );
    assert!(c.tools.calls().is_empty());
    let res = reqwest::Client::new()
        .get(format!("{}/healthz", c.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_forwarded_user_needs_the_proxy_secret() {
    let c = console(Options::default()).await;
    let ask = |secret: Option<&str>, path: &str| {
        let mut req = c
            .http
            .get(format!("{}{path}", c.base))
            .header("x-forwarded-user", "root");
        if let Some(s) = secret {
            req = req.header("x-pmos-proxy-secret", s);
        }
        req.send()
    };
    // Without the secret, or with a wrong one, nobody is signed in, whatever the user header
    // says, on every page, and no upstream is called.
    for secret in [None, Some(""), Some("proxy-secret-0123456789abcdef0123457")] {
        for path in ["/bots", "/approvals", "/", "/no-such-page"] {
            let res = ask(secret, path).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{path} {secret:?}");
            assert!(res.text().await.unwrap().contains("Not signed in"));
        }
    }
    let res = c
        .http
        .post(format!("{}/routines/rtn_ana1/pause", c.base))
        .header("x-forwarded-user", "ana")
        .header("origin", c.base.as_str())
        .form(&[("csrf", "x")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());
    // The container's health check needs no secret.
    let res = ask(None, "/healthz").await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // With it, the user header counts.
    let res = ask(Some(PROXY_SECRET), "/bots").await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_forwarded_header_is_ignored_without_trust_proxy() {
    let c = console(Options {
        identity: Identity::Dev("ben".into()),
        ..Options::default()
    })
    .await;
    // The header says root (an admin); the console only believes the dev user.
    let (status, body) = c.get(Some("root"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<p class=\"who\">ben</p>"));
    assert!(!body.contains("pm-ana"));

    let c = console(Options {
        identity: Identity::Dev("nobody".into()),
        ..Options::default()
    })
    .await;
    assert_eq!(c.get(Some("ana"), "/bots").await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bots_screen_shows_the_roster_with_scope_and_settings() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("root"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    for want in [
        "id=\"bot-docs\"",
        "id=\"bot-pm-ana\"",
        "id=\"bot-pm-ben\"",
        "docs-helper",
        "Jira projects: <code>Acme Vault</code>",
        "Confluence spaces: <code>SUP</code>",
        "Slack channels: <span class=\"muted\">none</span>",
        "As its owner, with their linked accounts",
        "<code>confluence.create_draft</code> <span class=\"badge wait\">approve</span>",
        "<code>slack.post_message</code> <span class=\"badge bad\">never</span>",
        "<code>jira.create_issue</code> <span class=\"badge wait\">approve</span>",
        "The Docs bot &lt;script&gt;",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert_escaped(&body);
    // The team bot comes first.
    assert!(body.find("bot-docs").unwrap() < body.find("bot-pm-ana").unwrap());
    // Every runtime call used the console's own token.
    let calls = c.runtime.calls();
    assert!(calls.iter().any(|(_, p, ..)| p == "/v1/bots/pm-ben"));
    assert!(calls
        .iter()
        .all(|(_, _, auth, _)| auth == &format!("Bearer {BOTS_TOKEN}")));
}

#[tokio::test]
async fn a_pm_sees_their_own_bots_and_the_team_bots() {
    let c = console(Options::default()).await;
    let (_, body) = c.get(Some("ana"), "/bots").await;
    assert!(body.contains("bot-docs") && body.contains("bot-pm-ana"));
    assert!(!body.contains("bot-pm-ben") && !body.contains("Ben&#39;s assistant"));
    // Ben's bot wasn't even fetched for Ana.
    assert!(!c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p == "/v1/bots/pm-ben"));
}

#[tokio::test]
async fn routines_screen_lists_next_runs_by_who_may_see_them() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("root"), "/routines").await;
    assert_eq!(status, StatusCode::OK);
    for want in [
        "<code>0 8 * * 1-5</code> Europe/London",
        "2026-11-16 08:00 UTC",
        "<span class=\"badge wait\">paused</span>",
        "action=\"/routines/rtn_ana1/pause\"",
        "action=\"/routines/rtn_ben1/resume\"",
        "action=\"/routines/rtn_ben1/delete\"",
        // An admin gets everyone's routines without their prompts (R1).
        "<span class=\"muted\">text hidden</span>",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert!(!body.contains("Summarize overnight #launch"));
    assert!(!body.contains("Weekly status for ben"));
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(m, p, ..)| m == "GET" && p == "/v1/routines"));

    let (_, body) = c.get(Some("ana"), "/routines").await;
    assert!(body.contains("rtn_ana1") && !body.contains("rtn_ben1"));
    assert!(
        body.contains("Summarize overnight #launch"),
        "her own prompt"
    );
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p == "/v1/routines?owner=ana"));
}

#[tokio::test]
async fn routine_actions_call_the_runtime() {
    let c = console(Options::default()).await;
    let token = c.csrf("ana").await;
    let origin = Some(c.base.as_str());

    let res = c
        .post("ana", "/routines/rtn_ana1/pause", &token, origin)
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()["location"], "/routines?done=pause");
    let res = c
        .post("ana", "/routines/rtn_ana1/resume", &token, origin)
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let res = c
        .post("ana", "/routines/rtn_ana1/delete", &token, origin)
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        c.runtime.writes(),
        vec![
            (
                "PATCH".into(),
                "/v1/routines/rtn_ana1".into(),
                r#"{"enabled":false}"#.into()
            ),
            (
                "PATCH".into(),
                "/v1/routines/rtn_ana1".into(),
                r#"{"enabled":true}"#.into()
            ),
            (
                "DELETE".into(),
                "/v1/routines/rtn_ana1".into(),
                String::new()
            ),
        ]
    );
    let (_, body) = c.get(Some("ana"), "/routines?done=delete").await;
    assert!(body.contains("Routine deleted."));

    // An admin may act on anyone's routine.
    let admin = c.csrf("root").await;
    let res = c
        .post("root", "/routines/rtn_ben1/resume", &admin, origin)
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(c.runtime.writes().len(), 4);
}

#[tokio::test]
async fn a_pm_cannot_touch_someone_elses_routine() {
    let c = console(Options::default()).await;
    let token = c.csrf("ana").await;
    let res = c
        .post("ana", "/routines/rtn_ben1/delete", &token, Some(&c.base))
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = c
        .post("ana", "/routines/rtn_ana1/explode", &token, Some(&c.base))
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(c.runtime.writes().is_empty());
}

#[tokio::test]
async fn forms_without_a_valid_token_or_from_elsewhere_are_refused() {
    let c = console(Options::default()).await;
    let ana = c.csrf("ana").await;
    let ben = c.csrf("ben").await;
    let path = "/routines/rtn_ana1/pause";
    // No token, a wrong one, and someone else's.
    for token in ["", "0".repeat(64).as_str(), ben.as_str()] {
        let res = c.post("ana", path, token, Some(&c.base)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "token {token:?}");
    }
    // The right token from another site.
    let res = c
        .post("ana", path, &ana, Some("https://evil.example"))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // The right token with neither Origin nor Referer.
    let res = c.post("ana", path, &ana, None).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "no Origin or Referer");
    // A Referer from the console itself is enough.
    let res = c
        .raw(Method::POST, path)
        .header("x-forwarded-user", "ana")
        .header("referer", format!("{}/routines", c.base))
        .form(&[("csrf", ana.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER, "a same-origin Referer");
    assert_eq!(c.runtime.writes().len(), 1);
    c.runtime.calls.lock().unwrap().clear();
    let res = c
        .raw(Method::POST, path)
        .header("x-forwarded-user", "ana")
        .header("sec-fetch-site", "cross-site")
        .form(&[("csrf", ana.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // No body at all.
    let res = c
        .raw(Method::POST, path)
        .header("x-forwarded-user", "ana")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // A GET never changes anything.
    assert_eq!(
        c.get(Some("ana"), path).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert!(c.runtime.writes().is_empty());
}

#[tokio::test]
async fn approvals_screen_is_read_only_and_scoped() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("root"), "/approvals").await;
    assert_eq!(status, StatusCode::OK);
    for want in [
        "Approvals are decided in Slack",
        "href=\"https://acme.slack.com\"",
        "<code>confluence.create_draft</code>",
        "<code>slack.post_message</code>",
        "Launch plan &lt;script&gt;",
        "href=\"/tasks/tsk_ana1\"",
        "prd-draft",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert_escaped(&body);
    assert!(!body.contains("<form"), "the queue has no actions");
    // An inbox the tool server cut at its safety cap says so (R12).
    assert!(
        body.contains("ben's inbox has 1200 pending drafts"),
        "{body}"
    );
    // Newest first.
    assert!(
        body.find("slack.post_message").unwrap() < body.find("confluence.create_draft").unwrap()
    );
    // Every member's inbox, pending only, with the service token: the members come from the
    // tool server's admin listing, so `cy` (no personal bot, no recent task) is read too.
    let calls = c.tools.calls();
    assert!(calls
        .iter()
        .any(|(_, p, ..)| p == "/internal/console/members?user=root"));
    for user in ["ana", "ben", "cy", "root"] {
        let want = format!("/internal/approvals?user={user}&status=pending");
        assert!(calls.iter().any(|(_, p, ..)| p == &want), "missing {want}");
    }
    assert!(calls
        .iter()
        .all(|(_, _, auth, _)| auth == &format!("Bearer {TOOLS_TOKEN}")));

    // A PM's queue asks for their own inbox only.
    let before = calls.len();
    let (_, body) = c.get(Some("ana"), "/approvals").await;
    assert!(body.contains("confluence.create_draft"));
    assert!(!body.contains("slack.post_message"));
    let asked: Vec<String> = c.tools.calls()[before..]
        .iter()
        .map(|(_, p, ..)| p.clone())
        .collect();
    assert_eq!(asked, vec!["/internal/approvals?user=ana&status=pending"]);
}

#[tokio::test]
async fn activity_lists_recent_tasks_by_who_may_see_them() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("root"), "/activity").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("href=\"/tasks/tsk_ana1\""));
    assert!(body.contains("href=\"/tasks/tsk_ben1\""));
    // An admin's list of everyone's tasks is metadata: the asks read "text hidden" (R1).
    assert!(!body.contains("What ports does Acme Vault need?"));
    assert!(!body.contains("Ben's private question"));
    assert_eq!(body.matches("text hidden").count(), 2, "{body}");
    assert_escaped(&body);
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p == "/v1/tasks?limit=50"));

    let (_, body) = c.get(Some("ana"), "/activity").await;
    assert!(body.contains("tsk_ana1") && !body.contains("tsk_ben1"));
    assert!(body.contains("What ports does Acme Vault need? &lt;script&gt;"));
    assert!(!body.contains("text hidden"));
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p == "/v1/tasks?limit=50&requested_by=ana"));
}

#[tokio::test]
async fn a_task_page_shows_events_trace_and_audit_escaped() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("ana"), "/tasks/tsk_ana1").await;
    assert_eq!(status, StatusCode::OK);
    for want in [
        // Events.
        "Working on it",
        "Searching &lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;",
        "(1/2 done)",
        // The trace, every entry.
        "&quot;content&quot;: &quot;&lt;script&gt;",
        "&quot;query&quot;: &quot;Acme Vault ports&quot;",
        "Port 443 &lt;/pre&gt;&lt;b&gt;bold&lt;/b&gt;",
        // Audit events for this run only.
        "<code>docs.search</code>",
        "docs.example.com/vault/ports",
        // The task itself.
        "Answer with &lt;script&gt;",
        "✓ Search the docs",
        "<code>apr_1</code>",
        "href=\"https://acme.slack.com/archives/D0ANA/p1731000200\"",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert!(!body.contains("slack.search"), "another run's audit event");
    assert_escaped(&body);
    let calls = c.runtime.calls();
    for path in ["/v1/tasks/tsk_ana1/events", "/v1/tasks/tsk_ana1/trace"] {
        assert!(calls.iter().any(|(_, p, ..)| p == path), "missing {path}");
    }
}

#[tokio::test]
async fn a_pm_cannot_open_someone_elses_task() {
    let c = console(Options::default()).await;
    let (status, body) = c.get(Some("ana"), "/tasks/tsk_ben1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("private question"));
    // The trace was never fetched.
    assert!(!c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p.starts_with("/v1/tasks/tsk_ben1/")));
    assert_eq!(
        c.get(Some("root"), "/tasks/tsk_ben1").await.0,
        StatusCode::OK
    );
    assert_eq!(
        c.get(Some("ana"), "/tasks/tsk_nope").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get(Some("ana"), "/tasks/..%2Fbots").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn without_an_audit_log_the_task_page_says_so() {
    let c = console(Options {
        audit: false,
        slack: false,
        ..Options::default()
    })
    .await;
    let (status, body) = c.get(Some("ana"), "/tasks/tsk_ana1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("PMOS_CONSOLE_AUDIT_LOG"));
    assert!(!body.contains("acme.slack.com"));
}

#[tokio::test]
async fn a_runtime_outage_is_a_readable_page() {
    let c = console(Options {
        runtime_up: false,
        ..Options::default()
    })
    .await;
    let (status, body) = c.get(Some("ana"), "/activity").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("The bot runtime didn&#39;t answer"));
    // The approvals queue comes from the tool server, which is still up.
    assert_eq!(c.get(Some("ana"), "/approvals").await.0, StatusCode::OK);
}

#[tokio::test]
async fn pages_carry_a_strict_policy_and_both_themes() {
    let c = console(Options::default()).await;
    let res = c
        .raw(Method::GET, "/bots")
        .header("x-forwarded-user", "ana")
        .send()
        .await
        .unwrap();
    let csp = res.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"));
    assert_eq!(res.headers()["cache-control"], "no-store");
    let body = res.text().await.unwrap();
    assert!(body.contains("prefers-color-scheme:dark"));
    assert!(body.contains("max-width:640px"));
    assert!(body.contains("name=\"viewport\""));
    assert!(!body.contains("<script"));
    let res = c.raw(Method::GET, "/").send().await.unwrap();
    assert_eq!(res.headers()["location"], "/bots");
}

// User assertions (docs/user-assertion.md).

// Test keys only: fixed bytes, never used anywhere real.
const CONSOLE_BOTS_KEY: &str = "Y29uc29sZS1ib3RzLXRlc3Qta2V5LTAxMjM0NTY3ODk=";
const CONSOLE_TOOLS_KEY: &str = "Y29uc29sZS10b29sc2VydmVyLXRlc3Qta2V5LTAxMjM=";

fn ring(id: &str, key: &str) -> pmos_assertion::KeyRing {
    pmos_assertion::KeyRing::new(pmos_assertion::Key::from_base64(id, key).unwrap())
}

/// The person a console call names (`requested_by`, `owner` or `user`), if any.
fn named(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| matches!(*k, "requested_by" | "owner" | "user"))
        .map(|(_, v)| v.to_string())
}

/// Is this a routine change (pause, resume, delete)? It names no one, and is signed for the
/// viewer who made it.
fn routine_change(method: &str, uri: &str) -> bool {
    (matches!(method, "PATCH" | "DELETE") && uri.starts_with("/v1/routines/"))
        || one_task_read(method, uri)
}

/// Is this one task's detail, events or trace? It names no one in its query, and is signed
/// for the viewer (under `require` the runtime answers only the task's PM or an admin).
fn one_task_read(method: &str, uri: &str) -> bool {
    method == "GET"
        && uri
            .strip_prefix("/v1/tasks/")
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('?'))
}

/// Issued by `console` for `audience`: every runtime call carries a valid assertion for the
/// viewer, `actor` (R1: the runtime decides what the viewer sees from it); every tool-server
/// call that names a person carries one for exactly that person, and every other carries none.
fn check_calls(seen: &Seen, audience: &str, key: (&str, &str), actor: Option<&str>) -> usize {
    let cache = pmos_assertion::MemoryReplayCache::default();
    let policy = pmos_assertion::VerifyPolicy::new(audience, &["console"]).unwrap();
    let mut signed = 0;
    for (method, uri, body, assertion) in seen.asserted.lock().unwrap().iter() {
        let expected = if audience == "bots" || routine_change(method, uri) {
            Some(actor.expect("a runtime call needs the viewer").to_string())
        } else {
            named(uri)
        };
        match expected {
            None => assert!(assertion.is_none(), "{method} {uri} names nobody"),
            Some(person) => {
                let header = assertion
                    .as_deref()
                    .unwrap_or_else(|| panic!("{uri} unsigned"));
                let binding = pmos_assertion::RequestBinding::new(method, uri, body).unwrap();
                let a = ring(key.0, key.1)
                    .verify(
                        header,
                        &policy,
                        pmos_assertion::unix_now(),
                        Some(&binding),
                        &cache,
                    )
                    .unwrap_or_else(|e| panic!("{method} {uri}: {e}"));
                assert_eq!(a.subject(), person);
                assert!(a.expires_at() - a.issued_at() <= 60);
                signed += 1;
            }
        }
    }
    signed
}

#[tokio::test]
async fn calls_that_name_a_person_carry_a_valid_assertion_for_them() {
    let c = console(Options {
        sign: true,
        ..Options::default()
    })
    .await;
    for user in ["ana", "root"] {
        c.runtime.asserted.lock().unwrap().clear();
        for page in ["/routines", "/activity", "/approvals", "/bots"] {
            assert_eq!(
                c.get(Some(user), page).await.0,
                StatusCode::OK,
                "{user} {page}"
            );
        }
        // Every runtime call is signed for the viewer: the bots, tasks and routines lists.
        assert!(check_calls(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY), Some(user)) >= 4);
    }
    c.runtime.asserted.lock().unwrap().clear();
    let token = c.csrf("ana").await;
    let res = c
        .post(
            "ana",
            "/routines/rtn_ana1/pause",
            &token,
            Some(c.base.as_str()),
        )
        .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    // Ana's routine check and pause, and every member's inbox for root.
    assert!(check_calls(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY), Some("ana")) >= 2);
    assert!(check_calls(&c.tools, "toolserver", ("ct1", CONSOLE_TOOLS_KEY), None) >= 4);
}

/// The task page reads one task's detail, events and trace signed for the viewer, who is the
/// task's PM or an admin (the runtime checks again under `require`).
#[tokio::test]
async fn task_pages_are_read_signed_for_the_viewer() {
    let c = console(Options {
        sign: true,
        ..Options::default()
    })
    .await;
    for viewer in ["ana", "root"] {
        c.runtime.asserted.lock().unwrap().clear();
        let (status, _) = c.get(Some(viewer), "/tasks/tsk_ana1").await;
        assert_eq!(status, StatusCode::OK, "{viewer}");
        let reads = c
            .runtime
            .asserted
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, u, ..)| one_task_read(m, u))
            .count();
        assert_eq!(reads, 3, "detail, events and trace");
        assert_eq!(
            check_calls(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY), Some(viewer)),
            3
        );
    }
}

#[tokio::test]
async fn routine_changes_are_signed_for_the_viewer_who_made_them() {
    let c = console(Options {
        sign: true,
        ..Options::default()
    })
    .await;
    let origin = Some(c.base.as_str());
    // An admin resumes and deletes someone else's routine: signed as the admin, not the owner.
    let token = c.csrf("root").await;
    for action in ["resume", "delete"] {
        let res = c
            .post(
                "root",
                &format!("/routines/rtn_ben1/{action}"),
                &token,
                origin,
            )
            .await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER, "{action}");
    }
    let changes: Vec<_> = c
        .runtime
        .asserted
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, u, ..)| routine_change(m, u))
        .cloned()
        .collect();
    assert_eq!(changes.len(), 2, "{changes:?}");
    // Every runtime call, the changes and the routine reads before them, is signed as root.
    let calls = c.runtime.asserted.lock().unwrap().len();
    assert_eq!(
        check_calls(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY), Some("root")),
        calls
    );
    // The assertion is bound to the change itself: it doesn't verify for another routine.
    let (method, _, body, assertion) = &changes[0];
    let other = pmos_assertion::RequestBinding::new(method, "/v1/routines/rtn_ana1", body).unwrap();
    let policy = pmos_assertion::VerifyPolicy::new("bots", &["console"]).unwrap();
    assert!(ring("cb1", CONSOLE_BOTS_KEY)
        .verify(
            assertion.as_deref().unwrap(),
            &policy,
            pmos_assertion::unix_now(),
            Some(&other),
            &pmos_assertion::MemoryReplayCache::default(),
        )
        .is_err());
}

#[tokio::test]
async fn without_keys_the_console_sends_no_assertion() {
    let c = console(Options::default()).await;
    c.get(Some("ana"), "/activity").await;
    c.get(Some("ana"), "/approvals").await;
    for seen in [&c.runtime, &c.tools] {
        assert!(seen
            .asserted
            .lock()
            .unwrap()
            .iter()
            .all(|(.., a)| a.is_none()));
    }
}

// The user database (docs/user-database-spec.md §2, U4d).

#[tokio::test]
async fn the_proxy_names_a_person_by_sign_in_name_or_by_handle() {
    let c = console(Options::default()).await;
    // By sign-in name, in any case: Ana, a PM.
    let (status, body) = c.get(Some("Ana@Example.COM"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<p class=\"who\">ana</p>"), "{body}");
    assert!(!body.contains("bot-pm-ben"));
    // By handle: the same person (the answer is kept under her handle too, so no new call).
    let (status, body) = c.get(Some("ana"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<p class=\"who\">ana</p>"));
    // Ben, who has no sign-in name, by handle.
    let (status, body) = c.get(Some("ben"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<p class=\"who\">ben</p>") && !body.contains("bot-pm-ana"));
    // Root by sign-in name is an admin.
    let (status, body) = c.get(Some("root@example.com"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("root · admin") && body.contains("bot-pm-ben"));
    // Each lookup went to `resolve` the way it was presented; a sign-in name lower-cased.
    assert_eq!(
        c.directory.lookups(),
        vec![
            "by=sign_in_name&name=ana%40example.com",
            "by=handle&handle=ben",
            "by=sign_in_name&name=root%40example.com",
        ]
    );
    // A Reviewer may use the console (their own things).
    assert_eq!(c.get(Some("rae"), "/activity").await.0, StatusCode::OK);
    // A sign-in name nobody has, and one no admin has confirmed yet.
    let (status, body) = c.get(Some("mallory@example.com"), "/bots").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("Not a PM OS user"));
    let (status, body) = c.get(Some("newbie@example.com"), "/bots").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body.contains("hasn&#39;t confirmed your sign-in name"),
        "{body}"
    );
    // Every lookup used the console's own tool server token.
    assert!(c
        .directory
        .seen
        .calls()
        .iter()
        .all(|(_, _, auth, _)| auth == &format!("Bearer {TOOLS_TOKEN}")));
}

#[tokio::test]
async fn an_auditor_only_person_gets_no_console() {
    let c = console(Options::default()).await;
    for who in ["audrey", "audrey@example.com"] {
        for path in [
            "/bots",
            "/routines",
            "/approvals",
            "/activity",
            "/tasks/tsk_ana1",
        ] {
            let (status, body) = c.get(Some(who), path).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who} {path}");
            assert!(
                body.contains("the audit log is in the web app"),
                "{who} {path}: {body}"
            );
        }
    }
    let res = c
        .post("audrey", "/routines/rtn_ana1/pause", "x", Some(&c.base))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());
    // An Auditor who is also a PM is a PM here: their own only, not an admin.
    let (status, body) = c.get(Some("dot"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("· admin") && !body.contains("bot-pm-ben"));
    let (_, body) = c.get(Some("dot"), "/approvals").await;
    assert!(!body.contains("slack.post_message"));
}

#[tokio::test]
async fn a_deactivated_or_unknown_person_is_refused() {
    let c = console(Options::default()).await;
    // Olga is deactivated; her kept `admin` role grants nothing.
    for who in ["olga", "olga@example.com"] {
        let (status, body) = c.get(Some(who), "/bots").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}");
        assert!(body.contains("deactivated"), "{body}");
    }
    assert_eq!(
        c.get(Some("nobody"), "/bots").await.0,
        StatusCode::FORBIDDEN
    );
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());
}

/// A deactivation, or a removed role, is seen within the 10-second cache: the answer is used
/// for under 10 seconds, then asked for again.
#[tokio::test]
async fn a_change_in_the_directory_counts_within_ten_seconds() {
    let clock = FakeClock::new();
    let c = console(Options {
        clock: Some(clock.clock()),
        ..Options::default()
    })
    .await;
    assert_eq!(c.get(Some("ana"), "/bots").await.0, StatusCode::OK);
    let (_, body) = c.get(Some("root"), "/bots").await;
    assert!(body.contains("root · admin"));
    c.directory.set("ana", "deactivated", &["pm"]);
    c.directory.set("root", "active", &["pm"]);
    // Inside the cache: the earlier answer, with no new lookup.
    clock.advance(9);
    let lookups = c.directory.lookups().len();
    assert_eq!(c.get(Some("ana"), "/bots").await.0, StatusCode::OK);
    assert!(c
        .get(Some("root"), "/bots")
        .await
        .1
        .contains("root · admin"));
    assert_eq!(
        c.directory.lookups().len(),
        lookups,
        "served from the cache"
    );
    // At 10 seconds the change counts.
    clock.advance(1);
    let (status, body) = c.get(Some("ana"), "/bots").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("deactivated"));
    let (status, body) = c.get(Some("root"), "/bots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("· admin") && !body.contains("bot-pm-ben"));
    let (_, body) = c.get(Some("root"), "/routines").await;
    assert!(!body.contains("rtn_ana1"), "no longer everyone's routines");
    // Reactivated: back within the same bound.
    c.directory.set("ana", "active", &["pm"]);
    clock.advance(10);
    assert_eq!(c.get(Some("ana"), "/bots").await.0, StatusCode::OK);
}

/// When the tool server can't answer, the console refuses: `503 users_unavailable`, with no
/// fallback to a kept answer (or to any file).
#[tokio::test]
async fn without_the_user_database_the_console_refuses() {
    let c = console(Options {
        directory_unreachable: true,
        ..Options::default()
    })
    .await;
    for who in ["ana", "root@example.com"] {
        let (status, body) = c.get(Some(who), "/bots").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{who}");
        assert!(body.contains("users_unavailable"), "{body}");
    }
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());

    // Reachable, then the store fails: the kept answer is not used once it is 10 seconds old.
    let clock = FakeClock::new();
    let c = console(Options {
        clock: Some(clock.clock()),
        ..Options::default()
    })
    .await;
    assert_eq!(c.get(Some("root"), "/bots").await.0, StatusCode::OK);
    c.directory.down(true);
    clock.advance(10);
    let before = c.runtime.calls().len();
    for path in ["/bots", "/approvals", "/routines"] {
        let (status, body) = c.get(Some("root"), path).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(body.contains("users_unavailable"));
    }
    assert_eq!(c.runtime.calls().len(), before);
    c.directory.down(false);
    assert_eq!(c.get(Some("root"), "/bots").await.0, StatusCode::OK);
}

/// The console reads no team file: a hostile `team.yaml` in its content root (the setting it
/// used to read) changes nothing, and neither does having none.
#[tokio::test]
async fn a_hostile_or_missing_team_file_changes_nothing() {
    let c = console(Options::default()).await;
    let content = tempfile::tempdir().unwrap();
    std::fs::write(content.path().join("team.yaml"), HOSTILE_TEAM).unwrap();
    for present in [true, false] {
        if !present {
            std::fs::remove_file(content.path().join("team.yaml")).unwrap();
        }
        let root = content.path().display().to_string();
        let config = Config::from_lookup(|k| {
            match k {
                "PMOS_BOTS_TOKEN" => Some(BOTS_TOKEN),
                "PMOS_TOOLSERVER_TOKEN" => Some(TOOLS_TOKEN),
                "PMOS_CONSOLE_TRUST_PROXY" => Some("1"),
                "PMOS_CONSOLE_PROXY_SECRET" => Some(PROXY_SECRET),
                "PMOS_CONTENT_ROOT" => Some(root.as_str()),
                "PMOS_BOTS_URL" => Some(c.bots_url.as_str()),
                "PMOS_TOOLSERVER_URL" => Some(c.tools_url.as_str()),
                _ => None,
            }
            .map(String::from)
        })
        .unwrap();
        // The production router: resolve at PMOS_TOOLSERVER_URL with the console's token.
        let (base, stop) = serve(pmos_console::router(config)).await;
        let get = |user: &'static str, path: &'static str| {
            let base = base.clone();
            async move {
                let res = reqwest::Client::new()
                    .get(format!("{base}{path}"))
                    .header("x-pmos-proxy-secret", PROXY_SECRET)
                    .header("x-forwarded-user", user)
                    .send()
                    .await
                    .unwrap();
                let status = res.status();
                (status, res.text().await.unwrap())
            }
        };
        // Mallory, by handle or by the address the file gives her: nobody.
        for who in ["mallory", "mallory@example.com"] {
            for path in ["/bots", "/approvals", "/tasks/tsk_ana1"] {
                assert_eq!(
                    get(who, path).await.0,
                    StatusCode::FORBIDDEN,
                    "{present} {who} {path}"
                );
            }
        }
        // The file can't make Ana an admin, revive Olga or give Audrey a console.
        let (status, body) = get("ana@example.com", "/bots").await;
        assert_eq!(status, StatusCode::OK, "{present}");
        assert!(body.contains("<p class=\"who\">ana</p>") && !body.contains("· admin"));
        assert!(!body.contains("bot-pm-ben"));
        assert_eq!(get("ana", "/tasks/tsk_ben1").await.0, StatusCode::NOT_FOUND);
        assert_eq!(get("olga", "/bots").await.0, StatusCode::FORBIDDEN);
        assert_eq!(get("audrey", "/bots").await.0, StatusCode::FORBIDDEN);
        // The real admin is unaffected.
        assert!(get("root", "/bots").await.1.contains("root · admin"));
        stop.cancel();
    }
}

/// No decision path in the console reads the team file: nothing in its code names it.
#[test]
fn the_console_reads_no_team_file() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0;
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for banned in [
                "TeamFile",
                "TeamMember",
                "team.yaml",
                "load_team",
                "content_root",
                "PMOS_CONTENT_ROOT",
                "team::",
            ] {
                assert!(
                    !code.contains(banned),
                    "{}:{}: `{banned}` in the console's code",
                    path.display(),
                    n + 1
                );
            }
        }
        checked += 1;
    }
    assert!(checked >= 8, "read every source file");
}

/// The assertion carries the viewer's roles from the user database's answer for this
/// request; an admin's read of another PM's inbox is signed for that PM with no roles.
#[tokio::test]
async fn assertions_carry_the_roles_the_directory_gave() {
    use pmos_contract::model::Role;
    let c = console(Options {
        sign: true,
        ..Options::default()
    })
    .await;
    let verify = |seen: &Seen, audience: &str, key: (&str, &str)| {
        let cache = pmos_assertion::MemoryReplayCache::default();
        let policy = pmos_assertion::VerifyPolicy::new(audience, &["console"]).unwrap();
        seen.asserted
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(m, u, b, a)| {
                let binding = pmos_assertion::RequestBinding::new(m, u, b).unwrap();
                let a = ring(key.0, key.1)
                    .verify(
                        a.as_deref()?,
                        &policy,
                        pmos_assertion::unix_now(),
                        Some(&binding),
                        &cache,
                    )
                    .unwrap();
                Some((a.subject().to_string(), a.roles().to_vec()))
            })
            .collect::<Vec<_>>()
    };
    c.get(Some("ana"), "/activity").await;
    c.get(Some("ana"), "/approvals").await;
    let signed: Vec<_> = verify(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY))
        .into_iter()
        .chain(verify(&c.tools, "toolserver", ("ct1", CONSOLE_TOOLS_KEY)))
        .collect();
    assert!(signed.len() >= 2);
    for (subject, roles) in signed {
        assert_eq!((subject.as_str(), roles), ("ana", vec![Role::Pm]));
    }
    c.runtime.asserted.lock().unwrap().clear();
    c.tools.asserted.lock().unwrap().clear();
    c.get(Some("root"), "/approvals").await;
    c.get(Some("root"), "/activity").await;
    let runtime = verify(&c.runtime, "bots", ("cb1", CONSOLE_BOTS_KEY));
    assert!(!runtime.is_empty());
    for (subject, roles) in runtime {
        assert_eq!(subject, "root");
        assert_eq!(roles, vec![Role::Pm, Role::Admin]);
    }
    let tools = verify(&c.tools, "toolserver", ("ct1", CONSOLE_TOOLS_KEY));
    let mut subjects: Vec<&str> = tools.iter().map(|(s, _)| s.as_str()).collect();
    subjects.sort();
    // The listing and the admin's own inbox are signed as the admin.
    assert_eq!(subjects, vec!["ana", "ben", "cy", "root", "root"]);
    for (subject, roles) in &tools {
        let want = if subject == "root" {
            vec![Role::Pm, Role::Admin]
        } else {
            vec![]
        };
        assert_eq!(roles, &want, "{subject}");
    }
}
