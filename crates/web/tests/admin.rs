//! `pmos-web`'s admin area and auditor view (`GET /api/admin/*`, docs/web-api.md, Admin and
//! auditor) against a mock tool server: the role matrix for every route, `/api/me`
//! permissions, pagination and filters, that nothing but metadata reaches the browser, the CSV
//! export's escaping and cap, and that every call names and signs for the viewer.

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
}

#[derive(Default)]
struct Tools {
    calls: Mutex<Vec<Call>>,
    events: Mutex<Vec<Value>>,
    mode: AtomicUsize,
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
    _body: Bytes,
) -> Response {
    tools.calls.lock().unwrap().push(Call {
        uri: uri.to_string(),
        method: method.to_string(),
        assertion: headers
            .get("x-pmos-assertion")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
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
    match uri.path() {
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
            "rejected": 2, "denied": 1,
        }))
        .into_response(),
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
    headers: reqwest::header::HeaderMap,
    text: String,
    body: Value,
}

async fn web() -> Web {
    let tools = Arc::new(Tools::default());
    tools.events.lock().unwrap().extend((0..5).map(event));
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
            dir.path().to_path_buf(),
        )),
        content_root: dir.path().to_path_buf(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
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
    // Down: the team list still loads from team.yaml, without accounts.
    w.tools.mode.store(DOWN, Ordering::SeqCst);
    let r = w.get(&cookie, "/api/admin/team").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["accounts_available"], false);
    assert_eq!(r.body["linking"], Value::Null);
    assert_eq!(r.body["members"][0]["accounts"], Value::Null);
    let r = w.get(&cookie, "/api/admin/audit").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.body["code"], "upstream_unavailable");
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
    assert_eq!(perms(&audrey), ["view-audit-log", "export-audit"]);
    let adam = w.get(&w.login("adam").await, "/api/me").await;
    let p = perms(&adam);
    assert_eq!(p.len(), 18);
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
    let members = r.body["members"].as_array().unwrap();
    assert_eq!(members.len(), 4);
    assert_eq!(
        members[0],
        json!({"handle": "adam", "name": "adam", "roles": ["admin"], "has_email": true,
               "bot": "pm-adam",
               "accounts": {"atlassian": false, "slack": true, "github": true}})
    );
    assert_eq!(members[1]["has_email"], false);
    // Not in the tool server's answer: nothing linked.
    assert_eq!(
        members[1]["accounts"],
        json!({"atlassian": false, "slack": false, "github": false})
    );
    assert!(!r.text.contains("@example.com"), "no email addresses");

    let r = w.get(&cookie, "/api/admin/overview").await;
    assert_eq!(
        r.body,
        json!({"since": "2026-11-09T09:00:00Z", "pending_drafts": 4, "approved": 7,
               "rejected": 2, "denied": 1})
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
