//! `pmos-web`'s admin area and auditor view (`GET /api/admin/*`, docs/web-api.md, Admin and
//! auditor) against a mock tool server: the role matrix for every route, `/api/me`
//! permissions, pagination and filters, that nothing but metadata reaches the browser, the CSV
//! export's escaping and cap, and that every call names and signs for the viewer. The Team
//! screen's routes (`/api/admin/users…`, `/api/admin/reviewers/…`, slice U5a): who may call,
//! the origin and CSRF checks, the forwarded request and its assertion, the re-picked answers
//! and the relayed refusal codes.

use std::sync::atomic::{AtomicUsize, Ordering};
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

const TEAM: &str = "members:
  - {handle: adam, slack-id: U0ADM, email: adam@example.com, roles: [admin]}
  - {handle: audrey, slack-id: U0AUD, roles: [auditor]}
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: rex, slack-id: U0REX, roles: [reviewer]}
";

/// What the mock tool server answers, when not as the tool server would.
const FORBIDDEN: usize = 1;
const DOWN: usize = 2;

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
    events: Mutex<Vec<Value>>,
    mode: AtomicUsize,
    /// The user database's routes (who is signed in, with which roles).
    directory: support::FakeDirectory,
    /// A refusal the Admin API's writes answer with, instead of success.
    refuse: Mutex<Option<(u16, String)>>,
}

