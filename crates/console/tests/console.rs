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
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "console-client-token-0123456789abcdef";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
const HOSTILE: &str = "<script>alert('x')</script>";

const TEAM: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, roles: [pm]}
  - {handle: root, slack-id: U0ROOT, email: root@example.com, roles: [pm, admin]}
";

/// Every call an upstream saw: method, path and query, the bearer token, and the body.
#[derive(Default)]
struct Seen {
    calls: Mutex<Vec<(String, String, String, String)>>,
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
            if let Some(who) = param("requested_by") {
                tasks.retain(|t| t["requested_by"] == json!(who));
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
            if let Some(owner) = param("owner") {
                items.retain(|r| r["owner"] == json!(owner));
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
    Json(json!({ "items": items })).into_response()
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
    runtime: Arc<Seen>,
    tools: Arc<Seen>,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

struct Options {
    identity: Identity,
    runtime_up: bool,
    audit: bool,
    slack: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            identity: Identity::Proxy,
            runtime_up: true,
            audit: true,
            slack: true,
        }
    }
}

async fn console(opts: Options) -> Console {
    let runtime_seen = Arc::new(Seen::default());
    let tools_seen = Arc::new(Seen::default());
    let (bots_url, stop_bots) = serve(
        Router::new()
            .fallback(runtime)
            .with_state(runtime_seen.clone()),
    )
    .await;
    let (tools_url, stop_tools) = serve(
        Router::new()
            .route("/internal/approvals", get(toolserver))
            .with_state(tools_seen.clone()),
    )
    .await;
    if !opts.runtime_up {
        stop_bots.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
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
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        identity: opts.identity,
        bots_url,
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url,
        toolserver_token: TOOLS_TOKEN.into(),
        content_root: dir.path().to_path_buf(),
        audit_log: opts.audit.then_some(audit_path),
        slack_url: opts.slack.then(|| "https://acme.slack.com".to_string()),
        public_url: None,
    };
    let (base, stop_console) = serve(pmos_console::router(config)).await;
    Console {
        base,
        runtime: runtime_seen,
        tools: tools_seen,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        _dir: dir,
        _stop: vec![stop_bots, stop_tools, stop_console],
    }
}

impl Console {
    async fn get(&self, user: Option<&str>, path: &str) -> (StatusCode, String) {
        let mut req = self.http.get(format!("{}{path}", self.base));
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
            .http
            .post(format!("{}{path}", self.base))
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
async fn auth_needs_a_team_member() {
    let c = console(Options::default()).await;
    // No user at all.
    let (status, body) = c.get(None, "/bots").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("Not signed in"));
    // Someone not in team.yaml, with a hostile name.
    let (status, body) = c.get(Some("mallory<script>"), "/activity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("mallory&lt;script&gt;"));
    assert_escaped(&body);
    // Refused requests never reach an upstream.
    assert!(c.runtime.calls().is_empty() && c.tools.calls().is_empty());
    // A member, by handle or by email.
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
        "Summarize overnight #launch",
        "<code>0 8 * * 1-5</code> Europe/London",
        "2026-11-16 08:00 UTC",
        "Weekly status for ben",
        "<span class=\"badge wait\">paused</span>",
        "action=\"/routines/rtn_ana1/pause\"",
        "action=\"/routines/rtn_ben1/resume\"",
        "action=\"/routines/rtn_ben1/delete\"",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(m, p, ..)| m == "GET" && p == "/v1/routines"));

    let (_, body) = c.get(Some("ana"), "/routines").await;
    assert!(body.contains("rtn_ana1") && !body.contains("rtn_ben1"));
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
        .post("ana", "/routines/rtn_ana1/resume", &token, None)
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
    let res = c
        .http
        .post(format!("{}{path}", c.base))
        .header("x-forwarded-user", "ana")
        .header("sec-fetch-site", "cross-site")
        .form(&[("csrf", ana.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // No body at all.
    let res = c
        .http
        .post(format!("{}{path}", c.base))
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
    // Newest first.
    assert!(
        body.find("slack.post_message").unwrap() < body.find("confluence.create_draft").unwrap()
    );
    // Every member's inbox, pending only, with the service token.
    let calls = c.tools.calls();
    for user in ["ana", "ben", "root"] {
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
    assert!(body.contains("What ports does Acme Vault need? &lt;script&gt;"));
    assert_escaped(&body);
    assert!(c
        .runtime
        .calls()
        .iter()
        .any(|(_, p, ..)| p == "/v1/tasks?limit=50"));

    let (_, body) = c.get(Some("ana"), "/activity").await;
    assert!(body.contains("tsk_ana1") && !body.contains("tsk_ben1"));
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
        .http
        .get(format!("{}/bots", c.base))
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
    let res = c.http.get(format!("{}/", c.base)).send().await.unwrap();
    assert_eq!(res.headers()["location"], "/bots");
}
