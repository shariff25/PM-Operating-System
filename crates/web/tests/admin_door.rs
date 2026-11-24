//! The admin door (slice H10, `docs/openshell-any-host-spec.md` §3.8, "The admin listener's
//! sign-in", r7's X6) against a stand-in tool server, served as `main` serves it: with each
//! connection's peer address. Both listeners of one `pmos-web`: `/api/admin/*` only on the admin
//! door; the `Host` check; no CORS; the tunnel and proxy modes and their peers; the forwarded
//! client's range check and D2 (a)'s allow-list; the role refused before any session or cookie
//! exists and re-checked on every request; each door's own cookie and sessions; step-up for the
//! sensitive actions; the admin door's sign-in and setup routes. Fake data only.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use pmos_web::config::{AdminDoor, AdminMode, Cidr};
use pmos_web::session::{MemorySessionStore, SessionStore};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

mod support;
use support::{door, FakeDirectory, FakeUser};

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
const RIGHT: &str = "the right passphrase";
const CODE: &str = "123456";
const EPOCH: i64 = 4;
const PUBLIC_URL: &str = "https://pmos.example.com";

/// The pending token the stand-in hands out for `handle` (43 characters, as a real one).
fn pending_for(handle: &str) -> String {
    let mut t = format!("pending-{handle}-");
    while t.len() < 43 {
        t.push('p');
    }
    t
}

fn setup_for(handle: &str) -> String {
    let mut t = format!("setup-{handle}-");
    while t.len() < 43 {
        t.push('s');
    }
    t
}

#[derive(Default)]
struct Tools {
    directory: FakeDirectory,
    /// Every call that wasn't a user-database lookup: its path and body.
    calls: Mutex<Vec<(String, Value)>>,
}

impl Tools {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|(p, _)| p == "/internal/admin/events")
            .map(|(_, b)| b)
            .collect()
    }
}

fn coded(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"code": code, "error": code}))).into_response()
}

fn user_json(d: &FakeDirectory, handle: &str) -> Value {
    let u = d.get(handle).unwrap();
    json!({
        "handle": u.handle, "display_name": u.display_name, "status": u.status,
        "roles": u.roles, "entra": "none", "sign_in_name_confirmed": true,
        "dual_role": false, "password_epoch": EPOCH,
    })
}

async fn toolserver(
    State(t): State<Arc<Tools>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOOLS_TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(res) = t.directory.handle(&method, &uri, &body) {
        // The real resolve answers pmos-web with the password epoch; the shared fake doesn't.
        let q = uri.query().unwrap_or_default().to_string();
        if res.status() == StatusCode::OK && q.contains("by=handle") {
            let handle = q
                .split('&')
                .find_map(|p| p.strip_prefix("handle="))
                .unwrap_or_default()
                .to_string();
            return Json(user_json(&t.directory, &handle)).into_response();
        }
        return res;
    }
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    t.calls.lock().unwrap().push((path.clone(), b.clone()));
    let handle_of = |name: &str| name.trim_end_matches("@example.com").to_string();
    match path.as_str() {
        "/internal/admin/events" => Json(json!({"recorded": true})).into_response(),
        "/internal/password/sign-in" => {
            let name = b["sign_in_name"].as_str().unwrap_or_default();
            if b["password"] == RIGHT && t.directory.get(&handle_of(name)).is_some() {
                Json(json!({"pending_token": pending_for(&handle_of(name)),
                            "expires_at": "2026-11-24T09:05:00Z"}))
                .into_response()
            } else {
                coded(StatusCode::UNAUTHORIZED, "sign_in_failed")
            }
        }
        "/internal/password/second-step" => {
            let token = b["pending_token"].as_str().unwrap_or_default();
            let Some(handle) = ["ada", "audrey", "ana"]
                .into_iter()
                .find(|h| pending_for(h) == token)
            else {
                return coded(StatusCode::UNAUTHORIZED, "sign_in_expired");
            };
            if b["code"] != CODE {
                return coded(StatusCode::UNAUTHORIZED, "code_failed");
            }
            Json(
                json!({"user": user_json(&t.directory, handle), "password_epoch": EPOCH,
                        "recovery_code_used": false, "recovery_codes_left": 10}),
            )
            .into_response()
        }
        "/internal/password/setup/begin" => {
            let name = b["sign_in_name"].as_str().unwrap_or_default();
            let handle = handle_of(name);
            if b["setup_code"] == "host-code" && t.directory.get(&handle).is_some() {
                Json(json!({"setup_token": setup_for(&handle), "handle": handle,
                            "kind": "setup", "expires_at": "2026-11-24T09:15:00Z"}))
                .into_response()
            } else {
                coded(StatusCode::UNAUTHORIZED, "setup_failed")
            }
        }
        "/internal/password/setup/password" => {
            Json(json!({"next": "authenticator"})).into_response()
        }
        _ => Json(json!({})).into_response(),
    }
}

