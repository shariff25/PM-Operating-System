//! `pmos-web`'s password sign-in routes against a stand-in tool server, served with peer
//! addresses (as `main` serves them), after the review of 2026-11-20: every refused attempt
//! counts against the address (P1); the address is the real browser's behind a trusted proxy
//! and never a value an untrusted peer forwards (P2, L1); a forwarded header with no trusted
//! proxy is refused; bodies are bounded (L6); a sign-in without an epoch starts no session
//! (L7); a password session fails closed when the user database can't be reached (PM4); the
//! reset notice passes through and is dismissed (PM2). Fake data only.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

mod support;
use support::FakeDirectory;

const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";
const TEAM: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, email: ben@example.com, roles: [pm]}
";
/// The one pending token the stand-in hands out after the right password.
const PENDING: &str = "ppppppppppppppppppppppppppppppppppppppppppp";
const RIGHT: &str = "the right passphrase";
const CODE: &str = "123456";

/// The stand-in tool server: the user database (with Ana's password epoch) and the password
/// routes, recording what reached them.
#[derive(Default)]
struct Tools {
    directory: FakeDirectory,
    /// Every password route call: its path and body.
    calls: Mutex<Vec<(String, Value)>>,
    /// The second step answers without `password_epoch` (L7).
    no_epoch: std::sync::atomic::AtomicBool,
}

const EPOCH: i64 = 3;

fn user_json(d: &FakeDirectory, handle: &str) -> Option<Value> {
    d.get(handle).map(|u| {
        json!({
            "handle": u.handle,
            "display_name": u.display_name,
            "status": u.status,
            "roles": u.roles,
            "entra": "none",
            "sign_in_name_confirmed": u.confirmed,
            "dual_role": false,
            "password_epoch": if handle == "ana" { Some(EPOCH) } else { None },
        })
    })
}

fn coded(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"code": code, "error": code}))).into_response()
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
        let by_handle = uri
            .query()
            .is_some_and(|q| q.contains("by=handle") && q.contains("handle=ana"));
        if res.status() == StatusCode::OK && by_handle {
            return Json(user_json(&t.directory, "ana").unwrap()).into_response();
        }
        return res;
    }
    if uri.path() == "/internal/admin/events" {
        return Json(json!({"recorded": true})).into_response();
    }
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    if path.starts_with("/internal/password/") {
        t.calls.lock().unwrap().push((path.clone(), b.clone()));
    }
    match path.as_str() {
        "/internal/password/sign-in" => {
            if b["sign_in_name"] == "ana@example.com" && b["password"] == RIGHT {
                Json(json!({"pending_token": PENDING, "expires_at": "2026-11-20T09:05:00Z"}))
                    .into_response()
            } else {
                coded(StatusCode::UNAUTHORIZED, "sign_in_failed")
            }
        }
        "/internal/password/second-step" => {
            if b["pending_token"] != PENDING {
                return coded(StatusCode::UNAUTHORIZED, "sign_in_expired");
            }
            if b["code"] != CODE {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"code": "code_failed", "error": "x", "attempts_left": 2})),
                )
                    .into_response();
            }
            let mut out = json!({
                "user": user_json(&t.directory, "ana").unwrap(),
                "password_epoch": EPOCH,
                "recovery_code_used": false,
                "recovery_codes_left": 10,
            });
            if t.no_epoch.load(std::sync::atomic::Ordering::SeqCst) {
                out.as_object_mut().unwrap().remove("password_epoch");
                out["user"]
                    .as_object_mut()
                    .unwrap()
                    .remove("password_epoch");
            }
            Json(out).into_response()
        }
        "/internal/password/status" => Json(json!({
            "enabled": true, "password_set": true, "second_step_enrolled": true,
            "setup_code_pending": false, "recovery_codes_left": 10, "needs_host_setup": false,
            "reset_notice": {"by": "ada", "by_name": "Ada", "at": "2026-11-20T08:00:00Z",
                             "host_user": "never-passed-on"},
        }))
        .into_response(),
        "/internal/password/reset-notice/dismiss" => StatusCode::NO_CONTENT.into_response(),
        _ => coded(StatusCode::NOT_FOUND, "not_found"),
    }
}

async fn serve_plain(app: Router) -> (String, CancellationToken) {
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

/// pmos-web as `main` serves it: with each connection's peer address.
async fn serve_with_peers(app: Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await
        .unwrap();
    });
    (base, cancel)
}

struct Web {
    base: String,
    tools: Arc<Tools>,
    http: reqwest::Client,
    clock: Arc<Mutex<DateTime<Utc>>>,
    _dir: tempfile::TempDir,
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

    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.strip_prefix(&format!("{name}=")))
            .map(|v| v.split(';').next().unwrap_or_default().to_string())
            .find(|v| !v.is_empty())
    }
}

