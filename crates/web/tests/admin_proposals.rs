//! The admin Policy Admin and content screens (`/api/admin/policy/*`, `/api/admin/content/*`,
//! `/api/admin/harness`; docs/web-api.md, Admin: policy and Admin: content and harness) against
//! a copy of the real content tree, a mock tool server and a mock runtime: the role matrix for
//! every route (validation is admins' only: A1), what an auditor and a PM can't read through
//! validation (a restricted context file, a PM's own `AGENTS.md`), the audit events pmos-web
//! sends (role refusals, invalid and stale proposals, an admin's sensitive reads, recorded
//! before the text is served: A4), validation errors and widening detection, path traversal
//! and symlinks, size and type caps, the change set sent to the tool server (pmos-web holds no
//! GitHub token: A2/A3) and how its answers are relayed (`github_not_linked`, stale base, rate
//! limit, audit down, not configured), the branch-protection field, and logs that hold no file
//! text. The tool server's own checks and the pull request are tested in
//! `crates/toolserver/tests/admin_open.rs`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::Duration;
use pmos_web::admin_config::AdminConfig;
use pmos_web::proposals::git_blob_sha;
use pmos_web::upstream::{Credentials, Signer};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

mod support;

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
// Test key only: fixed bytes, never used anywhere real.
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const BASE: &str = "1111111111111111111111111111111111111111";
/// A marker put in proposed file text: it must never reach a log.
const SECRET_TEXT: &str = "MARKER-proposed-file-text-7c41";
/// The restricted file's and Ana's master file's bodies: never served to an auditor or a PM.
const RESTRICTED_BODY: &str = "Restricted body.";
const ANA_BODY: &str = "Ana's own";

/// The fake user directory's people (`FakeDirectory::seed_team`).
const USERS: &str = "members:
  - {handle: adam, slack-id: U0ADM, email: adam@example.com, roles: [admin]}
  - {handle: audrey, slack-id: U0AUD, roles: [auditor]}
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: rex, slack-id: U0REX, roles: [reviewer]}
";

// Logs: every test's, in one buffer (the subscriber is process-wide).

fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = buf.clone();
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || LogWriter(w.clone()))
            .try_init();
        buf
    })
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// The mock tool server.

#[derive(Debug, Clone)]
struct Call {
    method: String,
    path: String,
    query: String,
    auth: Option<String>,
    assertion: Option<String>,
    body: Value,
}

fn call(method: &Method, uri: &Uri, headers: &HeaderMap, body: &Bytes) -> Call {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    Call {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or_default().to_string(),
        auth: h("authorization"),
        assertion: h("x-pmos-assertion"),
        body: serde_json::from_slice(body).unwrap_or(Value::Null),
    }
}

#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Call>>,
    /// What `POST …/proposals/open` answers; `None`: `201` with a pull request.
    open_reply: Mutex<Option<(u16, Value)>>,
    /// The event route fails.
    events_down: AtomicBool,
    /// The viewer has no linked GitHub account.
    not_linked: AtomicBool,
    /// Proposals are off on the tool server.
    unconfigured: AtomicBool,
    /// Sign-in events (`POST /internal/admin/events`), kept apart from the admin actions
    /// these tests count.
    sign_ins: Mutex<Vec<Call>>,
    /// The user database's routes (who is signed in, with which roles).
    directory: support::FakeDirectory,
    /// The bot marketplace's routes (the catalog, each PM's team), answered apart.
    bots: support::FakeBots,
}

impl Mock {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
    fn to(&self, path: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.path == path)
            .collect()
    }
    fn opened(&self) -> Vec<Call> {
        self.to("/internal/admin/proposals/open")
    }
    fn events(&self) -> Vec<Call> {
        self.to("/internal/admin/actions")
    }
    fn reply(&self, status: u16, body: Value) {
        *self.open_reply.lock().unwrap() = Some((status, body));
    }
}

fn pull_request(open: &Value) -> Value {
    let kind = open["kind"].as_str().unwrap_or_default();
    json!({
        "number": 42,
        "url": "https://github.example/acme/pmos/pull/42",
        "author": "adam-gh",
        "branch": format!("pmos/proposal/{kind}/adam-20261117-101509-d2aabc50"),
        "base": {"branch": "main", "commit": BASE},
        "files": open["changes"].as_array().map(|c| c.iter().map(|f| f["path"].clone()).collect::<Vec<_>>()),
        "widening": false,
        "allowance_changes": [],
        "stale_check": "passed",
        "labels_added": true,
    })
}

