//! The admin Policy Admin and content screens (`/api/admin/policy/*`, `/api/admin/content/*`,
//! `/api/admin/harness`; docs/web-api.md, Admin: policy and Admin: content and harness) against
//! a copy of the real content tree, a mock tool server, a mock runtime and a mock GitHub: the
//! role matrix for every route, validation errors and widening detection, path traversal and
//! symlinks, size and type caps, `501` without GitHub, the pull request's calls (branch,
//! commit, pull request, labels) with the token in the `Authorization` header only, a stale
//! base, audit records, the rate limit, and logs that hold no token and no file text.

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
use pmos_web::admin_config::{AdminConfig, GitHubConfig};
use pmos_web::proposals::git_blob_sha;
use pmos_web::upstream::{Credentials, Signer};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
const GITHUB_TOKEN: &str = "github_pat_TESTONLY_9f8e7d6c5b4a39281706";
// Test key only: fixed bytes, never used anywhere real.
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const BASE: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
/// A marker put in proposed file text: it must never reach a log.
const SECRET_TEXT: &str = "MARKER-proposed-file-text-7c41";

const TEAM: &str = "members:
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

// Mocks.

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
    /// Blob ids on the base, by repository path.
    blobs: Mutex<BTreeMap<String, String>>,
    /// The tool server's audit route fails.
    down: AtomicBool,
    pulls: Mutex<Vec<Value>>,
}

impl Mock {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
    fn writes(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == "POST")
            .collect()
    }
}