impl Tools {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

/// A query parameter, decoded.
fn query(uri: &Uri, key: &str) -> Option<String> {
    let url = reqwest::Url::parse(&format!("http://mock{uri}")).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn event(i: usize) -> Value {
    json!({
        "id": format!("ev_{i}"), "at": format!("2026-11-16T09:{:02}:00Z", i % 60),
        "actor": "ana", "client": "mcp", "action": "jira.create_issue", "system": "jira",
        "target": format!("apr_{i}"), "skill": null, "bot": "pm-ana", "task_id": "tsk_1",
        "outcome": "pending", "detail": null, "sources": 2, "titled_sources": 1,
    })
}

/// Like the tool server's `GET /internal/admin/audit`: `before` is an index here.
fn audit_page(tools: &Tools, uri: &Uri) -> Value {
    let events = tools.events.lock().unwrap();
    let start = query(uri, "before")
        .and_then(|c| c.strip_prefix("c1.").and_then(|n| n.parse().ok()))
        .unwrap_or(0usize);
    let limit: usize = query(uri, "limit").unwrap().parse().unwrap();
    let end = (start + limit).min(events.len());
    let next = (end < events.len()).then(|| format!("c1.{end}"));
    json!({ "events": events[start.min(end)..end], "next": next })
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
    match tools.mode.load(Ordering::SeqCst) {
        FORBIDDEN => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"code": "forbidden", "error": "no"})),
            )
                .into_response()
        }
        DOWN => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        _ => {}
    }
    let path = uri.path();
    if (path.starts_with("/internal/admin/users") || path.starts_with("/internal/admin/reviewers"))
        && method != Method::GET
    {
        if let Some((status, code)) = tools.refuse.lock().unwrap().clone() {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"code": code, "error": "SECRET tool server sentence"})),
            )
                .into_response();
        }
        let handle = path.split('/').nth(4).unwrap_or("zed");
        let status = if method == Method::POST && path == "/internal/admin/users" {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        return (
            status,
            Json(
                json!({"version": 8, "user": admin_user(handle), "note": "a note",
                        "extra": "SECRET"}),
            ),
        )
            .into_response();
    }
    match path {
        "/internal/admin/users" => Json(json!({
            "version": 7, "view": "admin", "linking": true, "extra": "SECRET",
            "deactivations": {"last_hour": 1, "limit": 3, "remaining": 2, "grace_hours": 24},
            "pilot_exception": {"ends": "2026-12-01", "ends_at": "2026-12-01T00:00:00Z",
                                "ending_soon": false, "lapsed": false},
            "users": [admin_user("adam"), admin_user("audrey"), admin_user("ana"),
                      {"handle": "olga", "display_name": "", "status": "deactivated",
                       "roles": ["pm"], "has_sign_in_name": false, "accounts": null}],
        }))
        .into_response(),
        "/internal/admin/audit" => Json(audit_page(&tools, &uri)).into_response(),
        "/internal/admin/accounts" => Json(json!({
            "linking": true,
            "members": [
                {"handle": "ana", "accounts": {"atlassian": true, "slack": false, "github": false}},
                {"handle": "adam", "accounts": {"atlassian": false, "slack": true, "github": true}},
            ],
        }))
        .into_response(),
        "/internal/admin/connectors" => Json(json!({"connectors": [
            {"name": "docs", "mode": "live", "health": "down", "error_class": "timeout",
             "checked_at": "2026-11-16T09:00:00Z", "error": "SECRET connector text"},
            {"name": "jira", "mode": "live", "health": "ok", "error_class": null,
             "checked_at": "2026-11-16T09:00:00Z"},
        ]}))
        .into_response(),
        "/internal/admin/overview" => Json(json!({
            "since": "2026-11-09T09:00:00Z", "pending_drafts": 4, "approved": 7,
            "rejected": 2, "denied": 1, "retention_days": 90,
            "oldest_event_at": "2026-08-19T03:00:01Z",
            "last_purge": {"at": "2026-11-17T03:07:12Z", "deleted": 1200, "extra": "dropped"},
            "held_drafts": 1,
            "users": {"active": 3, "deactivated": 1, "sign_in_unconfirmed": ["ana"],
                      "dual_role": [], "pilot_exception": null, "role_conflicts": [],
                      "entra_needs_host": [], "no_slack": [], "identity_conflicts": 1,
                      "identity_conflict_users": ["ana"], "resolve_miss_warning": ["console"],
                      "resolve_miss_warning_threshold": 100, "deactivations_last_hour": 1,
                      "deactivation_limit": 3, "deactivation_limit_reached": false,
                      "deactivation_grace_hours": 24, "bots_without_active_owner": [],
                      "password_resets": [{"handle": "ana", "by": "ada", "at": "2026-11-16T08:00:00Z", "extra": "dropped"}],
                      "secret": "SECRET"},
        }))
        .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// One user as the tool server's Admin API answers an admin, with fields the contract doesn't
/// have (which `pmos-web` must drop).
fn admin_user(handle: &str) -> Value {
    json!({
        "handle": handle, "display_name": format!("{handle} <b>"), "status": "active",
        "roles": if handle == "adam" { json!(["admin"]) } else { json!(["pm"]) },
        "created_at": "2026-11-01T00:00:00Z", "deactivated_at": null, "grace_ends_at": null,
        "dual_role": false, "role_conflict": false, "has_sign_in_name": true,
        "sign_in_name_confirmed": true, "sign_in_unconfirmed": false, "entra": "none",
        "entra_needs_host": false, "slack_linked": true, "entra_linked": false,
        "atlassian_linked": false, "github_linked": false,
        "accounts": {"atlassian": handle == "ana", "slack": true, "github": false, "token": "SECRET"},
        "host_only": handle == "adam", "bot": format!("pm-{handle}"), "team_bots": [],
        "reviews": [], "reviewed_by": [],
        "sign_in_name": format!("{handle}@example.com"), "atlassian_email": null,
        "github_login": null, "oid": "SECRET-oid", "slack_id": "SECRET-slack",
    })
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
    headers: reqwest::header::HeaderMap,
    text: String,
    body: Value,
}

async fn web() -> Web {
    let tools = Arc::new(Tools::default());
    tools.events.lock().unwrap().extend((0..5).map(event));
    tools.directory.seed_team(TEAM);
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        // Never called by these routes.
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

impl Web {
    async fn send(&self, cookie: Option<&str>, method: Method, path: &str) -> Reply {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(c) = cookie {
            req = req.header("cookie", format!("__Host-pmos={c}"));
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            text,
            body,
        }
    }

    async fn get(&self, cookie: &str, path: &str) -> Reply {
        self.send(Some(cookie), Method::GET, path).await
    }

    /// Sign in as `handle`: the session cookie.
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
}

const ROUTES: [&str; 5] = [
    "/api/admin/audit",
    "/api/admin/audit/export",
    "/api/admin/team",
    "/api/admin/connectors",
    "/api/admin/overview",
];

#[tokio::test]
async fn role_matrix() {
    let w = web().await;
    for route in ROUTES {
        let r = w.send(None, Method::GET, route).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{route}");
        assert_eq!(r.body["code"], "signed_out");
    }
    for (who, allowed) in [
        ("adam", ROUTES.to_vec()),
        (
            "audrey",
            vec![
                "/api/admin/audit",
                "/api/admin/audit/export",
                "/api/admin/team",
                "/api/admin/overview",
            ],
        ),
        ("ana", vec![]),
        ("rex", vec![]),
    ] {
        let cookie = w.login(who).await;
        for route in ROUTES {
            let before = w.tools.calls().len();
            let r = w.get(&cookie, route).await;
            if allowed.contains(&route) {
                assert_eq!(r.status, StatusCode::OK, "{who} {route}: {}", r.text);
            } else {
                assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} {route}");
                assert_eq!(r.body["code"], "forbidden", "{who} {route}");
                assert_eq!(
                    w.tools.calls().len(),
                    before,
                    "{who} {route}: refused before any upstream call"
                );
            }
        }
    }
    // Only GET.
    let cookie = w.login("adam").await;
    for (method, route) in [
        (Method::POST, "/api/admin/audit"),
        (Method::DELETE, "/api/admin/team"),
        (Method::PUT, "/api/admin/connectors"),
    ] {
        let r = w.send(Some(&cookie), method, route).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{route}");
        assert_eq!(r.body["code"], "method_not_allowed");
    }
}

#[tokio::test]
async fn the_tool_server_decides_too() {
    let w = web().await;
    let cookie = w.login("audrey").await;
    w.tools.mode.store(FORBIDDEN, Ordering::SeqCst);
    for route in [
        "/api/admin/audit",
        "/api/admin/audit/export",
        "/api/admin/team",
        "/api/admin/overview",
    ] {
        let r = w.get(&cookie, route).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{route}");
        assert_eq!(r.body["code"], "forbidden");
    }
    // Down: the team list comes from the user database now, so it fails like every admin read
    // (there is no `team.yaml` to fall back to).
    w.tools.mode.store(DOWN, Ordering::SeqCst);
    for route in ["/api/admin/team", "/api/admin/audit", "/api/admin/users"] {
        let r = w.get(&cookie, route).await;
        assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{route}");
        assert_eq!(r.body["code"], "upstream_unavailable");
    }
}

#[tokio::test]
async fn me_lists_permissions() {
    let w = web().await;
    let perms = |r: &Reply| -> Vec<String> {
        r.body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect()
    };
    let ana = w.get(&w.login("ana").await, "/api/me").await;
    assert_eq!(ana.body["roles"], json!(["pm"]), "unchanged");
    assert!(ana.body["csrf_token"].is_string(), "unchanged");
    let p = perms(&ana);
    assert!(p.contains(&"decide-own-approval".to_string()));
    assert!(!p.contains(&"view-audit-log".to_string()));
    let audrey = w.get(&w.login("audrey").await, "/api/me").await;
    // Audit metadata, and the platform's configuration read-only (the policy and content
    // screens, `admin_proposals.rs`).
    assert_eq!(
        perms(&audrey),
        [
            "view-audit-log",
            "export-audit",
            "view-content",
            "view-policies"
        ]
    );
    let adam = w.get(&w.login("adam").await, "/api/me").await;
    let p = perms(&adam);
    assert_eq!(p.len(), 21);
    assert!(p.contains(&"manage-connectors".to_string()));
}

#[tokio::test]
async fn audit_pages_filters_and_signs_for_the_viewer() {
    let w = web().await;
    let cookie = w.login("audrey").await;
    let mut ids = Vec::new();
    let mut path = "/api/admin/audit?limit=2".to_string();
    loop {
        let r = w.get(&cookie, &path).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        for e in r.body["events"].as_array().unwrap() {
            ids.push(e["id"].as_str().unwrap().to_string());
        }
        match r.body["next_cursor"].as_str() {
            Some(c) => path = format!("/api/admin/audit?limit=2&before={c}"),
            None => {
                assert_eq!(r.body["next_cursor"], Value::Null);
                break;
            }
        }
    }
    assert_eq!(ids, ["ev_0", "ev_1", "ev_2", "ev_3", "ev_4"]);
    // The default page.
    w.get(&cookie, "/api/admin/audit").await;
    assert_eq!(
        query(
            &w.tools.calls().last().unwrap().uri.parse().unwrap(),
            "limit"
        )
        .as_deref(),
        Some("50")
    );
    // Filters go to the tool server checked and normalized, with the viewer named and signed.
    let r = w
        .get(
            &cookie,
            "/api/admin/audit?actor=ana&action=jira.create_issue&outcome=denied\
             &from=2026-11-16T10:00:00%2B01:00&to=2026-11-17T00:00:00Z&ignored=1",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let call = w.tools.calls().last().unwrap().clone();
    let uri: Uri = call.uri.parse().unwrap();
    assert_eq!(uri.path(), "/internal/admin/audit");
    assert_eq!(call.method, "GET");
    assert_eq!(query(&uri, "user").as_deref(), Some("audrey"));
    assert_eq!(query(&uri, "actor").as_deref(), Some("ana"));
    assert_eq!(query(&uri, "action").as_deref(), Some("jira.create_issue"));
    assert_eq!(query(&uri, "outcome").as_deref(), Some("denied"));
    assert_eq!(query(&uri, "from").as_deref(), Some("2026-11-16T09:00:00Z"));
    assert_eq!(query(&uri, "to").as_deref(), Some("2026-11-17T00:00:00Z"));
    assert_eq!(query(&uri, "ignored"), None);
    assert_eq!(query(&uri, "purpose"), None);
    // The assertion names the viewer and is bound to this exact request.
    let policy = pmos_assertion::VerifyPolicy::new("toolserver", &["web"]).unwrap();
    let binding = pmos_assertion::RequestBinding::new("GET", &call.uri, b"").unwrap();
    let a = ring()
        .verify(
            call.assertion.as_deref().expect("signed"),
            &policy,
            pmos_assertion::unix_now(),
            Some(&binding),
            &pmos_assertion::MemoryReplayCache::default(),
        )
        .unwrap();
    assert_eq!(a.subject(), "audrey");
    // Every admin call is signed.
    for path in ["/api/admin/team", "/api/admin/overview"] {
        w.get(&cookie, path).await;
        assert!(
            w.tools.calls().last().unwrap().assertion.is_some(),
            "{path}"
        );
    }
    // Bad parameters never reach the tool server.
    let before = w.tools.calls().len();
    for bad in [
        "limit=0",
        "limit=101",
        "limit=ten",
        "outcome=maybe",
        "from=yesterday",
        "from=2026-11-17T00:00:00Z&to=2026-11-16T00:00:00Z",
        "actor=ana%20smith",
        "action=jira%2Fcreate",
        "before=c1.%22%3E",
    ] {
        let r = w.get(&cookie, &format!("/api/admin/audit?{bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
        assert_eq!(r.body["code"], "bad_request", "{bad}");
    }
    assert_eq!(w.tools.calls().len(), before);
}

#[tokio::test]
async fn only_metadata_reaches_the_browser() {
    let w = web().await;
    {
        let mut events = w.tools.events.lock().unwrap();
        events.clear();
        let mut e = event(0);
        // As if the tool server leaked content: pmos-web serves only its known fields.
        for (k, v) in [
            ("args", json!({"body": "SECRET draft body"})),
            ("text", json!("SECRET slack text")),
            ("prompt", json!("SECRET prompt")),
            ("result", json!({"answer": "SECRET result"})),
            ("source_titles", json!({"jira:PAY-1": "SECRET title"})),
            ("args_hash", json!("SECRET-hash")),
            ("token", json!(TOOLS_TOKEN)),
        ] {
            e[k] = v;
        }
        events.push(e);
    }
    let cookie = w.login("adam").await;
    for path in ["/api/admin/audit", "/api/admin/audit/export"] {
        let r = w.get(&cookie, path).await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(!r.text.contains("SECRET"), "{path}: {}", r.text);
        assert!(!r.text.contains(TOOLS_TOKEN), "{path}");
        assert!(r.text.contains("apr_0"), "{path}");
    }
    let r = w.get(&cookie, "/api/admin/audit").await;
    let keys: Vec<&str> = r.body["events"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action",
            "actor",
            "at",
            "bot",
            "client",
            "detail",
            "id",
            "outcome",
            "skill",
            "sources",
            "system",
            "target",
            "task_id",
            "titled_sources",
        ]
    );
    let r = w.get(&cookie, "/api/admin/connectors").await;
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    assert_eq!(r.body["connectors"][0]["error_class"], "timeout");
    assert_eq!(r.body["connectors"][1]["error_class"], Value::Null);
}

#[tokio::test]
async fn team_and_overview() {
    let w = web().await;
    let cookie = w.login("audrey").await;
    let r = w.get(&cookie, "/api/admin/team").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["accounts_available"], true);
    assert_eq!(r.body["linking"], true);
    // From the user database (`GET /internal/admin/users`): active users only, no addresses.
    let members = r.body["members"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(
        members[0],
        json!({"handle": "adam", "name": "adam <b>", "roles": ["admin"], "has_email": true,
               "bot": "pm-adam",
               "accounts": {"atlassian": false, "slack": true, "github": false}})
    );
    assert!(!r.text.contains("@example.com"), "no email addresses");
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    assert!(w
        .tools
        .calls()
        .iter()
        .any(|c| c.uri == "/internal/admin/users?user=audrey"));

    let r = w.get(&cookie, "/api/admin/overview").await;
    assert_eq!(
        r.body,
        json!({"since": "2026-11-09T09:00:00Z", "pending_drafts": 4, "approved": 7,
               "rejected": 2, "denied": 1, "retention_days": 90,
               "oldest_event_at": "2026-08-19T03:00:01Z",
               "last_purge": {"at": "2026-11-17T03:07:12Z", "deleted": 1200},
               "held_drafts": 1,
               "users": {"active": 3, "deactivated": 1, "sign_in_unconfirmed": ["ana"],
                         "dual_role": [], "pilot_exception": null, "role_conflicts": [],
                         "entra_needs_host": [], "no_slack": [], "identity_conflicts": 1,
                         "identity_conflict_users": ["ana"],
                         "resolve_miss_warning": ["console"],
                         "resolve_miss_warning_threshold": 100, "deactivations_last_hour": 1,
                         "deactivation_limit": 3, "deactivation_limit_reached": false,
                         "deactivation_grace_hours": 24, "bots_without_active_owner": [],
                         "password_resets": [{"handle": "ana", "by": "ada", "at": "2026-11-16T08:00:00Z"}]}})
    );
}

#[tokio::test]
async fn csv_export_escapes_and_caps() {
    let w = web().await;
    {
        let mut events = w.tools.events.lock().unwrap();
        events.clear();
        let mut e = event(0);
        e["actor"] = json!("=HYPERLINK(\"http://x\")");
        e["target"] = json!("@SUM(1)");
        e["detail"] = json!("-2+3");
        e["bot"] = json!("+cmd");
        e["skill"] = json!("\tx");
        e["task_id"] = json!("a,b\"c");
        events.push(e);
    }
    let cookie = w.login("audrey").await;
    let r = w
        .get(&cookie, "/api/admin/audit/export?from=2026-11-01T00:00:00Z")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["content-type"], "text/csv; charset=utf-8");
    assert!(r.headers["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    assert_eq!(r.headers["x-pmos-truncated"], "false");
    assert_eq!(r.headers["x-pmos-row-count"], "1");
    let lines: Vec<&str> = r.text.split("\r\n").collect();
    assert_eq!(
        lines[0],
        "\"at\",\"actor\",\"client\",\"action\",\"system\",\"target\",\"outcome\",\"detail\",\
         \"sources\",\"titled_sources\",\"skill\",\"bot\",\"task_id\",\"id\""
    );
    assert_eq!(
        lines[1],
        "\"2026-11-16T09:00:00Z\",\"'=HYPERLINK(\"\"http://x\"\")\",\"mcp\",\"jira.create_issue\",\
         \"jira\",\"'@SUM(1)\",\"pending\",\"'-2+3\",\"2\",\"1\",\"'\tx\",\"'+cmd\",\"a,b\"\"c\",\
         \"ev_0\""
    );
    assert_eq!(lines[2], "");
    let call = w.tools.calls().last().unwrap().clone();
    let uri: Uri = call.uri.parse().unwrap();
    assert_eq!(query(&uri, "purpose").as_deref(), Some("export"));
    assert_eq!(query(&uri, "from").as_deref(), Some("2026-11-01T00:00:00Z"));
    assert_eq!(query(&uri, "user").as_deref(), Some("audrey"));

    // More than the cap: 10,000 rows, and the header says so.
    {
        let mut events = w.tools.events.lock().unwrap();
        events.clear();
        events.extend((0..10_050).map(event));
    }
    let before = w.tools.calls().len();
    let r = w.get(&cookie, "/api/admin/audit/export").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["x-pmos-truncated"], "true");
    assert_eq!(r.headers["x-pmos-row-count"], "10000");
    assert_eq!(
        r.text.matches("\r\n").count(),
        10_001,
        "header + 10,000 rows"
    );
    let pages = w.tools.calls().len() - before;
    assert_eq!(pages, 10, "read in pages of 1,000");
    // Exactly the cap: not truncated.
    w.tools.events.lock().unwrap().truncate(10_000);
    let r = w.get(&cookie, "/api/admin/audit/export").await;
    assert_eq!(r.headers["x-pmos-truncated"], "false");
    assert_eq!(r.headers["x-pmos-row-count"], "10000");
}

// --- The Team screen's routes (slice U5a) --------------------------------------------------

impl Web {
    /// A write as the app sends it: same origin, the session's CSRF token, JSON.
    async fn write(
        &self,
        cookie: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        origin: Option<&str>,
    ) -> Reply {
        let me = self.get(cookie, "/api/me").await;
        let csrf = me.body["csrf_token"].as_str().unwrap().to_string();
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("cookie", format!("__Host-pmos={cookie}"))
            .header("x-pmos-csrf", csrf);
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            text,
            body,
        }
    }
}

/// Every write route, with a body it takes.
fn user_writes() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (
            Method::POST,
            "/api/admin/users",
            Some(json!({"handle": "zed", "sign_in_name": "zed@example.com",
                        "atlassian_email": "zed@example.com", "roles": ["pm"]})),
        ),
        (
            Method::PUT,
            "/api/admin/users/ana/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": 7})),
        ),
        (
            Method::PUT,
            "/api/admin/users/ana/fields",
            Some(json!({"display_name": "Ana", "expected_version": 7})),
        ),
        (
            Method::POST,
            "/api/admin/users/ana/sign-in-name/confirm",
            Some(json!({"sign_in_name": "ana@example.com"})),
        ),
        (Method::POST, "/api/admin/users/ana/deactivate", None),
        (
            Method::POST,
            "/api/admin/users/ana/reactivate",
            Some(json!({})),
        ),
        (
            Method::DELETE,
            "/api/admin/users/ana/identities/entra",
            None,
        ),
        (
            Method::PUT,
            "/api/admin/reviewers/rex",
            Some(json!({"pms": ["ana"]})),
        ),
    ]
}