async fn toolserver(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = m.directory.handle(&method, &uri, &body) {
        return res;
    }
    if let Some(res) = m.bots.handle(&method, &uri, &headers, &body) {
        return res;
    }
    let c = call(&method, &uri, &headers, &body);
    if c.path == "/internal/admin/events" {
        m.sign_ins.lock().unwrap().push(c);
        return Json(json!({"recorded": true})).into_response();
    }
    m.calls.lock().unwrap().push(c.clone());
    if c.auth.as_deref() != Some(&format!("Bearer {TOOLS_TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let unconfigured = m.unconfigured.load(Ordering::SeqCst);
    let not_linked = m.not_linked.load(Ordering::SeqCst);
    let coded = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"code": code, "error": "the tool server's own words"})),
        )
            .into_response()
    };
    match (c.method.as_str(), c.path.as_str()) {
        ("POST", "/internal/admin/actions") => {
            if m.events_down.load(Ordering::SeqCst) {
                return coded(503, "audit_unavailable");
            }
            Json(json!({"recorded": true})).into_response()
        }
        ("POST", "/internal/admin/proposals/open") => {
            if unconfigured {
                return coded(501, "not_configured");
            }
            if not_linked {
                return coded(409, "github_not_linked");
            }
            match m.open_reply.lock().unwrap().clone() {
                Some((status, body)) => {
                    let mut res =
                        (StatusCode::from_u16(status).unwrap(), Json(body.clone())).into_response();
                    if let Some(s) = body.get("retry_after").and_then(Value::as_u64) {
                        res.headers_mut()
                            .insert("retry-after", s.to_string().parse().unwrap());
                    }
                    res
                }
                None => (StatusCode::CREATED, Json(pull_request(&c.body))).into_response(),
            }
        }
        ("GET", "/internal/admin/proposals") => {
            if unconfigured {
                return coded(501, "not_configured");
            }
            if not_linked {
                return Json(json!({"repo": "acme/pmos", "github": "not_linked", "proposals": []}))
                    .into_response();
            }
            let kind = if c.query.contains("kind=policy") {
                "policy"
            } else {
                "content"
            };
            Json(json!({"repo": "acme/pmos", "github": "linked", "proposals": [
                {"number": 42, "url": "https://github.example/acme/pmos/pull/42",
                 "title": format!("PM OS {kind}: x"), "branch": format!("pmos/proposal/{kind}/adam-1"),
                 "base": "main", "status": "open", "proposed_by": "adam", "base_commit": BASE,
                 "author": "adam-gh", "labels": ["pmos-proposal"], "created_at": null, "updated_at": null}
            ]}))
            .into_response()
        }
        ("GET", "/internal/admin/protection") => {
            if not_linked {
                return Json(json!({"configured": !unconfigured, "branch": "main", "required_reviews": null, "verified": false, "github": "not_linked"}))
                    .into_response();
            }
            Json(json!({"configured": !unconfigured, "branch": "main", "required_reviews": 1, "verified": true, "source": "branch_protection", "github": "linked"}))
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn runtime(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let _ = (method, headers);
    match uri.path() {
        "/v1/bots" => Json(json!([
            {"name": "docs", "role": "docs-helper", "scope": "team", "owner": "TODO", "description": "d"}
        ]))
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

// The fixture: the repository's own content, with this test's team, and its policies
// generated from it.

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_tree(from: &Path, to: &Path) {
    for e in walkdir::WalkDir::new(from).follow_links(false) {
        let e = e.unwrap();
        let rel = e.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else if e.file_type().is_file() {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let content = dir.path().join("content");
    copy_tree(&repo_root().join("content"), &content);
    // A restricted context file and a PM's own master file.
    std::fs::write(
        content.join("context/products/secret-roadmap.md"),
        "---\ntitle: Secret roadmap\nowner: \"adam\"\nlast-reviewed: 2026-11-14\nsensitivity: restricted\n---\n\nRestricted body.\n",
    )
    .unwrap();
    std::fs::create_dir_all(content.join("master/people/ana")).unwrap();
    std::fs::write(content.join("master/people/ana/AGENTS.md"), "# Ana's own\n").unwrap();
    let today = chrono::Utc::now().date_naive();
    let generated = pmos_policy::generate(&content, today).expect("the content generates");
    pmos_policy::write(&dir.path().join("policies"), &generated).unwrap();
    std::fs::write(
        dir.path().join("gateway.yaml"),
        "upstreams:\n  qwen: http://vllm-default:8000/v1\nroles:\n  default:\n    upstream: qwen\n    model: qwen3.6-35b-a3b\n  extraction:\n    upstream: qwen\n    model: nemotron-3-nano\n  reasoning:\n    upstream: qwen\n    model: qwen3.6-35b-a3b\n",
    )
    .unwrap();
    dir
}

/// Blob ids of every file under `root/content` and `root/policies`, by repository path.
fn blobs_of(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for top in ["content", "policies"] {
        for e in walkdir::WalkDir::new(root.join(top)) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, git_blob_sha(&std::fs::read(e.path()).unwrap()));
            }
        }
    }
    out
}

/// The admin routes live on the admin door only (slice H10): `base` and `http` reach it.
struct Web {
    base: String,
    tools: Arc<Mock>,
    http: reqwest::Client,
    sessions: Arc<dyn pmos_web::session::SessionStore>,
    dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }
}

async fn web() -> Web {
    logs();
    let tools = Arc::new(Mock::default());
    tools.directory.seed_team(USERS);
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let (bots_url, stop_bots) = serve(Router::new().fallback(runtime)).await;
    let dir = fixture();
    let content = dir.path().join("content");
    let door = support::door::listener().await;
    let ring = pmos_assertion::KeyRing::new(
        pmos_assertion::Key::from_base64("wt1", WEB_TOOLS_KEY).unwrap(),
    );
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url,
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url,
        toolserver_token: Credentials::new(TOOLS_TOKEN).with_signer(Signer::new(
            "web",
            "toolserver",
            ring,
        )),
        content_root: content.clone(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        // The admin door's sign-in (it never serves development sign-in).
        password_login: true,
        trusted_proxies: Vec::new(),
        admin: Some(support::door::tunnel(door.port)),
        password_clients: Vec::new(),
    };
    let admin = AdminConfig {
        policies_root: Some(dir.path().join("policies")),
        gateway_config: Some(dir.path().join("gateway.yaml")),
        ..AdminConfig::default()
    };
    let sessions: Arc<dyn pmos_web::session::SessionStore> =
        Arc::new(pmos_web::session::MemorySessionStore::new());
    let deps = Deps {
        admin,
        sessions: sessions.clone(),
        ..Deps::in_memory()
    };
    let server = pmos_web::app::build_server(cfg, deps).unwrap();
    let (_public, stop_web) = serve(server.public).await;
    let port = door.port;
    let stop_admin = support::door::serve(door, server.admin.unwrap());
    Web {
        base: support::door::base(port),
        tools,
        http: support::door::client(port),
        sessions,
        dir,
        _stop: vec![stop_tools, stop_bots, stop_web, stop_admin],
    }
}

#[derive(Clone)]
struct Who {
    cookie: String,
    csrf: String,
}

impl Web {
    async fn send(
        &self,
        who: Option<&Who>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Reply {
        // A browser sends Origin on every write; without it the server refuses the request.
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("origin", self.base.clone());
        if let Some(w) = who {
            req = req
                .header("cookie", format!("__Host-pmos-admin={}", w.cookie))
                .header("x-pmos-csrf", &w.csrf);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
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

    async fn get(&self, who: &Who, path: &str) -> Reply {
        self.send(Some(who), Method::GET, path, None).await
    }

    async fn post(&self, who: &Who, path: &str, body: Value) -> Reply {
        self.send(Some(who), Method::POST, path, Some(body)).await
    }

    /// An admin-door session for `handle` (every request checks the person's roles there).
    async fn login(&self, handle: &str) -> Who {
        let (cookie, csrf) = support::door::session(&self.sessions, handle).await;
        Who { cookie, csrf }
    }

    fn content(&self) -> PathBuf {
        self.dir.path().join("content")
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.content().join(rel)).unwrap()
    }
}

fn tools_spec(tools: &[&str]) -> Value {
    json!({"skills": {"docs": {"tools": tools}}})
}

fn proposal(spec: Value) -> Value {
    json!({"spec": spec, "title": "Let the Docs bot search Jira", "reason": "PMs asked for it @everyone ``` fence"})
}

const RESTRICTED: &str = "content/context/products/secret-roadmap.md";
const ANA_MASTER: &str = "content/master/people/ana/AGENTS.md";

// The role matrix.

#[tokio::test]
async fn role_matrix_for_every_route() {
    let w = web().await;
    let file = "/api/admin/content/file?path=content/skills/docs/SKILL.md";
    let validate_spec = json!({"spec": tools_spec(&["docs.search", "docs.read"])});
    let validate_content = json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x"}]});
    // (method, path, body, auditor allowed)
    let routes: Vec<(Method, &str, Option<Value>, bool)> = vec![
        (Method::GET, "/api/admin/policy", None, true),
        // Validation shows the current text of the files it names: admins only (A1).
        (
            Method::POST,
            "/api/admin/policy/validate",
            Some(validate_spec.clone()),
            false,
        ),
        (Method::GET, "/api/admin/policy/proposals", None, true),
        (
            Method::POST,
            "/api/admin/policy/proposals",
            Some(proposal(tools_spec(&["docs.search"]))),
            false,
        ),
        (Method::GET, "/api/admin/content", None, true),
        (Method::GET, file, None, true),
        (
            Method::POST,
            "/api/admin/content/validate",
            Some(validate_content.clone()),
            false,
        ),
        (Method::GET, "/api/admin/content/proposals", None, true),
        (
            Method::POST,
            "/api/admin/content/proposals",
            Some(
                json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x"}], "title": "t", "reason": "r"}),
            ),
            false,
        ),
        (Method::GET, "/api/admin/harness", None, true),
    ];
    for (method, path, body, _) in &routes {
        let r = w.send(None, method.clone(), path, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(r.code(), "signed_out");
    }
    let adam = w.login("adam").await;
    let audrey = w.login("audrey").await;
    // A PM or a Reviewer: slice H10 serves these routes on the admin door only, which refuses
    // anyone but an Admin or an Auditor before any route runs (and ends that session), so no
    // route's own refusal is reached or recorded for them.
    for name in ["ana", "rex"] {
        for (method, path, body, _) in &routes {
            let who = w.login(name).await;
            let r = w.send(Some(&who), method.clone(), path, body.clone()).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{name} {path}");
            assert_eq!(r.code(), "admin_door_not_allowed", "{name} {path}");
            let text = r.body.to_string();
            assert!(!text.contains(RESTRICTED_BODY), "{name} {path}: {text}");
        }
        assert!(
            w.tools
                .events()
                .iter()
                .all(|c| c.query != format!("user={name}")),
            "{name}: no route ran"
        );
    }
    for (method, path, body, auditor) in &routes {
        let r = w
            .send(Some(&audrey), method.clone(), path, body.clone())
            .await;
        if *auditor {
            assert!(
                r.status.is_success(),
                "audrey {path}: {} {}",
                r.status,
                r.body
            );
        } else {
            assert_eq!(r.status, StatusCode::FORBIDDEN, "audrey {path}");
            assert_eq!(r.code(), "forbidden");
        }
    }
    let actions: Vec<String> = w
        .tools
        .events()
        .iter()
        .filter(|c| c.query == "user=audrey")
        .map(|c| c.body["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        actions,
        [
            "admin.policy_validate",
            "admin.policy_proposal",
            "admin.content_validate",
            "admin.content_proposal"
        ]
    );
    assert!(
        w.tools.opened().is_empty(),
        "nobody but an admin reached the proposal route"
    );
    for (method, path, body, _) in &routes {
        if method == Method::POST && path.ends_with("/proposals") {
            continue; // Opened below, in their own tests.
        }
        let r = w
            .send(Some(&adam), method.clone(), path, body.clone())
            .await;
        assert!(
            r.status.is_success(),
            "adam {path}: {} {}",
            r.status,
            r.body
        );
    }
    // Without the CSRF token, a write is refused before anything else.
    let r = w
        .http
        .post(format!("{}/api/admin/policy/validate", w.base))
        .header("origin", w.base.clone())
        .header("cookie", format!("__Host-pmos-admin={}", adam.cookie))
        .json(&validate_spec)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(r.json::<Value>().await.unwrap()["code"], "bad_csrf");
    // Only the routes' own methods.
    let r = w
        .send(
            Some(&adam),
            Method::PUT,
            "/api/admin/policy",
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn me_lists_the_new_permissions() {
    let w = web().await;
    let perms = |r: &Reply| -> Vec<String> {
        r.body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect()
    };
    let audrey = w.get(&w.login("audrey").await, "/api/me").await;
    assert_eq!(
        perms(&audrey),
        [
            "view-audit-log",
            "export-audit",
            "view-content",
            "view-policies"
        ]
    );
    let adam = perms(&w.get(&w.login("adam").await, "/api/me").await);
    for p in [
        "view-content",
        "edit-content",
        "view-policies",
        "edit-policies",
    ] {
        assert!(adam.contains(&p.to_string()), "{p}");
    }
    // A PM isn't served on the admin door at all (slice H10).
    let ana = w.get(&w.login("ana").await, "/api/me").await;
    assert_eq!(ana.code(), "admin_door_not_allowed");
}

// A1: restricted context and a PM's own master file, through validation.

#[tokio::test]
async fn an_auditor_or_a_pm_cant_read_restricted_or_personal_files_through_validation() {
    let w = web().await;
    // A PM never gets past the admin door (slice H10): refused before any route.
    let ana = w.login("ana").await;
    let r = w
        .post(
            &ana,
            "/api/admin/content/validate",
            json!({"changes": [{"path": ANA_MASTER, "delete": true}]}),
        )
        .await;
    assert_eq!(r.code(), "admin_door_not_allowed");
    assert!(!r.body.to_string().contains(ANA_BODY));
    for who in ["audrey"] {
        let s = w.login(who).await;
        for path in [RESTRICTED, ANA_MASTER] {
            // The reviewer's scenario: propose to delete it, read the diff.
            for (route, body) in [
                (
                    "/api/admin/content/validate",
                    json!({"changes": [{"path": path, "delete": true}]}),
                ),
                (
                    "/api/admin/content/validate",
                    json!({"changes": [{"path": path, "text": "x"}]}),
                ),
                (
                    "/api/admin/content/proposals",
                    json!({"changes": [{"path": path, "delete": true}], "title": "t", "reason": "r"}),
                ),
            ] {
                let r = w.post(&s, route, body).await;
                assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} {route} {path}");
                assert_eq!(r.code(), "forbidden");
                let text = r.body.to_string();
                assert!(
                    !text.contains(RESTRICTED_BODY) && !text.contains(ANA_BODY),
                    "{who} {path}: {text}"
                );
            }
            let r = w
                .get(&s, &format!("/api/admin/content/file?path={path}"))
                .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} {path}");
            assert!(!r.body.to_string().contains(RESTRICTED_BODY));
        }
        // The policy validation is closed to them too.
        let r = w
            .post(
                &s,
                "/api/admin/policy/validate",
                json!({"spec": tools_spec(&["docs.search"])}),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who}");
    }
    // Each refusal is in the audit log as `denied`, `role`.
    let events = w.tools.events();
    assert!(!events.is_empty());
    assert!(events
        .iter()
        .all(|e| e.body["outcome"] == "denied" && e.body["detail"] == "role"));
    assert!(events
        .iter()
        .all(|e| !e.body.to_string().contains("secret-roadmap")));
    assert!(w.tools.opened().is_empty());
}

#[tokio::test]
async fn an_admins_sensitive_reads_are_recorded_before_the_text_is_served() {
    let w = web().await;
    let adam = w.login("adam").await;
    // Validating the deletion of the restricted file: allowed, and recorded first.
    let r = w
        .post(
            &adam,
            "/api/admin/content/validate",
            json!({"changes": [{"path": RESTRICTED, "delete": true}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["content_changes"][0]["diff"]
        .as_str()
        .unwrap()
        .contains(RESTRICTED_BODY));
    let e = w.tools.events();
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].query, "user=adam");
    assert_eq!(e[0].body["action"], "admin.content_sensitive_read");
    assert_eq!(e[0].body["outcome"], "ok");
    assert_eq!(e[0].body["detail"], "restricted context");
    assert_eq!(e[0].body["count"], 1);
    // The restricted file's name is never sent: it can itself be sensitive.
    assert_eq!(e[0].body["target"], Value::Null);
    assert!(!e[0].body.to_string().contains("secret-roadmap"));

    // A PM's own master file, opened: the target says whose.
    let r = w
        .get(&adam, &format!("/api/admin/content/file?path={ANA_MASTER}"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let e = w.tools.events();
    assert_eq!(e.len(), 2);
    assert_eq!(e[1].body["detail"], "personal file");
    assert_eq!(e[1].body["target"], "content:personal:ana");
    assert!(!e[1].body.to_string().contains(ANA_BODY));

    // A file that isn't sensitive: nothing recorded.
    let r = w
        .get(
            &adam,
            "/api/admin/content/file?path=content/skills/docs/SKILL.md",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(w.tools.events().len(), 2);

    // The audit log down: the text isn't served.
    w.tools.events_down.store(true, Ordering::SeqCst);
    for r in [
        w.get(&adam, &format!("/api/admin/content/file?path={RESTRICTED}"))
            .await,
        w.post(
            &adam,
            "/api/admin/content/validate",
            json!({"changes": [{"path": ANA_MASTER, "text": "x"}]}),
        )
        .await,
    ] {
        assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.body);
        assert_eq!(r.code(), "audit_unavailable");
        let text = r.body.to_string();
        assert!(!text.contains(RESTRICTED_BODY) && !text.contains(ANA_BODY));
    }
}

#[tokio::test]
async fn an_auditor_never_sees_lint_messages_of_files_they_cant_open() {
    let w = web().await;
    // A restricted file with a warning that quotes its front matter (its review date).
    std::fs::write(
        w.content().join("context/products/secret-roadmap.md"),
        "---\ntitle: Secret roadmap\nowner: \"adam\"\nlast-reviewed: 2001-02-03\nsensitivity: restricted\n---\n\nRestricted body.\n",
    )
    .unwrap();
    let issues = |r: &Reply| -> Vec<Value> {
        r.body["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == RESTRICTED)
            .unwrap()["lint"]["issues"]
            .as_array()
            .unwrap()
            .clone()
    };
    let adam = w.login("adam").await;
    let seen = issues(&w.get(&adam, "/api/admin/content").await);
    assert!(
        seen.iter()
            .any(|i| i["message"].as_str().unwrap().contains("2001-02-03")),
        "{seen:?}"
    );
    let audrey = w.login("audrey").await;
    let r = w.get(&audrey, "/api/admin/content").await;
    let hidden = issues(&r);
    assert_eq!(hidden.len(), seen.len());
    for i in &hidden {
        assert_eq!(i["message"], "A finding on a file your role can't open.");
        assert_eq!(i["code"], "context-stale", "the code stays");
    }
    assert!(!serde_json::to_string(&hidden)
        .unwrap()
        .contains("2001-02-03"));
}

// Policy: reading.

#[tokio::test]
async fn policy_read_shows_the_spec_the_files_drift_and_allowances() {
    let w = web().await;
    let adam = w.login("adam").await;
    let r = w.get(&adam, "/api/admin/policy").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let b = &r.body;
    assert_eq!(b["spec"]["bots"]["docs"]["role"], "docs-helper");
    assert_eq!(b["spec"]["bots"]["docs"]["scope"], "team");
    assert_eq!(b["spec"]["roles"]["docs-helper"]["skills"], json!(["docs"]));
    assert_eq!(
        b["spec"]["skills"]["docs"]["tools"],
        json!(["docs.search", "docs.read"])
    );
    assert_eq!(b["drift"]["in_sync"], true, "{}", b["drift"]);
    assert_eq!(b["drift"]["checked_in_available"], true);
    let docs = &b["bots"][0];
    assert_eq!(docs["bot"], "docs");
    assert_eq!(docs["policy_file"], "policies/bots/docs.yaml");
    let net = docs["allowances"]["network"].as_array().unwrap();
    let hosts: Vec<&str> = net.iter().map(|n| n["host"].as_str().unwrap()).collect();
    assert_eq!(
        hosts,
        [
            "gateway.pmos.internal",
            "bots.pmos.internal",
            "toolserver.pmos.internal"
        ]
    );
    let ts = net.iter().find(|n| n["service"] == "toolserver").unwrap();
    assert_eq!(ts["mcp_tools"], json!(["docs_read", "docs_search"]));
    assert!(docs["allowances"]["filesystem"]["read_only"]
        .as_array()
        .unwrap()
        .contains(&json!("/sandbox/skills/docs")));
    assert!(!docs["allowances"]["summary"].as_array().unwrap().is_empty());
    let f = b["files"].as_array().unwrap();
    assert!(f
        .iter()
        .any(|f| f["path"] == "policies/bots/docs.yaml" && f["state"] == "same"));
    assert!(f[0]["generated"].is_string() && f[0]["checked_in"].is_string());
    assert_eq!(b["fixed"]["services"].as_array().unwrap().len(), 3);
    // Each name only at its own pinned address (decision O6), in the fixed facts and in what the
    // checked-in file allows.
    let pins: Vec<(&str, &Value)> = b["fixed"]["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["host"].as_str().unwrap(), &s["allowed_ips"]))
        .collect();
    assert_eq!(
        pins,
        [
            ("toolserver.pmos.internal", &json!(["10.201.77.10/32"])),
            ("gateway.pmos.internal", &json!(["10.201.77.11/32"])),
            ("bots.pmos.internal", &json!(["10.201.77.12/32"])),
        ]
    );
    assert!(net.iter().all(|n| n["pinned"] == true), "{net:?}");
    assert_eq!(ts["allowed_ips"], json!(["10.201.77.10/32"]));
    assert_eq!(b["proposals_configured"], true);
    // Branch protection, as the tool server read it with adam's own account.
    assert_eq!(b["protection"]["required_reviews"], 1);
    assert_eq!(b["protection"]["verified"], true);
    assert_eq!(b["protection"]["branch"], "main");
    let p = w.tools.to("/internal/admin/protection");
    assert_eq!(p[0].query, "user=adam");
    assert!(p[0].assertion.is_some());

    // A hand edit that adds egress: drift, and the rule it breaks.
    let p = w.dir.path().join("policies/bots/docs.yaml");
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(
        &p,
        text.replace("host: gateway.pmos.internal", "host: slack.com"),
    )
    .unwrap();
    let r = w.get(&adam, "/api/admin/policy").await;
    assert_eq!(r.body["drift"]["in_sync"], false);
    let f = r.body["files"].as_array().unwrap();
    assert!(f
        .iter()
        .any(|f| f["path"] == "policies/bots/docs.yaml" && f["state"] == "differs"));
    let rules: Vec<&str> = r.body["drift"]["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["code"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"net-egress"), "{rules:?}");
    // What's checked in is what the screen says the sandbox may do.
    let net = r.body["bots"][0]["allowances"]["network"]
        .as_array()
        .unwrap()
        .clone();
    assert!(net
        .iter()
        .any(|n| n["host"] == "slack.com" && n["service"].is_null()));
}

// Policy: validating.

#[tokio::test]
async fn policy_validation_marks_widening_and_narrowing() {
    let w = web().await;
    let adam = w.login("adam").await;
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search", "docs.read", "jira.search"])}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["valid"], true, "{}", r.body["errors"]);
    assert_eq!(r.body["widening"], true);
    let changes = r.body["allowance_changes"].as_array().unwrap();
    let tool = changes
        .iter()
        .find(|c| c["kind"] == "network.mcp_tool" && c["item"] == "jira_search")
        .unwrap_or_else(|| panic!("{changes:?}"));
    assert_eq!(tool["direction"], "wider");
    assert_eq!(tool["widening"], true);
    assert_eq!(tool["bot"], "docs");
    assert!(r.body["summary"]["wider"].as_u64().unwrap() >= 1);
    let content = r.body["content_changes"].as_array().unwrap();
    assert_eq!(content[0]["path"], "content/skills/docs/SKILL.md");
    assert!(content[0]["diff"]
        .as_str()
        .unwrap()
        .contains("+  tools: \"docs.search docs.read jira.search\""));
    let policy = r.body["policy_changes"].as_array().unwrap();
    assert_eq!(policy[0]["path"], "policies/bots/docs.yaml");
    assert!(
        policy[0]["diff"]
            .as_str()
            .unwrap()
            .contains("+                  - jira_search"),
        "{}",
        policy[0]["diff"]
    );
    // Nothing was written.
    assert!(!w.read("skills/docs/SKILL.md").contains("jira.search"));

    // Narrower.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search"])}),
        )
        .await;
    assert_eq!(r.body["valid"], true);
    assert_eq!(r.body["widening"], false, "{}", r.body["allowance_changes"]);
    assert!(r.body["allowance_changes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["direction"] == "narrower"));

    // The same spec: no changes at all.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search", "docs.read"])}),
        )
        .await;
    assert_eq!(r.body["valid"], true);
    assert!(r.body["policy_changes"].as_array().unwrap().is_empty());
    assert!(r.body["allowance_changes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn policy_validation_errors_have_paths() {
    let w = web().await;
    let adam = w.login("adam").await;
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": {"bots": {"nope": {"role": "x"}, "docs": {"scope": "galaxy"}},
                            "skills": {"docs": {"tools": ["docs.search", "docs.search", "Bad Tool"]}}}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["valid"], false);
    let paths: Vec<&str> = r.body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    for p in [
        "spec.bots.nope",
        "spec.bots.docs.scope",
        "spec.skills.docs.tools[1]",
        "spec.skills.docs.tools[2]",
    ] {
        assert!(paths.contains(&p), "{p}: {paths:?}");
    }
    // A role naming a skill that doesn't exist: the content lint's error, at its file.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": {"roles": {"docs-helper": {"skills": ["docs", "no-such-skill"]}}}}),
        )
        .await;
    assert_eq!(r.body["valid"], false);
    let e = r.body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "role-skill-unknown")
        .unwrap_or_else(|| panic!("{}", r.body));
    assert_eq!(e["path"], "content/roles/docs-helper/ROLE.md");
    // A tool that isn't in the registry.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search", "web.fetch"])}),
        )
        .await;
    assert_eq!(r.body["valid"], false);
    assert!(r.body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["code"] == "skill-unknown-tool"));
    // Unknown fields are refused outright.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": {"bots": {"docs": {"hosts": ["slack.com"]}}}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(w.tools.calls().is_empty(), "validating sends nothing");
    // A proposal that doesn't validate: 422, never sent to the tool server, and recorded.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search", "web.fetch"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.code(), "invalid");
    assert!(!r.body["errors"].as_array().unwrap().is_empty());
    assert!(w.tools.opened().is_empty());
    let e = w.tools.events();
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].body["action"], "admin.policy_proposal");
    assert_eq!(e[0].body["outcome"], "denied");
    assert_eq!(e[0].body["detail"], "invalid");
    // A spec error is recorded the same way.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(json!({"bots": {"nope": {"role": "x"}}})),
        )
        .await;
    assert_eq!(r.code(), "invalid");
    assert_eq!(w.tools.events().len(), 2);
}

// Proposals: what goes to the tool server, and what comes back.

#[tokio::test]
async fn a_policy_proposal_goes_to_the_tool_server_as_a_checked_change_set() {
    let w = web().await;
    let adam = w.login("adam").await;
    let tools = ["docs.search", "docs.read", "jira.search"];
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&tools)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["number"], 42);
    assert_eq!(r.body["author"], "adam-gh", "opened as the admin");
    assert_eq!(r.body["base"]["commit"], BASE);

    let open = w.tools.opened();
    assert_eq!(open.len(), 1);
    let c = &open[0];
    assert_eq!(c.method, "POST");
    assert_eq!(c.query, "user=adam");
    assert!(
        c.assertion.is_some(),
        "signed for the admin, bound to the body"
    );
    assert_eq!(
        c.auth.as_deref(),
        Some(format!("Bearer {TOOLS_TOKEN}").as_str())
    );
    assert_eq!(c.body["kind"], "policy");
    assert_eq!(c.body["title"], "Let the Docs bot search Jira");
    assert_eq!(c.body["reason"], "PMs asked for it @everyone ``` fence");
    let changes = c.body["changes"].as_array().unwrap();
    let paths: Vec<&str> = changes
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"]
    );
    // Each file with the blob id it was computed from: the deployed copy's.
    let blobs = blobs_of(w.dir.path());
    for f in changes {
        let path = f["path"].as_str().unwrap();
        assert_eq!(f["base_blob_sha"], blobs[path], "{path}");
    }
    assert!(changes[0]["text"]
        .as_str()
        .unwrap()
        .contains("tools: \"docs.search docs.read jira.search\""));
    assert!(changes[1]["text"]
        .as_str()
        .unwrap()
        .contains("- jira_search"));
    // Nothing on disk changed, and pmos-web reached no GitHub.
    assert!(!w.read("skills/docs/SKILL.md").contains("jira.search"));

    // Listed through the tool server, with adam's own account.
    let r = w.get(&adam, "/api/admin/policy/proposals").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["github"], "linked");
    assert_eq!(r.body["repo"], "acme/pmos");
    let list = r.body["proposals"].as_array().unwrap();
    assert_eq!(list[0]["proposed_by"], "adam");
    let l = w.tools.to("/internal/admin/proposals");
    assert_eq!(l[0].query, "user=adam&kind=policy");
    assert!(l[0].assertion.is_some());

    // The same spec as deployed: nothing to propose, nothing sent.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search", "docs.read"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.code(), "no_change");
    // No title or reason.
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            json!({"spec": tools_spec(&tools), "title": " \u{7} ", "reason": "x"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(w.tools.opened().len(), 1);
}

#[tokio::test]
async fn a_content_proposal_is_sent_and_logs_hold_no_file_text() {
    let w = web().await;
    let adam = w.login("adam").await;
    let opened = w
        .get(
            &adam,
            "/api/admin/content/file?path=content/playbooks/bar.md",
        )
        .await;
    assert_eq!(opened.status, StatusCode::OK);
    let sha = opened.body["sha256"].as_str().unwrap().to_string();
    let text = opened.body["text"]
        .as_str()
        .unwrap()
        .replace("# The bar", &format!("# The bar\n\n{SECRET_TEXT}"));
    let r = w
        .post(
            &adam,
            "/api/admin/content/proposals",
            json!({
                "changes": [
                    {"path": "content/playbooks/bar.md", "text": text, "base_sha256": sha},
                    {"path": "content/playbooks/new-one.md", "text": format!("---\ntitle: New\nowner: \"adam\"\nversion: \"0.1.0\"\n---\n\n{SECRET_TEXT}\n")},
                ],
                "title": "Sharpen the bar",
                "reason": "Reviewers asked.",
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let c = &w.tools.opened()[0];
    assert_eq!(c.body["kind"], "content");
    let changes = c.body["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1]["path"], "content/playbooks/new-one.md");
    assert_eq!(changes[1]["base_blob_sha"], Value::Null, "a new file");
    assert!(
        w.tools.events().is_empty(),
        "nothing refused, nothing sensitive"
    );

    let r = w.get(&adam, "/api/admin/content/proposals").await;
    assert_eq!(r.body["proposals"].as_array().unwrap().len(), 1);

    // Logs: the handle and the outcome, never the file's text or a token.
    let logs = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
    assert!(logs.contains("proposal sent to the tool server"));
    assert!(!logs.contains(SECRET_TEXT), "no file text in the logs");
    assert!(!logs.contains(TOOLS_TOKEN));
}

#[tokio::test]
async fn the_tool_servers_refusals_are_relayed_with_pmos_webs_own_words() {
    let w = web().await;
    let adam = w.login("adam").await;
    let propose = || {
        let w = &w;
        let adam = adam.clone();
        async move {
            w.post(
                &adam,
                "/api/admin/policy/proposals",
                proposal(tools_spec(&["docs.search"])),
            )
            .await
        }
    };
    // Not linked: a plain sentence saying how to link.
    w.tools.not_linked.store(true, Ordering::SeqCst);
    let r = propose().await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.code(), "github_not_linked");
    assert!(r.body["error"]
        .as_str()
        .unwrap()
        .contains("Link your GitHub"));
    // ... and the list is an explanatory empty state, not an error.
    let r = w.get(&adam, "/api/admin/policy/proposals").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["github"], "not_linked");
    assert_eq!(r.body["proposals"], json!([]));
    assert!(r.body["message"]
        .as_str()
        .unwrap()
        .contains("Link your GitHub"));
    let r = w.get(&adam, "/api/admin/policy").await;
    assert_eq!(r.body["protection"]["verified"], false);
    assert_eq!(r.body["protection"]["github"], "not_linked");
    w.tools.not_linked.store(false, Ordering::SeqCst);

    // A stale base: the paths and the base come through.
    w.tools.reply(
        409,
        json!({"code": "stale_base", "error": "x", "paths": ["content/skills/docs/SKILL.md"],
               "base": {"branch": "main", "commit": BASE}, "junk": "not relayed"}),
    );
    let r = propose().await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "stale_base");
    assert_eq!(r.body["paths"], json!(["content/skills/docs/SKILL.md"]));
    assert_eq!(r.body["base"]["commit"], BASE);
    assert!(r.body.get("junk").is_none());
    assert_ne!(r.body["error"], "x", "pmos-web's own sentence");

    // The rate limit, with Retry-After.
    w.tools.reply(
        429,
        json!({"code": "rate_limited", "error": "x", "retry_after": 1200}),
    );
    let r = propose().await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.code(), "rate_limited");
    assert_eq!(r.headers["retry-after"], "1200");

    // The audit log down at the tool server: nothing was opened.
    w.tools
        .reply(502, json!({"code": "audit_unavailable", "error": "x"}));
    let r = propose().await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.code(), "audit_unavailable");

    // Its own re-validation found errors.
    w.tools.reply(
        422,
        json!({"code": "invalid", "error": "x", "errors": [{"severity": "error", "code": "net-egress", "path": "policies/bots/docs.yaml", "line": null, "message": "m"}], "warnings": []}),
    );
    let r = propose().await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["errors"][0]["code"], "net-egress");

    // A code pmos-web doesn't know, or a bad assertion: an upstream failure, never its text.
    w.tools.reply(
        401,
        json!({"code": "bad_assertion", "error": "secret detail"}),
    );
    let r = propose().await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.code(), "upstream_unavailable");
    assert!(!r.body.to_string().contains("secret detail"));

    // Proposals off at the tool server: reads and validation still work.
    *w.tools.open_reply.lock().unwrap() = None;
    w.tools.unconfigured.store(true, Ordering::SeqCst);
    let r = propose().await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(r.code(), "not_configured");
    let r = w.get(&adam, "/api/admin/content/proposals").await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(r.code(), "not_configured");
    let r = w.get(&adam, "/api/admin/policy").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["proposals_configured"], false);
    let r = w.get(&adam, "/api/admin/content").await;
    assert_eq!(r.body["proposals_configured"], false);
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search"])}),
        )
        .await;
    assert_eq!(r.body["valid"], true);
}