async fn web(trusted: &[&str]) -> Web {
    let tools = Arc::new(Tools::default());
    tools.directory.seed_team(TEAM);
    let (tools_url, stop_tools) =
        serve_plain(Router::new().fallback(toolserver).with_state(tools.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url: "http://127.0.0.1:1".into(),
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_url,
        toolserver_token: pmos_web::upstream::Credentials::new(TOOLS_TOKEN),
        content_root: dir.path().to_path_buf(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        password_login: true,
        trusted_proxies: trusted
            .iter()
            .map(|t| t.parse::<IpAddr>().unwrap())
            .collect(),
    };
    let clock = Arc::new(Mutex::new(
        DateTime::parse_from_rfc3339("2026-11-20T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    let c = clock.clone();
    let deps = Deps {
        clock: Arc::new(move || *c.lock().unwrap()),
        ..Deps::in_memory()
    };
    let (base, stop_web) = serve_with_peers(pmos_web::build(cfg, deps).unwrap()).await;
    Web {
        base,
        tools,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        clock,
        _dir: dir,
        _stop: vec![stop_tools, stop_web],
    }
}

impl Web {
    /// A call from the app's origin, with these extra headers (cookies, forwarded headers).
    async fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, String)],
        body: Option<Value>,
    ) -> Reply {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base));
        if method != Method::GET {
            req = req.header("origin", self.base.clone());
        }
        for (k, v) in headers {
            req = req.header(*k, v);
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

    async fn sign_in(&self, headers: &[(&str, String)], password: &str) -> Reply {
        self.send(
            Method::POST,
            "/api/auth/password/sign-in",
            headers,
            Some(json!({"sign_in_name": "ana@example.com", "password": password})),
        )
        .await
    }

    /// The password and the second step from `headers`: the session cookie.
    async fn full_sign_in(&self, headers: &[(&str, String)]) -> Option<String> {
        let r = self.sign_in(headers, RIGHT).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let pending = r.cookie("__Host-pmos-pending").expect("a pending cookie");
        let mut h = headers.to_vec();
        h.push(("cookie", format!("__Host-pmos-pending={pending}")));
        let r = self
            .send(
                Method::POST,
                "/api/auth/password/second-step",
                &h,
                Some(json!({"code": CODE})),
            )
            .await;
        r.cookie("__Host-pmos")
    }

    fn sources(&self) -> Vec<Value> {
        self.tools
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b["source"].clone())
            .collect()
    }

    fn password_calls(&self) -> usize {
        self.tools.calls.lock().unwrap().len()
    }
}

fn xff(v: &str) -> (&'static str, String) {
    ("x-forwarded-for", v.to_string())
}

/// P1: every refused attempt counts against the address, not only a wrong password: an
/// unknown or expired pending token, a body that isn't the route's JSON, a missing cookie.
/// After twenty the address waits, and the tool server isn't asked again.
#[tokio::test]
async fn every_refused_attempt_counts_against_the_address() {
    let w = web(&[]).await;
    let random = format!("__Host-pmos-pending={}", "r".repeat(43));
    for i in 0..20 {
        let r = match i % 3 {
            0 => {
                w.send(
                    Method::POST,
                    "/api/auth/password/second-step",
                    &[("cookie", random.clone())],
                    Some(json!({"code": "000000"})),
                )
                .await
            }
            1 => {
                w.send(
                    Method::POST,
                    "/api/auth/password/sign-in",
                    &[],
                    Some(json!({"bogus": true})),
                )
                .await
            }
            _ => {
                w.send(
                    Method::POST,
                    "/api/auth/password/setup/password",
                    &[],
                    Some(json!({"password": "whatever it is here"})),
                )
                .await
            }
        };
        assert!(
            r.status.is_client_error() && r.status != StatusCode::TOO_MANY_REQUESTS,
            "{i}: {} {}",
            r.status,
            r.body
        );
    }
    let asked = w.password_calls();
    let r = w.sign_in(&[], RIGHT).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
    );
    assert!(r.headers.get("retry-after").is_some());
    assert_eq!(w.password_calls(), asked, "the tool server wasn't asked");
    // The window passes: the address may try again.
    *w.clock.lock().unwrap() += Duration::minutes(15);
    assert_eq!(w.sign_in(&[], RIGHT).await.status, StatusCode::OK);
}

/// P1 and P2: behind a trusted proxy, two browsers have separate limits. A flood of junk from
/// one doesn't stop the other's sign-in, and the tool server is told each one's address.
#[tokio::test]
async fn behind_a_trusted_proxy_a_flood_from_one_browser_does_not_stop_another() {
    let w = web(&["127.0.0.1"]).await;
    let attacker = [xff("198.51.100.66")];
    let mut limited = false;
    for _ in 0..30 {
        if w.sign_in(&attacker, "a wrong guess here").await.status == StatusCode::TOO_MANY_REQUESTS
        {
            limited = true;
            break;
        }
    }
    assert!(limited, "the attacker's address waits");
    let owner = [xff("203.0.113.10")];
    let session = w.full_sign_in(&owner).await;
    assert!(
        session.is_some(),
        "the owner signed in from another address"
    );
    let sources = w.sources();
    assert!(sources.contains(&json!("198.51.100.66")), "{sources:?}");
    assert!(sources.contains(&json!("203.0.113.10")), "{sources:?}");
    // IPv6 browsers are counted, and told to the tool server, by their /64.
    w.sign_in(&[xff("2001:db8:5:6::1")], "a wrong guess here")
        .await;
    assert!(w.sources().contains(&json!("2001:db8:5:6::/64")));
}