#[tokio::test]
async fn user_routes_forward_signed_for_the_admin() {
    let w = web().await;
    let cookie = w.login("adam").await;
    let base = w.base.clone();
    for (method, path, body) in user_writes() {
        let before = w.tools.calls().len();
        let r = w
            .write(&cookie, method.clone(), path, body.clone(), Some(&base))
            .await;
        let want = if path == "/api/admin/users" { 201 } else { 200 };
        assert_eq!(r.status.as_u16(), want, "{path}: {}", r.text);
        assert_eq!(r.body["version"], 8, "{path}");
        assert_eq!(r.body["note"], "a note");
        assert!(!r.text.contains("SECRET"), "{path}: {}", r.text);
        // An admin gets the addresses back, never an oid or a Slack id.
        assert!(r.body["user"].get("sign_in_name").is_some(), "{path}");
        let calls = w.tools.calls();
        assert_eq!(calls.len(), before + 1, "{path}: one call");
        let call = calls.last().unwrap();
        let upstream = path.replacen("/api/", "/internal/", 1);
        assert_eq!(call.uri, format!("{upstream}?user=adam"), "{path}");
        assert_eq!(call.method, method.as_str());
        // The body goes as sent, and the assertion is bound to it.
        let sent: Value = if call.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&call.body).unwrap()
        };
        assert_eq!(sent, body.clone().unwrap_or(Value::Null), "{path}");
        let policy = pmos_assertion::VerifyPolicy::new("toolserver", &["web"]).unwrap();
        let binding =
            pmos_assertion::RequestBinding::new(method.as_str(), &call.uri, &call.body).unwrap();
        let a = ring()
            .verify(
                call.assertion.as_deref().expect("signed"),
                &policy,
                pmos_assertion::unix_now(),
                Some(&binding),
                &pmos_assertion::MemoryReplayCache::default(),
            )
            .unwrap();
        assert_eq!(a.subject(), "adam", "{path}");
    }
}