#[tokio::test]
async fn a_stale_copy_is_refused_here_and_recorded() {
    let w = web().await;
    let adam = w.login("adam").await;
    // A content change made from a copy the server has since redeployed.
    for route in [
        "/api/admin/content/validate",
        "/api/admin/content/proposals",
    ] {
        let r = w
            .post(
                &adam,
                route,
                json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x", "base_sha256": "00"}],
                       "title": "t", "reason": "r"}),
            )
            .await;
        // Validate takes no title: its body is refused first.
        if route.ends_with("validate") {
            assert_eq!(r.status, StatusCode::BAD_REQUEST);
            let r = w
                .post(
                    &adam,
                    route,
                    json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x", "base_sha256": "00"}]}),
                )
                .await;
            assert_eq!(r.status, StatusCode::CONFLICT);
            assert_eq!(r.code(), "stale_base");
        } else {
            assert_eq!(r.status, StatusCode::CONFLICT);
            assert_eq!(r.code(), "stale_base");
        }
    }
    let e = w.tools.events();
    assert_eq!(e.len(), 2, "{e:?}");
    assert_eq!(e[0].body["action"], "admin.content_validate");
    assert_eq!(e[1].body["action"], "admin.content_proposal");
    assert!(e
        .iter()
        .all(|e| e.body["detail"] == "stale base" && e.body["count"] == 1));
    assert!(w.tools.opened().is_empty());
}