/// P2: a peer that isn't a trusted proxy is counted by its own address, whatever
/// `X-Forwarded-For` it sends.
#[tokio::test]
async fn an_untrusted_peers_forwarded_for_is_ignored() {
    let w = web(&["10.9.9.9"]).await;
    for i in 0..20 {
        let r = w
            .sign_in(&[xff(&format!("192.0.2.{}", i + 1))], "a wrong guess here")
            .await;
        assert_eq!(r.code(), "sign_in_failed");
    }
    let r = w.sign_in(&[xff("192.0.2.200")], RIGHT).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    assert!(w.sources().iter().all(|s| *s == json!("127.0.0.1")));
}

/// With no trusted proxy (`PMOS_WEB_DIRECT=1`), a forwarded header means a proxy nobody named
/// is in front: refused before the tool server is asked.
#[tokio::test]
async fn with_no_trusted_proxy_a_forwarded_header_is_refused() {
    let w = web(&[]).await;
    for h in ["x-forwarded-for", "forwarded", "x-real-ip"] {
        let r = w.sign_in(&[(h, "203.0.113.10".into())], RIGHT).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "proxy_not_trusted"),
            "{h}"
        );
    }
    assert_eq!(w.password_calls(), 0);
    assert_eq!(w.sign_in(&[], RIGHT).await.status, StatusCode::OK);
}

/// L6: the password routes read at most 4 KiB.
#[tokio::test]
async fn a_password_body_over_four_kib_is_refused() {
    let w = web(&[]).await;
    let r = w.sign_in(&[], &"x".repeat(5 * 1024)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.body);
    assert_eq!(w.password_calls(), 0);
}

/// L7: a second step the tool server answers without the credential's epoch starts no
/// session (one would never be checked against a reset).
#[tokio::test]
async fn a_sign_in_without_an_epoch_starts_no_session() {
    let w = web(&[]).await;
    w.tools
        .no_epoch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(w.full_sign_in(&[]).await, None);
}

/// PM4: while the user database can't be reached, a development session may read on its
/// last-known-good answer, but a password session fails closed: a reset can't be outlived.
#[tokio::test]
async fn a_password_session_fails_closed_when_the_user_database_is_down() {
    let w = web(&[]).await;
    let session = w.full_sign_in(&[]).await.expect("signed in");
    let pw = [("cookie", format!("__Host-pmos={session}"))];
    assert_eq!(
        w.send(Method::GET, "/api/me", &pw, None).await.status,
        StatusCode::OK
    );
    let dev = w
        .send(
            Method::POST,
            "/api/auth/dev-login",
            &[],
            Some(json!({"handle": "ben"})),
        )
        .await;
    let dev_cookie = dev.cookie("__Host-pmos").expect("a dev session");
    let dev = [("cookie", format!("__Host-pmos={dev_cookie}"))];
    assert_eq!(
        w.send(Method::GET, "/api/me", &dev, None).await.status,
        StatusCode::OK
    );
    w.tools.directory.set_down(true);
    *w.clock.lock().unwrap() += Duration::seconds(30);
    assert_eq!(
        w.send(Method::GET, "/api/me", &dev, None).await.status,
        StatusCode::OK,
        "a development session reads on its last-known-good answer"
    );
    let r = w.send(Method::GET, "/api/me", &pw, None).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "users_unavailable")
    );
}

/// PM2: the person's status carries the reset notice (re-picked: nothing else passes), and
/// they dismiss it.
#[tokio::test]
async fn the_reset_notice_passes_through_and_is_dismissed() {
    let w = web(&[]).await;
    let session = w.full_sign_in(&[]).await.expect("signed in");
    let c = ("cookie", format!("__Host-pmos={session}"));
    let me = w
        .send(Method::GET, "/api/me", std::slice::from_ref(&c), None)
        .await;
    let csrf = me.body["csrf_token"].as_str().unwrap().to_string();
    let st = w
        .send(
            Method::GET,
            "/api/auth/password/status",
            std::slice::from_ref(&c),
            None,
        )
        .await;
    assert_eq!(
        st.body["reset_notice"],
        json!({"by": "ada", "by_name": "Ada", "at": "2026-11-20T08:00:00Z"})
    );
    assert_eq!(st.body["needs_host_setup"], false);
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/reset-notice/dismiss",
            &[c, ("x-pmos-csrf", csrf)],
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    assert!(w
        .tools
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|(p, _)| p == "/internal/password/reset-notice/dismiss"));
}