#[tokio::test]
async fn user_writes_need_an_admin_the_origin_and_the_csrf_token() {
    let w = web().await;
    let base = w.base.clone();
    // An auditor or a PM: refused here, with no upstream call.
    for who in ["audrey", "ana"] {
        let cookie = w.login(who).await;
        for (method, path, body) in user_writes() {
            let before = w.tools.calls().len();
            let r = w.write(&cookie, method, path, body, Some(&base)).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} {path}");
            assert_eq!(r.body["code"], "forbidden");
            assert_eq!(w.tools.calls().len(), before, "{who} {path}");
        }
    }
    let cookie = w.login("adam").await;
    for (method, path, body) in user_writes() {
        let before = w.tools.calls().len();
        // No origin, or another one.
        for origin in [None, Some("https://evil.example")] {
            let r = w
                .write(&cookie, method.clone(), path, body.clone(), origin)
                .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
            assert_eq!(r.body["code"], "bad_origin", "{path}");
        }
        // No CSRF token.
        let mut req = w
            .http
            .request(method.clone(), format!("{}{path}", w.base))
            .header("cookie", format!("__Host-pmos={cookie}"))
            .header("origin", base.clone());
        if let Some(b) = &body {
            req = req.json(b);
        }
        let res = req.send().await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            w.tools.calls().len(),
            before,
            "{path}: refused before any call"
        );
    }
    // Bodies that aren't an object; a handle that can't be one; an unknown identity kind.
    for (method, path, body) in [
        (
            Method::PUT,
            "/api/admin/users/ana/roles",
            Some(json!(["pm"])),
        ),
        (Method::POST, "/api/admin/users", None),
    ] {
        let r = w.write(&cookie, method, path, body, Some(&base)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}");
    }
    for (method, path) in [
        (Method::PUT, "/api/admin/users/Ana%20X/roles"),
        (Method::DELETE, "/api/admin/users/ana/identities/slack"),
    ] {
        let r = w
            .write(&cookie, method, path, Some(json!({})), Some(&base))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
    }
    // Signed out.
    let r = w
        .send(None, Method::POST, "/api/admin/users/ana/deactivate")
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn user_refusals_keep_the_tool_servers_code() {
    let w = web().await;
    let cookie = w.login("adam").await;
    let base = w.base.clone();
    for (status, code) in [
        (403, "self_change"),
        (403, "host_only"),
        (409, "last_admin"),
        (409, "stale"),
        (409, "owns_bots"),
        (409, "dual_role"),
        (429, "deactivation_limit"),
        (503, "assertion_required"),
        (422, "invalid"),
        (404, "not_found"),
    ] {
        *w.tools.refuse.lock().unwrap() = Some((status, code.to_string()));
        let r = w
            .write(
                &cookie,
                Method::POST,
                "/api/admin/users/ana/deactivate",
                None,
                Some(&base),
            )
            .await;
        assert_eq!(r.status.as_u16(), status, "{code}: {}", r.text);
        assert_eq!(r.body["code"], code);
        // The sentence is pmos-web's own.
        assert!(!r.text.contains("SECRET"), "{code}");
    }
    // A code this server doesn't know is the tool server refusing.
    *w.tools.refuse.lock().unwrap() = Some((409, "something_new".to_string()));
    let r = w
        .write(
            &cookie,
            Method::POST,
            "/api/admin/users/ana/deactivate",
            None,
            Some(&base),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.body["code"], "upstream_unavailable");
}