// Content: reading.

#[tokio::test]
async fn the_content_tree_has_metadata_and_lint_status() {
    let w = web().await;
    let audrey = w.login("audrey").await;
    let r = w.get(&audrey, "/api/admin/content").await;
    assert_eq!(r.status, StatusCode::OK);
    let files = r.body["files"].as_array().unwrap();
    let skill = files
        .iter()
        .find(|f| f["path"] == "content/skills/docs/SKILL.md")
        .unwrap();
    assert_eq!(skill["kind"], "skill");
    assert_eq!(skill["name"], "docs");
    assert_eq!(skill["version"], "0.1.0");
    assert_eq!(
        skill["lint"]["status"], "warning",
        "owner TODO: {}",
        skill["lint"]
    );
    assert!(skill["size"].as_u64().unwrap() > 0);
    assert_eq!(skill["editable"], true);
    // No team file in the content since slice U7 (a proposal may still never write one).
    assert!(!files.iter().any(|f| f["path"] == "content/team.yaml"));
    let secret = files.iter().find(|f| f["path"] == RESTRICTED).unwrap();
    assert_eq!(secret["sensitivity"], "restricted");
    assert_eq!(secret["readable"], false, "an auditor doesn't open it");
    let skills = r.body["skills"].as_array().unwrap();
    let docs = skills.iter().find(|s| s["name"] == "docs").unwrap();
    assert_eq!(docs["used_by"], json!(["docs-helper"]));
    let roles = r.body["roles"].as_array().unwrap();
    let helper = roles.iter().find(|r| r["name"] == "docs-helper").unwrap();
    assert_eq!(helper["bots"], json!(["docs"]));
    assert_eq!(r.body["lint"]["errors"], 0);
    assert_eq!(r.body["protection"]["verified"], true);
    let adam = w.login("adam").await;
    let r = w.get(&adam, "/api/admin/content").await;
    let secret = r.body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == RESTRICTED)
        .unwrap()
        .clone();
    assert_eq!(secret["readable"], true);
}

