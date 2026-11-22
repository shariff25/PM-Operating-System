//! `pmos-web`'s password sign-in routes against a stand-in tool server, served with peer
//! addresses (as `main` serves them), after the review of 2026-11-20: every refused attempt
//! counts against the address (P1), and since the review of 8e8f8fe (N2) against what it is
//! about from there too (a sign-in name, a token, a person: 10 each, 100 per address), so a
//! busy office behind one address isn't locked out by a few people's mistakes while guessing
//! at one account and spraying across accounts stay bounded; the address is the real browser's behind a trusted proxy
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
/// A live setup token the stand-in accepts.
const SETUP: &str = "sssssssssssssssssssssssssssssssssssssssssss";
const RIGHT: &str = "the right passphrase";
/// A password the stand-in answers as the tool server does when its shared limit is full.
const SHARED_FULL: &str = "the shared allowance is full";
/// A setup token whose password the stand-in refuses with the shared limit's `429`.
const SETUP_SHARED_FULL: &str = "fffffffffffffffffffffffffffffffffffffffffff";
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

/// The tool server's `429` from a limit it shares (the global allowance, a block's share).
fn too_many_from_tools() -> Response {
    let mut res = coded(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    res.headers_mut()
        .insert("retry-after", axum::http::HeaderValue::from_static("7"));
    res
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
            if b["password"] == SHARED_FULL {
                return too_many_from_tools();
            }
            let name = b["sign_in_name"].as_str().unwrap_or_default();
            // Ana, and the office's people (N2), each with the right password.
            if (name == "ana@example.com" || name.ends_with("@office.example"))
                && b["password"] == RIGHT
            {
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
        "/internal/password/setup/password" => {
            if b["setup_token"] == SETUP_SHARED_FULL {
                return too_many_from_tools();
            }
            if b["setup_token"] == SETUP {
                Json(json!({"next": "authenticator"})).into_response()
            } else {
                coded(StatusCode::UNAUTHORIZED, "setup_expired")
            }
        }
        "/internal/password/recovery-codes" => {
            Json(json!({"recovery_codes": ["aaaa-bbbb-cccc-dddd"]})).into_response()
        }
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
        self.sign_in_as(headers, "ana@example.com", password).await
    }

    async fn sign_in_as(&self, headers: &[(&str, String)], name: &str, password: &str) -> Reply {
        self.send(
            Method::POST,
            "/api/auth/password/sign-in",
            headers,
            Some(json!({"sign_in_name": name, "password": password})),
        )
        .await
    }

    /// The password and the second step from `headers`: the session cookie.
    async fn full_sign_in(&self, headers: &[(&str, String)]) -> Option<String> {
        self.full_sign_in_as(headers, "ana@example.com").await
    }

    /// [`Self::full_sign_in`] as `name`.
    async fn full_sign_in_as(&self, headers: &[(&str, String)], name: &str) -> Option<String> {
        let r = self.sign_in_as(headers, name, RIGHT).await;
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
/// unknown or expired pending or setup token, a wrong password, a body that isn't the route's
/// JSON, a missing cookie. Since N2 (the review of 8e8f8fe) each counts against what it is
/// about from there as well (10 each, and the refusals past them aren't counted against the
/// address), and the address waits after a hundred, about anything, and the tool server isn't
/// asked again. (Before, this test asserted the address waited after twenty refusals of three
/// kinds; it now asserts ten for one subject, ten for requests with nothing readable, and a
/// hundred for the address.)
#[tokio::test]
async fn every_refused_attempt_counts_against_the_address() {
    let w = web(&[]).await;
    // One made-up pending token, ten times: then that token waits from here.
    let random = format!("__Host-pmos-pending={}", "r".repeat(43));
    let second_step = |cookie: String| {
        let w = &w;
        async move {
            w.send(
                Method::POST,
                "/api/auth/password/second-step",
                &[("cookie", cookie)],
                Some(json!({"code": "000000"})),
            )
            .await
        }
    };
    for i in 0..10 {
        let r = second_step(random.clone()).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "sign_in_expired"),
            "{i}"
        );
    }
    let r = second_step(random.clone()).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "that token waits");
    // Nothing readable (a body that isn't the route's JSON, no step cookie): one subject.
    for i in 0..10 {
        let r = if i % 2 == 0 {
            w.send(
                Method::POST,
                "/api/auth/password/sign-in",
                &[],
                Some(json!({"bogus": true})),
            )
            .await
        } else {
            w.send(
                Method::POST,
                "/api/auth/password/setup/password",
                &[],
                Some(json!({"password": "whatever it is here"})),
            )
            .await
        };
        assert!(
            r.status.is_client_error() && r.status != StatusCode::TOO_MANY_REQUESTS,
            "{i}: {} {}",
            r.status,
            r.body
        );
    }
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/confirm",
            &[],
            Some(json!({"code": CODE})),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    // Ana signs in meanwhile (20 refusals so far from this address, none hers).
    assert!(w.full_sign_in(&[]).await.is_some());
    // Eighty more, of every kind, about eighty other subjects: the address's hundred.
    for i in 0..80 {
        let r = match i % 3 {
            0 => second_step(format!("__Host-pmos-pending={:0>43}", i)).await,
            1 => {
                w.sign_in_as(&[], &format!("p{i}@example.com"), "a wrong guess here")
                    .await
            }
            _ => {
                w.send(
                    Method::POST,
                    "/api/auth/password/setup/password",
                    &[("cookie", format!("__Host-pmos-setup={:x>43}", i))],
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

/// N2 (the review of 8e8f8fe): thirty people behind one office address, signing in in the
/// same quarter of an hour. Before, pmos-web counted every wrong password against the address,
/// twenty in 15 minutes for everyone: the office's twenty-first typo locked every password
/// route out for all of them, and only the tool server's own office test covered the case.
/// Now each person mistypes twice (60 refusals, three times what used to lock the office out)
/// and all thirty sign in; three more who keep trying wrong passwords (thirty times each) wait
/// alone, ten refusals each, and nobody else does; and Ana, from the same address, signs in
/// after all of it. (Ninety refusals: the address's hundred takes refusals about ten names at
/// their limit, or a hundred people's mistakes, in 15 minutes.)
#[tokio::test]
async fn a_busy_office_behind_one_address_is_not_locked_out_by_a_few_peoples_mistakes() {
    let w = web(&["127.0.0.1"]).await;
    let office = [xff("203.0.113.10")];
    for n in 0..30 {
        let name = format!("p{n:02}@office.example");
        for typo in 0..2 {
            let r = w.sign_in_as(&office, &name, "a wrong guess here").await;
            assert_eq!(
                (r.status, r.code()),
                (StatusCode::UNAUTHORIZED, "sign_in_failed"),
                "{name}, typo {typo}"
            );
        }
        assert!(
            w.full_sign_in_as(&office, &name).await.is_some(),
            "{name} signs in after two typos"
        );
    }
    // Three people who keep getting it wrong: ten refusals each are counted, then each waits
    // alone (pmos-web's own `429`, the tool server not asked), and the rest of the office
    // doesn't.
    for n in 0..3 {
        let name = format!("stuck{n}@office.example");
        let mut statuses = Vec::new();
        for _ in 0..30 {
            statuses.push(
                w.sign_in_as(&office, &name, "a wrong guess here")
                    .await
                    .status,
            );
        }
        let refused = statuses
            .iter()
            .filter(|s| **s == StatusCode::UNAUTHORIZED)
            .count();
        assert_eq!(refused, 10, "{name}: {statuses:?}");
        let asked = w.password_calls();
        let r = w.sign_in_as(&office, &name, RIGHT).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{name} waits");
        assert_eq!(w.password_calls(), asked, "pmos-web's own wait");
    }
    // 60 + 30 refusals from the address, and still room: the others sign in.
    for n in 0..30 {
        let name = format!("p{n:02}@office.example");
        assert!(w.full_sign_in_as(&office, &name).await.is_some(), "{name}");
    }
    assert!(w.full_sign_in(&office).await.is_some(), "Ana too");
    // A stuck person's wait ends with the window, from the office as from anywhere.
    *w.clock.lock().unwrap() += Duration::minutes(15);
    assert!(w
        .full_sign_in_as(&office, "stuck0@office.example")
        .await
        .is_some());
}

/// N2: the guessing bounds still hold at pmos-web. **One account from one address**: ten
/// refused attempts in 15 minutes, then that name waits there (the tool server isn't asked;
/// it has its own, tighter schedule per account and address behind this); the same name from
/// another address, and another name from this one, are not held up. **Spraying across
/// accounts from one address**: a hundred refused attempts at a hundred names in 15 minutes,
/// then every password route from that address waits, whatever the name, the right password
/// included, until the window moves; another address is not held up. So from one address at
/// most 10 refused attempts per account and 100 in all per 15 minutes reach the tool server
/// (960 and 9,600 a day).
#[tokio::test]
async fn guessing_at_one_account_and_spraying_from_one_address_stay_bounded() {
    let w = web(&["127.0.0.1"]).await;
    let guesser = [xff("198.51.100.66")];
    for i in 0..10 {
        assert_eq!(
            w.sign_in(&guesser, &format!("guess {i} at ana"))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    let asked = w.password_calls();
    for _ in 0..20 {
        let r = w.sign_in(&guesser, "another guess at ana").await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
        );
    }
    // Spelled another way, it is the same name (as the tool server reads it).
    let r = w
        .sign_in_as(&guesser, "  ANA@Example.COM ", "and another")
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        w.password_calls(),
        asked,
        "none of them reached the tool server"
    );
    // A setup's first step at her name waits there too: one count per name.
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/begin",
            &guesser,
            Some(json!({"sign_in_name": "ana@example.com", "setup_code": "AAAAA"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        w.full_sign_in(&[xff("203.0.113.10")]).await.is_some(),
        "Ana, from her own address"
    );

    // Spraying: 90 more names from the same address (ten refusals counted already).
    let sprayer = [xff("192.0.2.77")];
    for n in 0..100 {
        let r = w
            .sign_in_as(&sprayer, &format!("n{n}@example.com"), "Summer2026!!")
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{n}");
    }
    let asked = w.password_calls();
    for name in ["n0@example.com", "fresh@example.com", "ana@example.com"] {
        let r = w.sign_in_as(&sprayer, name, RIGHT).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{name}");
    }
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/begin",
            &sprayer,
            Some(json!({"sign_in_name": "fresh@example.com", "setup_code": "AAAAA"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(w.password_calls(), asked, "the tool server wasn't asked");
    assert!(
        w.full_sign_in(&[xff("192.0.2.78")]).await.is_some(),
        "another address"
    );
    *w.clock.lock().unwrap() += Duration::minutes(15);
    assert!(
        w.full_sign_in(&sprayer).await.is_some(),
        "the window moved on"
    );
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
    assert!(limited, "the attacker waits for that name there");
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

/// M1 (the re-check of eb24d28): with proxies named, a forwarded header from a peer that isn't
/// one (the proxy's address moved, or the setting is wrong) is refused, not silently counted
/// as one address for everyone, and counted on `/readyz`; so is a trusted proxy that names no
/// client. The tool server is never asked, so it never learns a proxy's address as anyone's.
/// A peer not on the list that forwards nothing is still counted by its own address.
#[tokio::test]
async fn a_wrong_or_stale_proxy_setting_is_refused_and_counted() {
    let w = web(&["10.9.9.9"]).await;
    for i in 0..3 {
        let r = w
            .sign_in(&[xff(&format!("192.0.2.{}", i + 1))], RIGHT)
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "proxy_not_trusted"),
            "{}",
            r.body
        );
    }
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/begin",
            &[("x-real-ip", "192.0.2.9".into())],
            Some(json!({"sign_in_name": "ana@example.com", "setup_code": "AAAAA"})),
        )
        .await;
    assert_eq!(r.code(), "proxy_not_trusted");
    assert_eq!(w.password_calls(), 0, "the tool server wasn't asked");
    let ready = w.send(Method::GET, "/readyz", &[], None).await;
    assert_eq!(ready.body["untrusted_proxy_refusals"], 4, "{}", ready.body);
    // GL3 (the gate review of 04c175f): a warning an operator and `pmos-admin status` see,
    // while the refusals are recent. It never makes the server unready by itself (this test
    // serves no bot runtime, so it isn't ready here for that reason alone).
    assert_eq!(
        ready.body["warnings"],
        json!(["untrusted_proxy_refusals"]),
        "{}",
        ready.body
    );
    *w.clock.lock().unwrap() += Duration::minutes(15);
    let ready = w.send(Method::GET, "/readyz", &[], None).await;
    assert_eq!(ready.body["warnings"], json!([]), "15 quiet minutes on");
    assert_eq!(ready.body["untrusted_proxy_refusals"], 4, "the count stays");
    // Nothing forwarded: counted by its own address, as before.
    assert_eq!(w.sign_in(&[], RIGHT).await.status, StatusCode::OK);
    assert_eq!(w.sources(), [json!("127.0.0.1")]);

    // The trusted proxy itself, naming no client: refused too, never counted as the proxy.
    let w = web(&["127.0.0.1"]).await;
    for headers in [
        vec![],
        vec![xff("127.0.0.1")],
        vec![xff("198.51.100.66, junk")],
    ] {
        let r = w.sign_in(&headers, RIGHT).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "proxy_not_trusted"),
            "{headers:?}"
        );
    }
    assert_eq!(w.password_calls(), 0);
    let ready = w.send(Method::GET, "/readyz", &[], None).await;
    assert_eq!(ready.body["untrusted_proxy_refusals"], 3, "{}", ready.body);
    // nginx's appending header, with junk the client sent: the address nginx appended counts.
    let r = w
        .sign_in(&[xff("junk, unknown, 203.0.113.10")], RIGHT)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(w.sources(), [json!("203.0.113.10")]);
}

/// H1 (the re-check of eb24d28): every `setup/password` call hashes at the tool server, so
/// each counts against the caller's address, and its setup token from there, even when it
/// succeeds; after ten for the token (N2; before, twenty for the address) it waits and the tool
/// server isn't asked again. Another browser's sign-in still goes through. LA: the signed-in
/// routes that do work (`recovery-codes`, `change`) count the same way, per person.
#[tokio::test]
async fn setup_passwords_count_against_the_address_even_when_they_succeed() {
    let w = web(&["127.0.0.1"]).await;
    let holder = [
        xff("198.51.100.66"),
        ("cookie", format!("__Host-pmos-setup={SETUP}")),
    ];
    for i in 0..10 {
        let r = w
            .send(
                Method::POST,
                "/api/auth/password/setup/password",
                &holder,
                Some(json!({"password": "a long enough passphrase"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{i}: {}", r.body);
    }
    let asked = w.password_calls();
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/password",
            &holder,
            Some(json!({"password": "a long enough passphrase"})),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
    );
    assert_eq!(w.password_calls(), asked, "the tool server wasn't asked");
    // Another browser signs in meanwhile.
    let owner = [xff("203.0.113.10")];
    let session = w.full_sign_in(&owner).await.expect("signed in");
    // Its recovery codes, ten times: then that person waits there too.
    let me = w
        .send(
            Method::GET,
            "/api/me",
            &[("cookie", format!("__Host-pmos={session}"))],
            None,
        )
        .await;
    let csrf = me.body["csrf_token"].as_str().unwrap().to_string();
    let signed = [
        xff("203.0.113.10"),
        ("cookie", format!("__Host-pmos={session}")),
        ("x-pmos-csrf", csrf),
    ];
    let mut statuses = Vec::new();
    for _ in 0..10 {
        statuses.push(
            w.send(
                Method::POST,
                "/api/auth/password/recovery-codes",
                &signed,
                Some(json!({"code": CODE})),
            )
            .await
            .status,
        );
    }
    // The full sign-in's two steps succeeded (not counted); so ten calls fit, then a wait.
    assert!(
        statuses.iter().all(|s| *s == StatusCode::OK),
        "{statuses:?}"
    );
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/recovery-codes",
            &signed,
            Some(json!({"code": CODE})),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
}

/// M4 and a Low of the review of 8e8f8fe: a `429` from a limit the tool server shares refused
/// the attempt before any check, so it is not a refusal of this address's. Before, each one
/// counted: a 30-person office behind one address, during a minute the tool server was full,
/// used up its twenty refusals and was locked out of password sign-in for 15 minutes; and an
/// invitee's setup password refused that way cost their slot. Now 30 such answers pass through
/// (with the tool server's `Retry-After`) and the office's sign-ins and the invitee's set still
/// go through after them, from the same address.
#[tokio::test]
async fn the_tool_servers_shared_limit_refusals_are_not_counted_against_the_address() {
    let w = web(&["127.0.0.1"]).await;
    let office = [xff("203.0.113.10")];
    for i in 0..30 {
        let r = w.sign_in(&office, SHARED_FULL).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"),
            "{i}"
        );
        assert_eq!(
            r.headers.get("retry-after").and_then(|v| v.to_str().ok()),
            Some("7"),
            "the tool server's own wait, not pmos-web's"
        );
    }
    let asked = w.password_calls();
    for _ in 0..30 {
        assert!(
            w.full_sign_in(&office).await.is_some(),
            "the office signs in"
        );
    }
    assert!(
        w.password_calls() > asked,
        "the tool server was asked every time"
    );
    let invitee = [
        xff("203.0.113.10"),
        ("cookie", format!("__Host-pmos-setup={SETUP_SHARED_FULL}")),
    ];
    for _ in 0..30 {
        let r = w
            .send(
                Method::POST,
                "/api/auth/password/setup/password",
                &invitee,
                Some(json!({"password": "a long enough passphrase"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    }
    let r = w
        .send(
            Method::POST,
            "/api/auth/password/setup/password",
            &[
                xff("203.0.113.10"),
                ("cookie", format!("__Host-pmos-setup={SETUP}")),
            ],
            Some(json!({"password": "a long enough passphrase"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    // Wrong passwords still count, as before: ten at Ana's name, then her name waits there
    // (N2; before, nineteen and the set that hashed, twenty for the address).
    for _ in 0..10 {
        assert_eq!(
            w.sign_in(&office, "a wrong guess here").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let asked = w.password_calls();
    let r = w.sign_in(&office, RIGHT).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(w.password_calls(), asked, "pmos-web's own wait");
    assert!(
        w.full_sign_in_as(&office, "p01@office.example")
            .await
            .is_some(),
        "and only hers"
    );
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