#[tokio::test]
async fn the_user_list_is_re_picked_and_addresses_go_to_admins_only() {
    let w = web().await;
    let adam = w.login("adam").await;
    let r = w.get(&adam, "/api/admin/users").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(!r.text.contains("SECRET"), "{}", r.text);
    assert_eq!(r.body["version"], 7);
    assert_eq!(r.body["view"], "admin");
    assert_eq!(r.body["deactivations"]["remaining"], 2);
    assert_eq!(r.body["pilot_exception"]["ends"], "2026-12-01");
    assert_eq!(r.body["users"][0]["sign_in_name"], "adam@example.com");
    assert_eq!(r.body["users"][3]["status"], "deactivated");
    // An auditor: the addresses are left out even if the tool server sent them.
    let audrey = w.login("audrey").await;
    let r = w.get(&audrey, "/api/admin/users").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["view"], "auditor");
    assert!(!r.text.contains("@example.com"), "{}", r.text);
    for u in r.body["users"].as_array().unwrap() {
        for key in [
            "sign_in_name",
            "atlassian_email",
            "github_login",
            "oid",
            "slack_id",
        ] {
            assert!(u.get(key).is_none(), "{key}");
        }
    }
    // A PM: refused here.
    let ana = w.login("ana").await;
    let before = w.tools.calls().len();
    let r = w.get(&ana, "/api/admin/users").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(w.tools.calls().len(), before);
}