/// UM3 (review of `b469d5b`): an auditor's tree shows a closed file's path, kind,
/// sensitivity, size and lint status, never its title, name, owner, version or review date;
/// `Restricted` in any case is closed, and so is front matter that doesn't parse (the rule
/// fails closed); findings on no listed file are redacted for an auditor too. The file route
/// agrees.
#[tokio::test]
async fn an_auditors_tree_shows_no_metadata_of_a_closed_file_and_fails_closed() {
    let w = web().await;
    let products = w.dir.path().join("content/context/products");
    std::fs::write(
        products.join("shouty.md"),
        "---\ntitle: Shouty secret\nowner: \"adam\"\nversion: 2\nsensitivity: Restricted\n---\n\nShouty body.\n",
    )
    .unwrap();
    std::fs::write(
        products.join("broken.md"),
        "---\ntitle: Broken secret\nsensitivity: restricted\nowner: [adam\n---\n\nBroken body.\n",
    )
    .unwrap();
    let audrey = w.login("audrey").await;
    let r = w.get(&audrey, "/api/admin/content").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let files = r.body["files"].as_array().unwrap();
    let entry = |p: &str| {
        files
            .iter()
            .find(|f| f["path"] == p)
            .unwrap_or_else(|| panic!("{p} listed"))
            .clone()
    };
    for path in [
        RESTRICTED,
        "content/context/products/shouty.md",
        "content/context/products/broken.md",
    ] {
        let e = entry(path);
        assert_eq!(e["readable"], false, "{path}");
        for field in ["title", "name", "owner", "version", "last_reviewed"] {
            assert_eq!(e[field], Value::Null, "{path}: {field}");
        }
        for issue in e["lint"]["issues"].as_array().unwrap() {
            assert_eq!(
                issue["message"], "A finding on a file your role can't open.",
                "{path}"
            );
        }
        let r = w
            .get(&audrey, &format!("/api/admin/content/file?path={path}"))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
    let text = r.body.to_string();
    for secret in ["Secret roadmap", "Shouty secret", "Broken secret"] {
        assert!(!text.contains(secret), "{secret} shown to an auditor");
    }
    for issue in r.body["lint"]["other"].as_array().unwrap() {
        assert_eq!(
            issue["message"],
            "A finding on a file your role can't open."
        );
    }
    // A skill stays readable, with its metadata.
    let skill = entry("content/skills/docs/SKILL.md");
    assert_eq!(
        (skill["readable"].clone(), skill["name"].clone()),
        (json!(true), json!("docs"))
    );
    // An admin sees everything.
    let adam = w.login("adam").await;
    let r = w.get(&adam, "/api/admin/content").await;
    let secret = r.body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == RESTRICTED)
        .unwrap()
        .clone();
    assert_eq!(secret["title"], "Secret roadmap");
}