async fn github(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let c = call(&method, &uri, &headers, &body);
    m.calls.lock().unwrap().push(c.clone());
    if c.auth.as_deref() != Some(&format!("Bearer {GITHUB_TOKEN}")) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "Bad credentials"})),
        )
            .into_response();
    }
    let p = c.path.strip_prefix("/repos/acme/pmos").unwrap_or("?");
    match (c.method.as_str(), p) {
        ("GET", "") => Json(json!({"default_branch": "main"})).into_response(),
        ("GET", "/git/ref/heads/main") => {
            Json(json!({"object": {"sha": BASE, "type": "commit"}})).into_response()
        }
        ("GET", p) if p == format!("/git/commits/{BASE}") => {
            Json(json!({"sha": BASE, "tree": {"sha": TREE}})).into_response()
        }
        ("GET", p) if p == format!("/git/trees/{TREE}") => {
            let tree: Vec<Value> = m
                .blobs
                .lock()
                .unwrap()
                .iter()
                .map(|(path, sha)| json!({"path": path, "type": "blob", "sha": sha, "mode": "100644"}))
                .collect();
            Json(json!({"sha": TREE, "tree": tree, "truncated": false})).into_response()
        }
        ("POST", "/git/trees") => {
            Json(json!({"sha": "3333333333333333333333333333333333333333"})).into_response()
        }
        ("POST", "/git/commits") => {
            Json(json!({"sha": "4444444444444444444444444444444444444444"})).into_response()
        }
        ("POST", "/git/refs") => {
            (StatusCode::CREATED, Json(json!({"ref": c.body["ref"]}))).into_response()
        }
        ("POST", "/pulls") => {
            let n = 42 + m.pulls.lock().unwrap().len() as u64;
            let pr = json!({
                "number": n,
                "html_url": format!("https://github.example/acme/pmos/pull/{n}"),
                "title": c.body["title"],
                "head": {"ref": c.body["head"]},
                "base": {"ref": c.body["base"]},
                "draft": false,
                "user": {"login": "pmos-bot"},
                "created_at": "2026-11-17T10:00:00Z",
                "updated_at": "2026-11-17T10:00:00Z",
                "labels": [],
                "body": c.body["body"],
            });
            m.pulls.lock().unwrap().push(pr.clone());
            (StatusCode::CREATED, Json(pr)).into_response()
        }
        ("POST", p) if p.starts_with("/issues/") && p.ends_with("/labels") => {
            let n: u64 = p
                .trim_start_matches("/issues/")
                .trim_end_matches("/labels")
                .parse()
                .unwrap();
            for pr in m.pulls.lock().unwrap().iter_mut() {
                if pr["number"] == n {
                    pr["labels"] = json!(c.body["labels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| json!({"name": l}))
                        .collect::<Vec<_>>());
                }
            }
            Json(json!([])).into_response()
        }
        ("GET", "/pulls") => {
            let mut all = m.pulls.lock().unwrap().clone();
            // Someone else's pull request: not a proposal.
            all.push(json!({"number": 7, "html_url": "x", "title": "Fix typo",
                "head": {"ref": "fix-typo"}, "base": {"ref": "main"}, "draft": false,
                "user": {"login": "someone"}, "labels": [], "body": "hi"}));
            all.reverse();
            Json(all).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
    }
}

async fn toolserver(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let c = call(&method, &uri, &headers, &body);
    m.calls.lock().unwrap().push(c.clone());
    if c.auth.as_deref() != Some(&format!("Bearer {TOOLS_TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if m.down.load(Ordering::SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match (c.method.as_str(), c.path.as_str()) {
        ("POST", "/internal/admin/proposals") => Json(json!({"recorded": true})).into_response(),
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
    std::fs::write(content.join("team.yaml"), TEAM).unwrap();
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

struct Web {
    base: String,
    gh: Arc<Mock>,
    tools: Arc<Mock>,
    http: reqwest::Client,
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

struct Opts {
    github: bool,
    per_hour: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            github: true,
            per_hour: 10,
        }
    }
}

async fn web() -> Web {
    web_with(Opts::default()).await
}

async fn web_with(opts: Opts) -> Web {
    logs();
    let gh = Arc::new(Mock::default());
    let tools = Arc::new(Mock::default());
    let (gh_url, stop_gh) = serve(Router::new().fallback(github).with_state(gh.clone())).await;
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let (bots_url, stop_bots) = serve(Router::new().fallback(runtime)).await;
    let dir = fixture();
    *gh.blobs.lock().unwrap() = blobs_of(dir.path());
    let content = dir.path().join("content");
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
            content.clone(),
        )),
        content_root: content.clone(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
    };
    let admin = AdminConfig {
        policies_root: Some(dir.path().join("policies")),
        gateway_config: Some(dir.path().join("gateway.yaml")),
        content_revision: None,
        github: opts.github.then(|| GitHubConfig {
            repo: "acme/pmos".into(),
            token: GITHUB_TOKEN.into(),
            api: gh_url.clone(),
            base: None,
            content_dir: "content".into(),
            policies_dir: "policies".into(),
        }),
        proposals_per_hour: opts.per_hour,
        content_dir: "content".into(),
        policies_dir: "policies".into(),
    };
    let deps = Deps {
        admin,
        ..Deps::in_memory()
    };
    let (base, stop_web) = serve(pmos_web::build(cfg, deps).unwrap()).await;
    Web {
        base,
        gh,
        tools,
        http: reqwest::Client::new(),
        dir,
        _stop: vec![stop_gh, stop_tools, stop_bots, stop_web],
    }
}

/// A signed-in browser: cookie and CSRF token.
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
                .header("cookie", format!("__Host-pmos={}", w.cookie))
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

    async fn login(&self, handle: &str) -> Who {
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
        let cookie = cookie
            .strip_prefix("__Host-pmos=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let me = self
            .http
            .get(format!("{}/api/me", self.base))
            .header("cookie", format!("__Host-pmos={cookie}"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        Who {
            cookie,
            csrf: me["csrf_token"].as_str().unwrap().to_string(),
        }
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
        (
            Method::POST,
            "/api/admin/policy/validate",
            Some(validate_spec.clone()),
            true,
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
            true,
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
    for (who, name) in [
        (&w.login("ana").await, "ana"),
        (&w.login("rex").await, "rex"),
    ] {
        for (method, path, body, _) in &routes {
            let r = w.send(Some(who), method.clone(), path, body.clone()).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{name} {path}");
            assert_eq!(r.code(), "forbidden", "{name} {path}");
        }
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
    assert!(
        w.gh.writes().is_empty(),
        "nobody but an admin reached GitHub writes"
    );
    assert!(w.tools.calls().is_empty(), "nor the audit route");
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
        .header("cookie", format!("__Host-pmos={}", adam.cookie))
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
    let ana = perms(&w.get(&w.login("ana").await, "/api/me").await);
    assert!(!ana
        .iter()
        .any(|p| p.contains("content") || p.contains("polic")));
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
    assert_eq!(b["proposals_configured"], true);

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
    // A proposal that doesn't validate: 422, nothing sent anywhere.
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
    assert!(w.gh.calls().is_empty() && w.tools.calls().is_empty());
}

// Proposals.

#[tokio::test]
async fn a_policy_proposal_opens_a_pull_request() {
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
    assert_eq!(r.body["url"], "https://github.example/acme/pmos/pull/42");
    assert_eq!(r.body["base"]["branch"], "main");
    assert_eq!(r.body["base"]["commit"], BASE);
    assert_eq!(r.body["widening"], true);
    assert_eq!(r.body["stale_check"], "passed");
    let branch = r.body["branch"].as_str().unwrap();
    assert!(branch.starts_with("pmos/proposal/policy/adam-"), "{branch}");
    assert_eq!(
        r.body["files"],
        json!(["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"])
    );

    // GitHub: every call with the token in Authorization only.
    let calls = w.gh.calls();
    for c in &calls {
        assert_eq!(
            c.auth.as_deref(),
            Some(format!("Bearer {GITHUB_TOKEN}").as_str())
        );
        assert!(!c.query.contains(GITHUB_TOKEN) && !c.path.contains(GITHUB_TOKEN));
        assert!(!c.body.to_string().contains(GITHUB_TOKEN));
    }
    let sequence: Vec<String> = calls
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                c.method,
                c.path.trim_start_matches("/repos/acme/pmos")
            )
        })
        .collect();
    assert_eq!(
        sequence,
        [
            "GET ".to_string(),
            "GET /git/ref/heads/main".into(),
            format!("GET /git/commits/{BASE}"),
            format!("GET /git/trees/{TREE}"),
            "POST /git/trees".into(),
            "POST /git/commits".into(),
            "POST /git/refs".into(),
            "POST /pulls".into(),
            "POST /issues/42/labels".into(),
        ]
    );
    let tree = &calls[4].body;
    assert_eq!(tree["base_tree"], TREE);
    let entries = tree["tree"].as_array().unwrap();
    let skill = entries
        .iter()
        .find(|e| e["path"] == "content/skills/docs/SKILL.md")
        .unwrap();
    assert!(skill["content"]
        .as_str()
        .unwrap()
        .contains("tools: \"docs.search docs.read jira.search\""));
    let policy = entries
        .iter()
        .find(|e| e["path"] == "policies/bots/docs.yaml")
        .unwrap();
    assert!(policy["content"]
        .as_str()
        .unwrap()
        .contains("- jira_search"));
    assert_eq!(calls[5].body["parents"], json!([BASE]));
    // A new ref, never an update to an existing one, and never the default branch.
    assert_eq!(calls[6].body["ref"], format!("refs/heads/{branch}"));
    assert!(!calls
        .iter()
        .any(|c| c.method == "PATCH" || c.path.contains("/merge")));
    let pr = &calls[7].body;
    assert_eq!(pr["base"], "main");
    assert_eq!(pr["head"], branch);
    assert_eq!(pr["title"], "PM OS policy: Let the Docs bot search Jira");
    let body = pr["body"].as_str().unwrap();
    assert!(body.contains("by `adam`"), "{body}");
    assert!(
        body.contains("**WIDER**") && body.contains("jira_search"),
        "{body}"
    );
    // The reason is fenced: no mentions, no breaking out of the fence.
    assert!(
        body.contains("```text\nPMs asked for it @everyone ''' fence\n```"),
        "{body}"
    );
    assert!(body.contains(BASE));
    assert_eq!(
        calls[8].body["labels"],
        json!(["pmos-proposal", "pmos-proposal:policy"])
    );
    // Nothing on disk changed.
    assert!(!w.read("skills/docs/SKILL.md").contains("jira.search"));

    // Audit: started, then opened with the pull request, each signed for adam.
    let audit = w.tools.calls();
    assert_eq!(audit.len(), 2, "{audit:?}");
    for (c, stage) in audit.iter().zip(["started", "opened"]) {
        assert_eq!(c.path, "/internal/admin/proposals");
        assert_eq!(c.query, "user=adam");
        assert!(c.assertion.is_some(), "signed for the admin");
        assert_eq!(c.body["kind"], "policy");
        assert_eq!(c.body["stage"], stage);
        assert_eq!(c.body["repo"], "acme/pmos");
        assert_eq!(c.body["base"], BASE);
        assert_eq!(
            c.body["paths"],
            json!(["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"])
        );
        let text = c.body.to_string();
        assert!(
            !text.contains("jira.search") && !text.contains("tools:"),
            "no file text: {text}"
        );
    }
    assert_eq!(audit[0].body["pr"], Value::Null);
    assert_eq!(audit[1].body["pr"], 42);

    // Listed as an open proposal; someone else's pull request isn't.
    let r = w.get(&adam, "/api/admin/policy/proposals").await;
    assert_eq!(r.status, StatusCode::OK);
    let list = r.body["proposals"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["number"], 42);
    assert_eq!(list[0]["proposed_by"], "adam");
    assert_eq!(list[0]["base_commit"], BASE);
    assert_eq!(list[0]["status"], "open");
    let r = w.get(&adam, "/api/admin/content/proposals").await;
    assert!(r.body["proposals"].as_array().unwrap().is_empty());

    // The same spec as deployed: nothing to propose.
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
}

#[tokio::test]
async fn a_content_proposal_opens_a_pull_request_and_logs_hold_no_token_or_text() {
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
    assert_eq!(r.body["widening"], false);
    let branch = r.body["branch"].as_str().unwrap();
    assert!(branch.starts_with("pmos/proposal/content/adam-"));
    assert_eq!(
        r.body["files"],
        json!(["content/playbooks/bar.md", "content/playbooks/new-one.md"])
    );
    let labels = w.gh.calls().last().unwrap().body["labels"].clone();
    assert_eq!(labels, json!(["pmos-proposal", "pmos-proposal:content"]));
    let audit = w.tools.calls();
    assert_eq!(audit[0].body["kind"], "content");
    assert!(!audit
        .iter()
        .any(|c| c.body.to_string().contains(SECRET_TEXT)));

    let r = w.get(&adam, "/api/admin/content/proposals").await;
    assert_eq!(r.body["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["repo"], "acme/pmos");

    // Logs: the handle and the pull request, never the token or the file's text.
    let logs = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
    assert!(logs.contains("proposal opened"), "the proposal is logged");
    assert!(!logs.contains(GITHUB_TOKEN), "no token in the logs");
    assert!(!logs.contains("TESTONLY"), "no token in the logs");
    assert!(!logs.contains(SECRET_TEXT), "no file text in the logs");
    assert!(!logs.contains(TOOLS_TOKEN));
}

#[tokio::test]
async fn a_stale_base_is_refused_before_anything_is_written() {
    let w = web().await;
    let adam = w.login("adam").await;
    // The default branch has moved on: the skill differs there.
    w.gh.blobs.lock().unwrap().insert(
        "content/skills/docs/SKILL.md".into(),
        git_blob_sha(b"someone else's edit"),
    );
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.code(), "stale_base");
    assert_eq!(r.body["paths"], json!(["content/skills/docs/SKILL.md"]));
    assert_eq!(r.body["base"]["commit"], BASE);
    assert!(w.gh.writes().is_empty());
    assert!(
        w.tools.calls().is_empty(),
        "nothing recorded: nothing was attempted"
    );

    // A content change made from a copy the server has since redeployed.
    let r = w
        .post(
            &adam,
            "/api/admin/content/validate",
            json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x", "base_sha256": "00"}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "stale_base");
}

#[tokio::test]
async fn no_audit_record_means_no_pull_request() {
    let w = web().await;
    let adam = w.login("adam").await;
    w.tools.down.store(true, Ordering::SeqCst);
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.code(), "audit_unavailable");
    assert!(w.gh.writes().is_empty(), "nothing written to GitHub");
}

#[tokio::test]
async fn proposals_are_rate_limited_per_admin() {
    let w = web_with(Opts {
        per_hour: 1,
        ..Opts::default()
    })
    .await;
    let adam = w.login("adam").await;
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = w
        .post(
            &adam,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.read"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.code(), "rate_limited");
    assert!(r.headers.get("retry-after").is_some());
}

#[tokio::test]
async fn without_github_proposals_are_not_configured_and_reads_still_work() {
    let w = web_with(Opts {
        github: false,
        ..Opts::default()
    })
    .await;
    let adam = w.login("adam").await;
    for (method, path, body) in [
        (
            Method::POST,
            "/api/admin/policy/proposals",
            Some(proposal(tools_spec(&["docs.search"]))),
        ),
        (Method::GET, "/api/admin/policy/proposals", None),
        (
            Method::POST,
            "/api/admin/content/proposals",
            Some(
                json!({"changes": [{"path": "content/playbooks/bar.md", "text": "x"}], "title": "t", "reason": "r"}),
            ),
        ),
        (Method::GET, "/api/admin/content/proposals", None),
    ] {
        let r = w.send(Some(&adam), method, path, body).await;
        assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED, "{path}");
        assert_eq!(r.code(), "not_configured");
    }
    let r = w.get(&adam, "/api/admin/policy").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["proposals_configured"], false);
    let r = w
        .post(
            &adam,
            "/api/admin/policy/validate",
            json!({"spec": tools_spec(&["docs.search"])}),
        )
        .await;
    assert_eq!(r.body["valid"], true);
    let r = w.get(&adam, "/api/admin/content").await;
    assert_eq!(r.status, StatusCode::OK);
    // An auditor still gets 403 first, not 501.
    let audrey = w.login("audrey").await;
    let r = w
        .post(
            &audrey,
            "/api/admin/policy/proposals",
            proposal(tools_spec(&["docs.search"])),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
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
    let team = files
        .iter()
        .find(|f| f["path"] == "content/team.yaml")
        .unwrap();
    assert_eq!(team["editable"], false);
    let secret = files
        .iter()
        .find(|f| f["path"] == "content/context/products/secret-roadmap.md")
        .unwrap();
    assert_eq!(secret["sensitivity"], "restricted");
    assert_eq!(secret["readable"], false, "an auditor doesn't open it");
    let skills = r.body["skills"].as_array().unwrap();
    let docs = skills.iter().find(|s| s["name"] == "docs").unwrap();
    assert_eq!(docs["used_by"], json!(["docs-helper"]));
    let roles = r.body["roles"].as_array().unwrap();
    let helper = roles.iter().find(|r| r["name"] == "docs-helper").unwrap();
    assert_eq!(helper["bots"], json!(["docs"]));
    assert_eq!(r.body["lint"]["errors"], 0);
    let adam = w.login("adam").await;
    let r = w.get(&adam, "/api/admin/content").await;
    let secret = r.body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "content/context/products/secret-roadmap.md")
        .unwrap()
        .clone();
    assert_eq!(secret["readable"], true);
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
    for path in [
        "content/context/products/secret-roadmap.md",
        "content/master/people/ana/AGENTS.md",
    ] {
        let r = w
            .get(&audrey, &format!("/api/admin/content/file?path={path}"))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert!(
            !r.body.to_string().contains("Restricted body")
                && !r.body.to_string().contains("Ana's own")
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
    assert!(w.gh.calls().is_empty() && w.tools.calls().is_empty());
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