async fn serve(app: Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await
        .unwrap();
    });
    (base, cancel)
}

struct Web {
    /// The admin door, at its `*.localhost` name.
    base: String,
    http: reqwest::Client,
    /// The public door.
    public: String,
    tools: Arc<Tools>,
    sessions: Arc<MemorySessionStore>,
    _dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
    text: String,
}

async fn reply(res: reqwest::Response) -> Reply {
    let status = res.status();
    let headers = res.headers().clone();
    let text = res.text().await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

/// `pmos-web` with both doors; `change` adjusts the admin door's settings.
async fn web_with(change: impl FnOnce(&mut AdminDoor, &mut Config)) -> Web {
    let tools = Arc::new(Tools::default());
    for (h, roles) in [
        ("ada", vec!["admin"]),
        ("audrey", vec!["auditor"]),
        ("ana", vec!["pm"]),
    ] {
        let mut u = FakeUser::new(h, &roles);
        u.sign_in_name = Some(format!("{h}@example.com"));
        u.confirmed = true;
        tools.directory.add(u);
    }
    let (tools_url, stop_tools) =
        serve(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let l = door::listener().await;
    let mut admin = door::tunnel(l.port);
    let mut cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url: "http://127.0.0.1:1".into(),
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url,
        toolserver_token: pmos_web::upstream::Credentials::new(TOOLS_TOKEN),
        content_root: dir.path().to_path_buf(),
        static_dir: None,
        public_url: Some(PUBLIC_URL.into()),
        dev_login: false,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        password_login: true,
        trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
        admin: None,
        password_clients: Vec::new(),
    };
    change(&mut admin, &mut cfg);
    cfg.admin = Some(admin);
    let sessions = Arc::new(MemorySessionStore::new());
    let deps = Deps {
        sessions: sessions.clone(),
        ..Deps::in_memory()
    };
    let server = pmos_web::app::build_server(cfg, deps).unwrap();
    let (public, stop_public) = serve(server.public).await;
    let port = l.port;
    let stop_admin = door::serve(l, server.admin.unwrap());
    Web {
        base: door::base(port),
        http: door::client(port),
        public,
        tools,
        sessions,
        _dir: dir,
        _stop: vec![stop_tools, stop_public, stop_admin],
    }
}

async fn web() -> Web {
    web_with(|_, _| {}).await
}

impl Web {
    fn store(&self) -> Arc<dyn SessionStore> {
        self.sessions.clone()
    }

    async fn get(&self, cookie: Option<&str>, path: &str) -> Reply {
        let mut req = self.http.get(format!("{}{path}", self.base));
        if let Some(c) = cookie {
            req = req.header("cookie", format!("__Host-pmos-admin={c}"));
        }
        reply(req.send().await.unwrap()).await
    }

    /// A write as the admin screens send it: the door's origin, the session's CSRF token, JSON.
    async fn write(
        &self,
        session: Option<&(String, String)>,
        method: Method,
        path: &str,
        body: Value,
    ) -> Reply {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("origin", self.base.clone())
            .json(&body);
        if let Some((cookie, csrf)) = session {
            req = req
                .header("cookie", format!("__Host-pmos-admin={cookie}"))
                .header("x-pmos-csrf", csrf);
        }
        reply(req.send().await.unwrap()).await
    }

    /// Password sign-in on the admin door: the second step's answer and the cookies it set.
    async fn password_sign_in(&self, handle: &str, cookies: &str) -> (Reply, Reply) {
        let first = reply(
            self.http
                .post(format!("{}/api/auth/password/sign-in", self.base))
                .header("origin", self.base.clone())
                .header("cookie", cookies)
                .json(&json!({"sign_in_name": format!("{handle}@example.com"), "password": RIGHT}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        let pending = set_cookie(&first, "__Host-pmos-admin-pending").unwrap_or_default();
        let second = reply(
            self.http
                .post(format!("{}/api/auth/password/second-step", self.base))
                .header("origin", self.base.clone())
                .header("cookie", format!("__Host-pmos-admin-pending={pending}"))
                .json(&json!({"code": CODE}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        (first, second)
    }
}

/// The value a `Set-Cookie` for `name` carries, if one was set.
fn set_cookie(r: &Reply, name: &str) -> Option<String> {
    r.headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix(&format!("{name}=")))
        .map(|c| c.split(';').next().unwrap().to_string())
        .filter(|v| !v.is_empty())
}

fn set_cookie_line(r: &Reply, name: &str) -> Option<String> {
    r.headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with(&format!("{name}=")))
        .map(String::from)
}

/// Every admin-door route this slice serves, with a body it takes.
fn admin_routes() -> Vec<(Method, &'static str)> {
    vec![
        (Method::GET, "/api/admin/audit"),
        (Method::GET, "/api/admin/audit/export"),
        (Method::GET, "/api/admin/team"),
        (Method::GET, "/api/admin/connectors"),
        (Method::GET, "/api/admin/overview"),
        (Method::GET, "/api/admin/bots"),
        (Method::POST, "/api/admin/bots/drafts/apr_1/release"),
        (Method::GET, "/api/admin/users"),
        (Method::POST, "/api/admin/users"),
        (Method::PUT, "/api/admin/users/ana/roles"),
        (Method::POST, "/api/admin/users/ana/deactivate"),
        (Method::DELETE, "/api/admin/users/ana/identities/entra"),
        (Method::POST, "/api/admin/users/ana/password-setup-code"),
        (Method::GET, "/api/admin/policy"),
        (Method::POST, "/api/admin/policy/proposals"),
        (Method::GET, "/api/admin/content"),
        (Method::POST, "/api/admin/content/proposals"),
        (Method::GET, "/api/admin/harness"),
    ]
}

#[tokio::test]
async fn admin_routes_answer_404_on_the_public_door_and_are_served_on_the_admin_door() {
    let w = web().await;
    let http = reqwest::Client::new();
    for (method, path) in admin_routes() {
        let r = reply(
            http.request(method.clone(), format!("{}{path}", w.public))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "public {path}");
        assert_eq!(r.body["code"], "not_found", "public {path}");
        // On the admin door they are routes: signed out without a session.
        let r = reply(
            w.http
                .request(method, format!("{}{path}", w.base))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "admin {path}: {}",
            r.text
        );
    }
    // The admin door serves only its own: no PM route, no development sign-in, no password
    // change or recovery codes, no health or internal route.
    for path in [
        "/api/drafts",
        "/api/tasks",
        "/api/brief",
        "/api/reviews",
        "/api/bots",
        "/healthz",
        "/readyz",
    ] {
        let r = w.get(None, path).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.text);
    }
    for path in [
        "/api/auth/dev-login",
        "/api/auth/password/change",
        "/api/auth/password/recovery-codes",
        "/api/auth/password/reset-notice/dismiss",
    ] {
        let r = w.write(None, Method::POST, path, json!({})).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        w.get(None, "/api/auth/password/status").await.status,
        StatusCode::NOT_FOUND
    );
    // Its providers: password only (no Entra redirect URI registered for it, the default).
    let r = w.get(None, "/api/auth/providers").await;
    assert_eq!(r.body["providers"], json!(["password"]));
}

#[tokio::test]
async fn a_host_other_than_the_admin_urls_authority_is_404_before_any_route() {
    let w = web().await;
    let port = w.base.rsplit(':').next().unwrap().to_string();
    let raw = reqwest::Client::new();
    for host in [
        "evil.example".to_string(),
        format!("evil.example:{port}"),
        format!("pmos-test.localhost:{}", port.parse::<u16>().unwrap() + 1),
        "pmos-test.localhost".to_string(),
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
    ] {
        for (method, path) in [
            (Method::GET, "/api/auth/providers"),
            (Method::POST, "/api/auth/password/sign-in"),
            (Method::GET, "/api/admin/audit"),
            (Method::GET, "/"),
        ] {
            let r = reply(
                raw.request(method, format!("http://127.0.0.1:{port}{path}"))
                    .header("host", host.clone())
                    .header("origin", w.base.clone())
                    .json(&json!({"sign_in_name": "ada@example.com", "password": RIGHT}))
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{host} {path}");
        }
    }
    // Not one reached the tool server: sign-in included.
    assert!(w.tools.calls().is_empty(), "{:?}", w.tools.calls());
    // The host compares without case.
    let r = reply(
        raw.get(format!("http://127.0.0.1:{port}/api/auth/providers"))
            .header("host", format!("PMOS-Test.LocalHost:{port}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // Counted on the public door's /readyz.
    let ready: Value = reqwest::get(format!("{}/readyz", w.public))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["admin_bad_host"], 24);
}

#[tokio::test]
async fn a_request_without_host_is_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let w = web().await;
    let port: u16 = w.base.rsplit(':').next().unwrap().parse().unwrap();
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    s.write_all(b"GET /api/auth/providers HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.0 404"), "{out}");
}

#[tokio::test]
async fn the_admin_door_never_sends_a_cors_header() {
    let w = web().await;
    let (cookie, csrf) = door::session(&w.store(), "ada").await;
    let mut paths: Vec<(Method, &str)> = admin_routes();
    paths.extend([
        (Method::GET, "/api/me"),
        (Method::GET, "/api/auth/providers"),
        (Method::POST, "/api/auth/password/sign-in"),
        (Method::GET, "/"),
    ]);
    for (method, path) in paths {
        for m in [method, Method::OPTIONS] {
            let r = reply(
                w.http
                    .request(m.clone(), format!("{}{path}", w.base))
                    .header("origin", "https://evil.example")
                    .header("access-control-request-method", "POST")
                    .header("cookie", format!("__Host-pmos-admin={cookie}"))
                    .header("x-pmos-csrf", csrf.clone())
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            let cors: Vec<_> = r
                .headers
                .keys()
                .filter(|k| k.as_str().starts_with("access-control-"))
                .collect();
            assert!(cors.is_empty(), "{m} {path}: {cors:?}");
        }
    }
}

#[tokio::test]
async fn tunnel_mode_takes_only_the_tunnel_peer_and_never_reads_x_forwarded_for() {
    // Another peer than the one connecting (127.0.0.1): every request 404, sign-in included.
    let w = web_with(|a, _| {
        a.mode = AdminMode::Tunnel {
            peer: "10.201.77.1".parse().unwrap(),
        }
    })
    .await;
    for path in ["/api/auth/providers", "/api/admin/audit", "/"] {
        assert_eq!(
            w.get(None, path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let (first, _) = w.password_sign_in("ada", "").await;
    assert_eq!(first.status, StatusCode::NOT_FOUND);
    assert!(w.tools.calls().is_empty());
    let ready: Value = reqwest::get(format!("{}/readyz", w.public))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(ready["admin_refused_peer"].as_u64().unwrap() >= 4);
    // The tunnel peer itself, with an X-Forwarded-For naming a public address: served, and the
    // password attempt is counted under the peer (the header never read).
    let w = web().await;
    let r = reply(
        w.http
            .post(format!("{}/api/auth/password/sign-in", w.base))
            .header("origin", w.base.clone())
            .header("x-forwarded-for", "203.0.113.9")
            .json(&json!({"sign_in_name": "ada@example.com", "password": RIGHT}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let sent = w
        .tools
        .calls()
        .into_iter()
        .find(|(p, _)| p == "/internal/password/sign-in")
        .unwrap();
    assert_eq!(sent.1["source"], "127.0.0.1");
}

/// Proxy mode: only the trusted terminator, naming a private client; D2 (a)'s allow-list on the
/// forwarded client's address.
#[tokio::test]
async fn proxy_mode_checks_the_peer_and_the_forwarded_client() {
    let w = web_with(|a, cfg| {
        a.url = format!(
            "https://pmos-admin.corp.example:{}",
            a.authority.rsplit(':').next().unwrap()
        );
        a.authority = a.url.trim_start_matches("https://").to_string();
        a.mode = AdminMode::Proxy {
            trusted: vec!["127.0.0.1".parse().unwrap()],
        };
        cfg.password_clients = vec![Cidr::parse("10.1.0.0/16").unwrap()];
    })
    .await;
    let port = w.base.rsplit(':').next().unwrap().to_string();
    let host = format!("pmos-admin.corp.example:{port}");
    let raw = reqwest::Client::new();
    let send = |xff: Option<&'static str>, path: &'static str| {
        let mut req = raw
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("host", host.clone())
            .header("origin", format!("https://{host}"))
            .json(&json!({"sign_in_name": "ada@example.com", "password": RIGHT}));
        if let Some(x) = xff {
            req = req.header("x-forwarded-for", x);
        }
        async move { reply(req.send().await.unwrap()).await }
    };
    // No client named, or a public one: 404, before any route.
    for xff in [
        None,
        Some("203.0.113.9"),
        Some("10.1.2.3, 203.0.113.9"),
        Some("garbage"),
    ] {
        let r = send(xff, "/api/auth/password/sign-in").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{xff:?}");
    }
    assert!(w.tools.calls().is_empty());
    // A private client outside the allow-list: 403 password_sign_in_not_from_here, before the
    // tool server is asked.
    let r = send(Some("10.2.0.1"), "/api/auth/password/sign-in").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "password_sign_in_not_from_here");
    assert!(w.tools.calls().is_empty());
    // Inside it: through to the tool server, counted under the forwarded address.
    let r = send(Some("10.1.0.5"), "/api/auth/password/sign-in").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(w.tools.calls()[0].1["source"], "10.1.0.5");
}

#[tokio::test]
async fn a_pm_with_the_right_password_and_code_gets_no_session_on_the_admin_door() {
    let w = web().await;
    let (first, second) = w.password_sign_in("ana", "").await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(second.status, StatusCode::FORBIDDEN, "{}", second.text);
    assert_eq!(second.body["code"], "admin_door_not_allowed");
    assert!(
        second.body["error"].as_str().unwrap().contains(PUBLIC_URL),
        "says where to go"
    );
    assert!(set_cookie(&second, "__Host-pmos-admin").is_none());
    assert!(set_cookie(&second, "__Host-pmos").is_none());
    let cleared = set_cookie_line(&second, "__Host-pmos-admin-pending").unwrap();
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert!(w.sessions.is_empty(), "no session row");
    // Audited, with the reason.
    let refused: Vec<Value> = w
        .tools
        .events()
        .into_iter()
        .filter(|e| e["action"] == "web.sign_in_refused")
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["reason"], "admin_door_not_allowed");
    assert_eq!(refused[0]["provider"], "password");
}

#[tokio::test]
async fn an_admin_signs_in_with_a_strict_cookie_of_the_admin_door_only() {
    let w = web().await;
    let (first, second) = w.password_sign_in("ada", "").await;
    let pending = set_cookie_line(&first, "__Host-pmos-admin-pending").unwrap();
    assert!(pending.contains("SameSite=Strict") && pending.contains("Secure"));
    assert_eq!(second.status, StatusCode::OK, "{}", second.text);
    let line = set_cookie_line(&second, "__Host-pmos-admin").unwrap();
    assert!(
        line.contains("Path=/; Secure; HttpOnly; SameSite=Strict"),
        "{line}"
    );
    assert!(
        set_cookie(&second, "__Host-pmos").is_none(),
        "never the public cookie"
    );
    let cookie = set_cookie(&second, "__Host-pmos-admin").unwrap();
    let me = w.get(Some(&cookie), "/api/me").await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text);
    assert_eq!(me.body["door"], "admin");
    assert_eq!(me.body["public_url"], PUBLIC_URL);
    // The admin door's session is no use on the public door, under either cookie name.
    for name in ["__Host-pmos", "__Host-pmos-admin"] {
        let r = reply(
            reqwest::Client::new()
                .get(format!("{}/api/me", w.public))
                .header("cookie", format!("{name}={cookie}"))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{name}");
    }
}

#[tokio::test]
async fn a_public_door_session_is_no_use_on_the_admin_door() {
    let w = web().await;
    // A public-door session for an Admin, presented on the admin door under its own cookie
    // name and under the admin door's.
    let id = pmos_web::session::random_token();
    w.sessions
        .insert(
            pmos_web::session::hash_id(&id),
            pmos_web::session::Session {
                handle: "ada".into(),
                csrf_token: pmos_web::session::random_token(),
                created_at: Utc::now(),
                last_seen: Utc::now(),
                entra_oid: None,
                entra_tid: None,
                password_epoch: None,
                door: pmos_web::session::Door::Public,
            },
        )
        .await
        .unwrap();
    let r = reply(
        w.http
            .get(format!("{}/api/admin/audit", w.base))
            .header("cookie", format!("__Host-pmos={id}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w.get(Some(&id), "/api/admin/audit").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["code"], "signed_out");
}

#[tokio::test]
async fn a_role_removed_mid_session_ends_it_at_the_next_request() {
    let w = web().await;
    let (cookie, _) = door::session(&w.store(), "audrey").await;
    assert_eq!(w.get(Some(&cookie), "/api/me").await.status, StatusCode::OK);
    w.tools
        .directory
        .update("audrey", |u| u.roles = vec!["pm".into()]);
    // The user database's answer is cached for up to 10 seconds; wait that out.
    tokio::time::sleep(std::time::Duration::from_millis(10_500)).await;
    let r = w.get(Some(&cookie), "/api/me").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "admin_door_not_allowed");
    assert!(w.sessions.is_empty(), "the session ended");
}

#[tokio::test]
async fn sensitive_actions_need_a_sign_in_from_the_last_five_minutes() {
    let w = web().await;
    let sensitive = [
        (Method::POST, "/api/admin/users", json!({})),
        (Method::PUT, "/api/admin/users/ana/roles", json!({})),
        (Method::POST, "/api/admin/users/ana/deactivate", json!({})),
        (
            Method::DELETE,
            "/api/admin/users/ana/identities/entra",
            json!({}),
        ),
        (
            Method::POST,
            "/api/admin/users/ana/password-setup-code",
            json!({}),
        ),
        (Method::POST, "/api/admin/policy/proposals", json!({})),
        (Method::POST, "/api/admin/content/proposals", json!({})),
        (Method::GET, "/api/admin/audit/export", Value::Null),
    ];
    let old = door::session_at(&w.store(), "ada", Utc::now() - Duration::minutes(6)).await;
    for (method, path, body) in &sensitive {
        let before = w.tools.calls().len();
        let r = w
            .write(Some(&old), method.clone(), path, body.clone())
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {}", r.text);
        assert_eq!(r.body["code"], "reauth_required", "{path}");
        assert!(
            w.tools.calls()[before..]
                .iter()
                .all(|(p, _)| p == "/internal/admin/events"),
            "{path}: nothing sent"
        );
    }
    // A read on the same session needs no step-up (the stand-in's empty answer is a 502).
    assert_ne!(
        w.get(Some(&old.0), "/api/admin/users").await.status,
        StatusCode::UNAUTHORIZED
    );
    let fresh = door::session(&w.store(), "ada").await;
    for (method, path, body) in &sensitive {
        let r = w
            .write(Some(&fresh), method.clone(), path, body.clone())
            .await;
        assert_ne!(r.body["code"], "reauth_required", "{path}: {}", r.text);
    }
}

#[tokio::test]
async fn writes_on_the_admin_door_must_come_from_its_own_origin() {
    let w = web().await;
    let (cookie, csrf) = door::session(&w.store(), "ada").await;
    for origin in [PUBLIC_URL, "https://evil.example"] {
        let r = reply(
            w.http
                .put(format!("{}/api/admin/users/ana/roles", w.base))
                .header("origin", origin)
                .header("cookie", format!("__Host-pmos-admin={cookie}"))
                .header("x-pmos-csrf", csrf.clone())
                .json(&json!({}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(r.body["code"], "bad_origin", "{origin}");
    }
}

#[tokio::test]
async fn the_admin_doors_setup_routes_serve_admins_and_auditors_only() {
    let w = web().await;
    let begin = |handle: &'static str| {
        let req = w
            .http
            .post(format!("{}/api/auth/password/setup/begin", w.base))
            .header("origin", w.base.clone())
            .json(&json!({"sign_in_name": format!("{handle}@example.com"), "setup_code": "host-code"}));
        async move { reply(req.send().await.unwrap()).await }
    };
    let r = begin("audrey").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let setup = set_cookie(&r, "__Host-pmos-admin-setup").unwrap();
    assert_eq!(setup, setup_for("audrey"));
    // The next step takes the admin door's setup cookie.
    let r = reply(
        w.http
            .post(format!("{}/api/auth/password/setup/password", w.base))
            .header("origin", w.base.clone())
            .header("cookie", format!("__Host-pmos-admin-setup={setup}"))
            .json(&json!({"password": "a long new passphrase"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    // A PM: 404, and no setup cookie.
    let r = begin("ana").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert!(set_cookie(&r, "__Host-pmos-admin-setup").is_none());
}

#[tokio::test]
async fn the_public_door_refuses_password_sign_in_from_outside_the_allow_list() {
    let w = web_with(|_, cfg| {
        cfg.password_clients = vec![Cidr::parse("10.1.0.0/16").unwrap()];
    })
    .await;
    let raw = reqwest::Client::new();
    let send = |xff: &'static str| {
        let req = raw
            .post(format!("{}/api/auth/password/sign-in", w.public))
            .header("origin", PUBLIC_URL)
            .header("x-forwarded-for", xff)
            .json(&json!({"sign_in_name": "ana@example.com", "password": RIGHT}));
        async move { reply(req.send().await.unwrap()).await }
    };
    let r = send("203.0.113.9").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "password_sign_in_not_from_here");
    assert!(
        w.tools.calls().is_empty(),
        "before the tool server is asked"
    );
    let r = send("10.1.4.4").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Entra and the providers list are unaffected.
    let r = reply(
        raw.get(format!("{}/api/auth/providers", w.public))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[test]
fn the_cidr_parser_is_what_the_doors_use() {
    let c = Cidr::parse("10.1.0.0/16").unwrap();
    assert!(c.contains("10.1.200.3".parse::<IpAddr>().unwrap()));
    assert!(c.contains("::ffff:10.1.0.9".parse::<IpAddr>().unwrap()));
    assert!(!c.contains("10.2.0.1".parse::<IpAddr>().unwrap()));
}