#[tokio::test]
async fn file_reads_are_confined_to_the_content_tree() {
    let w = web().await;
    let adam = w.login("adam").await;
    let r = w
        .get(
            &adam,
            "/api/admin/content/file?path=content/skills/docs/SKILL.md",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["front_matter"]["name"], "docs");
    assert_eq!(
        r.body["front_matter"]["metadata"]["tools"],
        "docs.search docs.read"
    );
    assert_eq!(r.body["kind"], "skill");
    assert_eq!(r.body["text"], w.read("skills/docs/SKILL.md"));
    assert_eq!(r.body["sha256"].as_str().unwrap().len(), 64);

    // Plant things to escape through: a symlink to a file outside, a symlinked folder, and a
    // file beside the content tree.
    std::fs::write(w.dir.path().join("outside.md"), "OUTSIDE").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            w.dir.path().join("outside.md"),
            w.content().join("playbooks/link.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(w.dir.path(), w.content().join("context/escape")).unwrap();
        std::os::unix::fs::symlink("/etc", w.content().join("templates/etc")).unwrap();
    }
    for (path, want) in [
        ("content/../outside.md", StatusCode::BAD_REQUEST),
        ("content/%2e%2e/outside.md", StatusCode::BAD_REQUEST),
        ("content/skills/../../outside.md", StatusCode::BAD_REQUEST),
        ("../outside.md", StatusCode::BAD_REQUEST),
        ("/etc/passwd", StatusCode::BAD_REQUEST),
        ("content//etc/passwd", StatusCode::BAD_REQUEST),
        ("content/.git/config", StatusCode::BAD_REQUEST),
        ("policies/bots/docs.yaml", StatusCode::BAD_REQUEST),
        ("deploy/compose.yaml", StatusCode::BAD_REQUEST),
        ("content\\..\\outside.md", StatusCode::BAD_REQUEST),
        (
            "content/skills/docs/SKILL.md%00.txt",
            StatusCode::BAD_REQUEST,
        ),
        ("", StatusCode::BAD_REQUEST),
        ("content/playbooks/link.md", StatusCode::BAD_REQUEST),
        ("content/context/escape/outside.md", StatusCode::BAD_REQUEST),
        ("content/templates/etc/passwd", StatusCode::BAD_REQUEST),
        ("content/skills/docs", StatusCode::BAD_REQUEST),
        ("content/skills/nope.md", StatusCode::NOT_FOUND),
    ] {
        let r = w
            .get(&adam, &format!("/api/admin/content/file?path={path}"))
            .await;
        assert_eq!(r.status, want, "{path}: {}", r.body);
        let text = r.body.to_string();
        assert!(
            !text.contains("OUTSIDE") && !text.contains("root:"),
            "{path}"
        );
    }
    // The tree lists no symlink.
    let r = w.get(&adam, "/api/admin/content").await;
    let paths: Vec<&str> = r.body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(!paths
        .iter()
        .any(|p| p.contains("link.md") || p.contains("escape") || p.contains("etc")));

    // An auditor reads skills, not restricted context or a PM's own master file.
    let audrey = w.login("audrey").await;
    let r = w
        .get(
            &audrey,
            "/api/admin/content/file?path=content/skills/docs/SKILL.md",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    for path in [RESTRICTED, ANA_MASTER] {
        let r = w
            .get(&audrey, &format!("/api/admin/content/file?path={path}"))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert!(
            !r.body.to_string().contains(RESTRICTED_BODY) && !r.body.to_string().contains(ANA_BODY)
        );
        let r = w
            .get(&adam, &format!("/api/admin/content/file?path={path}"))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
    }
}

// Content: validating.

#[tokio::test]
async fn content_validation_runs_the_lint_and_enforces_the_caps() {
    let w = web().await;
    let adam = w.login("adam").await;
    let validate = |changes: Value| {
        let w = &w;
        let adam = adam.clone();
        async move {
            w.post(
                &adam,
                "/api/admin/content/validate",
                json!({ "changes": changes }),
            )
            .await
        }
    };
    // Broken front matter: an error at its file, with a line.
    let broken = "---\nname: docs\ndescription: d\nmetadata:\n  owner: [unclosed\n---\n\nBody\n";
    let r = validate(json!([{"path": "content/skills/docs/SKILL.md", "text": broken}])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["valid"], false);
    let errors = r.body["errors"].as_array().unwrap();
    let e = errors
        .iter()
        .find(|e| e["path"] == "content/skills/docs/SKILL.md")
        .unwrap_or_else(|| panic!("{errors:?}"));
    assert!(e["line"].as_u64().is_some(), "{e}");
    // The role that used the skill now fails too.
    assert!(errors
        .iter()
        .any(|e| e["path"] == "content/roles/docs-helper/ROLE.md"));
    assert_eq!(r.body["content_changes"][0]["change"], "modified");

    // Deleting the skill: the role and so the bot's policy can't stand.
    let r = validate(json!([{"path": "content/skills/docs/SKILL.md", "delete": true}])).await;
    assert_eq!(r.body["valid"], false);
    assert_eq!(r.body["content_changes"][0]["change"], "deleted");

    // A good change: a new playbook, valid, no policy change.
    let r = validate(json!([{"path": "content/playbooks/new.md", "text": "---\ntitle: New\nowner: \"adam\"\nversion: \"0.1.0\"\n---\n\nBody\n"}])).await;
    assert_eq!(r.body["valid"], true, "{}", r.body);
    assert_eq!(r.body["content_changes"][0]["change"], "added");
    assert!(r.body["policy_changes"].as_array().unwrap().is_empty());

    // Changing a skill's tools from the content screen is a policy change too.
    let skill = w
        .read("skills/docs/SKILL.md")
        .replace("docs.search docs.read", "docs.search docs.read jira.search");
    let r = validate(json!([{"path": "content/skills/docs/SKILL.md", "text": skill}])).await;
    assert_eq!(r.body["valid"], true, "{}", r.body);
    assert_eq!(r.body["widening"], true);
    assert_eq!(
        r.body["policy_changes"][0]["path"],
        "policies/bots/docs.yaml"
    );

    // Caps and refusals: nothing is linted.
    let big = "a".repeat(256 * 1024 + 1);
    for (changes, status, code) in [
        (
            json!([{"path": "content/playbooks/x.md", "text": big}]),
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
        ),
        (
            json!([{"path": "content/playbooks/x.md", "text": "a\u{0}b"}]),
            StatusCode::BAD_REQUEST,
            "not_text",
        ),
        (
            json!([{"path": "content/playbooks/x.png", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "not_text",
        ),
        (
            json!([{"path": "content/skills/docs/run.sh", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "not_text",
        ),
        (
            json!([{"path": "content/team.yaml", "text": "members: []"}]),
            StatusCode::BAD_REQUEST,
            "path_not_allowed",
        ),
        (
            json!([{"path": ".github/workflows/ci.yml", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "path_not_allowed",
        ),
        (
            json!([{"path": "deploy/compose.yaml", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "path_not_allowed",
        ),
        (
            json!([{"path": "crates/web/src/app.rs", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "path_not_allowed",
        ),
        (
            json!([{"path": "policies/bots/docs.yaml", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "path_not_allowed",
        ),
        (
            json!([{"path": "content/../deploy/x.yaml", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "bad_path",
        ),
        (
            json!([{"path": "content/.github/x.md", "text": "x"}]),
            StatusCode::BAD_REQUEST,
            "bad_path",
        ),
        (
            json!([{"path": "content/playbooks/bar.md", "text": "x", "delete": true}]),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!([{"path": "content/playbooks/bar.md"}]),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!([{"path": "content/playbooks/a.md", "text": "x"}, {"path": "content/playbooks/a.md", "text": "y"}]),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!([{"path": "content/playbooks/nope.md", "delete": true}]),
            StatusCode::BAD_REQUEST,
            "not_found",
        ),
        (json!([]), StatusCode::BAD_REQUEST, "no_change"),
        (
            json!([{"path": "content/playbooks/bar.md", "text": w.read("playbooks/bar.md")}]),
            StatusCode::BAD_REQUEST,
            "no_change",
        ),
    ] {
        let r = validate(changes.clone()).await;
        assert_eq!(
            (r.status, r.code()),
            (status, code),
            "{changes:.80}: {}",
            r.body
        );
    }
    let many: Vec<Value> = (0..51)
        .map(|i| json!({"path": format!("content/playbooks/p{i}.md"), "text": "x"}))
        .collect();
    let r = validate(json!(many)).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::PAYLOAD_TOO_LARGE, "too_many_files")
    );

    // A symlink inside the content tree can't be written through.
    #[cfg(unix)]
    {
        std::fs::create_dir_all(w.dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(
            w.dir.path().join("elsewhere"),
            w.content().join("playbooks/linked"),
        )
        .unwrap();
        let r = validate(json!([{"path": "content/playbooks/linked/x.md", "text": "x"}])).await;
        assert_eq!((r.status, r.code()), (StatusCode::BAD_REQUEST, "bad_path"));
        assert!(std::fs::read_dir(w.dir.path().join("elsewhere"))
            .unwrap()
            .next()
            .is_none());
    }
    // Nothing on disk changed, and nothing was sent anywhere.
    assert!(!w.content().join("playbooks/new.md").exists());
    assert!(w.tools.calls().is_empty());
}

// The harness.

#[tokio::test]
async fn the_harness_view_maps_bots_to_gateway_models() {
    let w = web().await;
    let audrey = w.login("audrey").await;
    let r = w.get(&audrey, "/api/admin/harness").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let docs = &r.body["bots"][0];
    assert_eq!(docs["name"], "docs");
    assert_eq!(docs["role"], "docs-helper");
    assert_eq!(docs["model_role"], "default");
    assert_eq!(docs["gateway_alias"], "pmos/default");
    assert_eq!(docs["gateway_model"], "qwen3.6-35b-a3b");
    assert_eq!(docs["tools"], json!(["docs.read", "docs.search"]));
    assert_eq!(docs["loaded_by_runtime"], true);
    assert_eq!(r.body["gateway_config"], "ok");
    assert_eq!(r.body["runtime"]["reachable"], true);
    let roles = r.body["model_roles"].as_array().unwrap();
    assert_eq!(roles.len(), 3);
    let text = r.body.to_string();
    assert!(!text.contains("vllm-default"), "never an upstream address");
    assert!(r.body["content_revision"].get("commit").is_some());
}

#[test]
fn the_fixture_writes_go_nowhere_else() {
    // Guard for the tests above: the fixture is a copy, so the repository's own content is
    // untouched.
    let before = std::fs::read_to_string(repo_root().join("content/skills/docs/SKILL.md")).unwrap();
    let dir = fixture();
    std::fs::write(dir.path().join("content/skills/docs/SKILL.md"), "x").unwrap();
    let after = std::fs::read_to_string(repo_root().join("content/skills/docs/SKILL.md")).unwrap();
    assert_eq!(before, after);
    let _ = std::io::stdout().flush();
}
