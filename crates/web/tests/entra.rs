//! Entra ID sign-in against a mock identity provider (discovery, JWKS, token endpoint):
//! the happy path, every ID token check, `state` and PKCE, the user database's sign-in rules
//! (spec §7: `(tid, oid)`, first link by `preferred_username` only, guests, host links for
//! Admins and Auditors, conflicts, deactivation, the tool server unreachable), start-up rules,
//! and that nothing secret reaches the logs.

use pmos_entra::testing::keys;

mod support;
use support::FakeDirectory;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use pmos_web::entra::{challenge, EntraConfig};
use pmos_web::{Config, Deps};
use serde_json::{json, Value};
use sha2::Digest;
use tokio_util::sync::CancellationToken;

const TENANT: &str = "72f988bf-0000-4000-8000-0000000000aa";
const CLIENT: &str = "0b7f0c6e-1111-4222-8333-444455556666";
const SECRET: &str = "entra-client-secret-do-not-log-1234567890";
const BOTS_TOKEN: &str = "web-client-token-0123456789abcdef0123";
const TOOLS_TOKEN: &str = "toolserver-service-token-0123456789ab";

const TEAM: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@contoso.example, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, roles: [pm]}
";

/// The `oid` the harness's tokens carry: Ana's Microsoft account.
const ANA_OID: &str = "00000000-0000-0000-0000-00000000a0a0";

/// The tool server, as far as sign-in reaches it: the user database's routes, and the sign-in
/// events route (each event kept as its URI, JSON body and raw body).
#[derive(Default)]
struct Tools {
    directory: FakeDirectory,
    events: Mutex<Vec<(String, Value, String)>>,
}

async fn tools_mock(
    State(t): State<Arc<Tools>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    if let Some(res) = t.directory.handle(&method, &uri, &body) {
        return res;
    }
    if uri.path() == "/internal/admin/events" {
        t.events.lock().unwrap().push((
            uri.to_string(),
            serde_json::from_slice(&body).unwrap_or(Value::Null),
            String::from_utf8_lossy(&body).into_owned(),
        ));
        return Json(json!({"recorded": true})).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn der(b64_der: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(b64_der)
        .unwrap()
}

fn jwk(kid: &str, n: &str) -> Value {
    json!({"kty": "RSA", "use": "sig", "kid": kid, "n": n, "e": "AQAB"})
}

/// Sign `claims` with RS256 under `kid`.
fn sign(claims: &Value, key_der: &str, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    jsonwebtoken::encode(&header, claims, &EncodingKey::from_rsa_der(&der(key_der))).unwrap()
}

// The mock identity provider.

struct Grant {
    challenge: String,
    redirect_uri: String,
    id_token: String,
}

#[derive(Default)]
struct IdpState {
    base: Mutex<String>,
    keys: Mutex<Vec<Value>>,
    grants: Mutex<HashMap<String, Grant>>,
    discovery_hits: AtomicUsize,
    jwks_hits: AtomicUsize,
    /// Every form the token endpoint got, and every code and token it handed out.
    forms: Mutex<Vec<HashMap<String, String>>>,
    secrets_seen: Mutex<Vec<String>>,
    next: AtomicUsize,
}

async fn discovery(State(idp): State<Arc<IdpState>>, Path(tenant): Path<String>) -> Response {
    idp.discovery_hits.fetch_add(1, Ordering::SeqCst);
    if tenant != TENANT {
        return StatusCode::NOT_FOUND.into_response();
    }
    let base = idp.base.lock().unwrap().clone();
    Json(json!({
        "issuer": format!("{base}/{TENANT}/v2.0"),
        "authorization_endpoint": format!("{base}/{TENANT}/oauth2/v2.0/authorize"),
        "token_endpoint": format!("{base}/{TENANT}/oauth2/v2.0/token"),
        "jwks_uri": format!("{base}/{TENANT}/discovery/v2.0/keys"),
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
    .into_response()
}

async fn jwks(State(idp): State<Arc<IdpState>>) -> Response {
    idp.jwks_hits.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "keys": *idp.keys.lock().unwrap() })).into_response()
}

/// Like Entra: the client secret, the code once, the same redirect URI, and the PKCE
/// verifier whose S256 is the challenge the code was issued for.
async fn token(
    State(idp): State<Arc<IdpState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    idp.forms.lock().unwrap().push(form.clone());
    let bad = |error: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error, "error_description": format!("AADSTS: {error}")})),
        )
            .into_response()
    };
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return bad("unsupported_grant_type");
    }
    if form.get("client_id").map(String::as_str) != Some(CLIENT)
        || form.get("client_secret").map(String::as_str) != Some(SECRET)
    {
        return bad("invalid_client");
    }
    let Some(grant) = form
        .get("code")
        .and_then(|c| idp.grants.lock().unwrap().remove(c))
    else {
        return bad("invalid_grant");
    };
    if form.get("redirect_uri") != Some(&grant.redirect_uri) {
        return bad("invalid_grant");
    }
    match form.get("code_verifier") {
        Some(v) if challenge(v) == grant.challenge => {}
        _ => return bad("invalid_grant"),
    }
    Json(json!({"token_type": "Bearer", "id_token": grant.id_token, "expires_in": 3600}))
        .into_response()
}

async fn serve(listener: tokio::net::TcpListener, app: Router) -> CancellationToken {
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    cancel
}

async fn listener() -> (tokio::net::TcpListener, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    (l, base)
}

// The harness.

struct Harness {
    web: String,
    idp: Arc<IdpState>,
    tools: Arc<Tools>,
    http: reqwest::Client,
    clock: Arc<Mutex<DateTime<Utc>>>,
    _dir: tempfile::TempDir,
    _stop: Vec<CancellationToken>,
}

/// A sign-in started at `/api/auth/login`.
struct Begun {
    authorize: reqwest::Url,
    /// The flow cookie, `name=value`.
    cookie: String,
    set_cookie: String,
    state: String,
    nonce: String,
    challenge: String,
}

/// What the callback answered.
struct Landed {
    status: StatusCode,
    location: String,
    set_cookies: Vec<String>,
}

impl Landed {
    /// The session cookie's value, if one was set.
    fn session(&self) -> Option<String> {
        self.set_cookies.iter().find_map(|c| {
            c.strip_prefix("__Host-pmos=")
                .map(|rest| rest.split(';').next().unwrap().to_string())
                .filter(|v| !v.is_empty())
        })
    }

    fn assert_refused(&self, reason: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER);
        assert_eq!(self.location, format!("/#/signed-out?reason={reason}"));
        assert!(self.session().is_none(), "{:?}", self.set_cookies);
        // The flow cookie is always cleared.
        assert!(self
            .set_cookies
            .iter()
            .any(|c| c.starts_with("__Host-pmos-entra=;") && c.contains("Max-Age=0")));
    }
}

fn entra_config(public: &str, authority: &str) -> EntraConfig {
    let pairs: HashMap<&str, String> = HashMap::from([
        ("PMOS_WEB_ENTRA_TENANT_ID", TENANT.to_string()),
        ("PMOS_WEB_ENTRA_CLIENT_ID", CLIENT.to_string()),
        ("PMOS_WEB_ENTRA_CLIENT_SECRET", SECRET.to_string()),
        ("PMOS_WEB_ENTRA_AUTHORITY", authority.to_string()),
    ]);
    EntraConfig::from_lookup(|k| pairs.get(k).cloned(), Some(public))
        .unwrap()
        .unwrap()
}

async fn harness() -> Harness {
    harness_with(|_| {}).await
}

async fn harness_with(change: impl FnOnce(&mut Config)) -> Harness {
    let idp = Arc::new(IdpState::default());
    idp.keys.lock().unwrap().push(jwk("key-a", keys::KEY_A_N));
    let (idp_listener, idp_base) = listener().await;
    *idp.base.lock().unwrap() = idp_base.clone();
    let stop_idp = serve(
        idp_listener,
        Router::new()
            .route(
                "/{tenant}/v2.0/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/{tenant}/discovery/v2.0/keys", get(jwks))
            .route("/{tenant}/oauth2/v2.0/token", post(token))
            .with_state(idp.clone()),
    )
    .await;
    let tools = Arc::new(Tools::default());
    tools.directory.seed_team(TEAM);
    let (tools_listener, tools_base) = listener().await;
    let stop_tools = serve(
        tools_listener,
        Router::new().fallback(tools_mock).with_state(tools.clone()),
    )
    .await;
    let (web_listener, web_base) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("team.yaml"), TEAM).unwrap();
    let mut config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        // Nothing here calls the runtime; the tool server answers who people are.
        bots_url: "http://127.0.0.1:9".into(),
        bots_token: BOTS_TOKEN.into(),
        toolserver_url: tools_base,
        toolserver_token: TOOLS_TOKEN.into(),
        content_root: dir.path().to_path_buf(),
        static_dir: None,
        public_url: Some(web_base.clone()),
        dev_login: false,
        entra: Some(entra_config(&web_base, &idp_base)),
        session_idle: Duration::hours(8),
        session_max: Duration::hours(24),
        internal_bind: None,
        password_login: false,
        trusted_proxies: Vec::new(),
    };
    change(&mut config);
    let clock = Arc::new(Mutex::new(Utc::now()));
    let c = clock.clone();
    let deps = Deps {
        clock: Arc::new(move || *c.lock().unwrap()),
        ..Deps::in_memory()
    };
    let stop_web = serve(web_listener, pmos_web::build(config, deps).unwrap()).await;
    Harness {
        web: web_base,
        idp,
        tools,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        clock,
        _dir: dir,
        _stop: vec![stop_idp, stop_tools, stop_web],
    }
}

impl Harness {
    fn now(&self) -> i64 {
        self.clock.lock().unwrap().timestamp()
    }

    fn advance(&self, by: Duration) {
        *self.clock.lock().unwrap() += by;
    }

    fn issuer(&self) -> String {
        format!("{}/{TENANT}/v2.0", self.idp.base.lock().unwrap())
    }

    /// The claims Entra would send for `email`, for this flow.
    fn claims(&self, begun: &Begun, email: &str) -> Value {
        let now = self.now();
        json!({
            "iss": self.issuer(),
            "aud": CLIENT,
            "tid": TENANT,
            "oid": ANA_OID,
            // The optional claim the app registration adds: a member of the tenant.
            "acct": 0,
            "sub": "subject-of-ana",
            "iat": now,
            "nbf": now,
            "exp": now + 3600,
            "nonce": begun.nonce,
            "name": "Ana Silva",
            "preferred_username": email,
            "ver": "2.0",
        })
    }

    async fn begin(&self, return_to: Option<&str>) -> Begun {
        let mut req = self.http.get(format!("{}/api/auth/login", self.web));
        if let Some(r) = return_to {
            req = req.query(&[("return_to", r)]);
        }
        let res = req.send().await.unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let authorize = reqwest::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
        let set_cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        let q: HashMap<String, String> = authorize.query_pairs().into_owned().collect();
        Begun {
            cookie,
            set_cookie,
            state: q["state"].clone(),
            nonce: q["nonce"].clone(),
            challenge: q["code_challenge"].clone(),
            authorize,
        }
    }

    /// Entra hands out a code for this flow's PKCE challenge, redeemable for `id_token`.
    fn issue(&self, challenge: &str, id_token: String) -> String {
        let code = format!(
            "code-{}-0AAAAAAAAAAsecretcode",
            self.idp.next.fetch_add(1, Ordering::SeqCst)
        );
        self.idp
            .secrets_seen
            .lock()
            .unwrap()
            .extend([code.clone(), id_token.clone()]);
        self.idp.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                challenge: challenge.into(),
                redirect_uri: format!("{}/api/auth/callback", self.web),
                id_token,
            },
        );
        code
    }

    async fn callback(&self, query: &[(&str, &str)], cookie: Option<&str>) -> Landed {
        let mut req = self
            .http
            .get(format!("{}/api/auth/callback", self.web))
            .query(query)
            // Microsoft's redirect: a cross-site top-level navigation.
            .header("sec-fetch-site", "cross-site")
            .header("referer", "https://login.microsoftonline.com/");
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        let res = req.send().await.unwrap();
        Landed {
            status: res.status(),
            location: res
                .headers()
                .get("location")
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default(),
            set_cookies: res
                .headers()
                .get_all("set-cookie")
                .iter()
                .map(|v| v.to_str().unwrap().to_string())
                .collect(),
        }
    }

    /// The whole flow with the token `make` builds from good claims.
    async fn sign_in_with(&self, make: impl FnOnce(&Begun, Value) -> String) -> Landed {
        let begun = self.begin(None).await;
        let claims = self.claims(&begun, "Ana@Contoso.example");
        let token = make(&begun, claims);
        let code = self.issue(&begun.challenge, token);
        self.callback(
            &[("code", &code), ("state", &begun.state)],
            Some(&begun.cookie),
        )
        .await
    }

    /// Move the clock past the 10 seconds a directory answer is cached for.
    fn past_the_cache(&self) {
        self.advance(Duration::seconds(10));
    }

    /// The handles linked by a first link so far.
    fn links(&self) -> Vec<String> {
        self.tools.directory.links.lock().unwrap().clone()
    }

    /// The sign-in events recorded, as JSON bodies.
    fn events(&self) -> Vec<Value> {
        self.tools
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.1.clone())
            .collect()
    }

    async fn me(&self, session: &str) -> (StatusCode, Value) {
        let res = self
            .http
            .get(format!("{}/api/me", self.web))
            .header("cookie", format!("__Host-pmos={session}"))
            .send()
            .await
            .unwrap();
        (res.status(), res.json().await.unwrap_or(Value::Null))
    }
}

fn good(_: &Begun, claims: Value) -> String {
    sign(&claims, keys::KEY_A_DER, "key-a")
}

// The happy path.

#[tokio::test]
async fn signs_in_with_entra_and_the_session_works() {
    let h = harness().await;
    let providers: Value = h
        .http
        .get(format!("{}/api/auth/providers", h.web))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers, json!({"providers": ["entra"]}));
    // Development sign-in is off.
    let dev = h
        .http
        .post(format!("{}/api/auth/dev-login", h.web))
        .json(&json!({"handle": "ana"}))
        .send()
        .await
        .unwrap();
    assert_eq!(dev.status(), StatusCode::NOT_FOUND);

    let begun = h.begin(None).await;
    // The authorize request: code flow with PKCE S256, state and nonce, our redirect URI.
    let q: HashMap<String, String> = begun.authorize.query_pairs().into_owned().collect();
    assert_eq!(
        begun.authorize.as_str().split('?').next().unwrap(),
        format!(
            "{}/{TENANT}/oauth2/v2.0/authorize",
            h.idp.base.lock().unwrap()
        )
    );
    assert_eq!(q["client_id"], CLIENT);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["response_mode"], "query");
    // No `email` scope: that claim is never read.
    assert_eq!(q["scope"], "openid profile");
    assert_eq!(q["redirect_uri"], format!("{}/api/auth/callback", h.web));
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(begun.challenge.len(), 43);
    assert!(begun.state.len() >= 43 && begun.nonce.len() >= 43);
    assert_ne!(begun.state, begun.nonce);
    assert!(!begun.authorize.as_str().contains(SECRET));
    // The flow cookie: __Host-, Secure, HttpOnly, Lax (sent on Microsoft's redirect back),
    // ten minutes.
    for attr in [
        "__Host-pmos-entra=",
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Lax",
        "Max-Age=600",
    ] {
        assert!(begun.set_cookie.contains(attr), "{}", begun.set_cookie);
    }
    assert!(!begun.set_cookie.contains("Domain"));

    let claims = h.claims(&begun, "Ana@Contoso.example");
    let code = h.issue(&begun.challenge, sign(&claims, keys::KEY_A_DER, "key-a"));
    let landed = h
        .callback(
            &[
                ("code", &code),
                ("state", &begun.state),
                ("session_state", "ignored"),
            ],
            Some(&begun.cookie),
        )
        .await;
    assert_eq!(landed.status, StatusCode::SEE_OTHER);
    assert_eq!(landed.location, "/");
    let session = landed.session().expect("a session cookie");
    assert!(landed
        .set_cookies
        .iter()
        .any(|c| c.starts_with("__Host-pmos=") && c.contains("HttpOnly")));
    assert!(landed
        .set_cookies
        .iter()
        .any(|c| c.starts_with("__Host-pmos-entra=;")));

    // The token endpoint got the secret, the code, the redirect URI and the verifier.
    let forms = h.idp.forms.lock().unwrap().clone();
    let form = forms.last().unwrap();
    assert_eq!(form["client_secret"], SECRET);
    assert_eq!(form["code"], code);
    assert_eq!(challenge(&form["code_verifier"]), begun.challenge);
    assert_eq!(form["redirect_uri"], format!("{}/api/auth/callback", h.web));

    // The first sign-in linked Ana's Microsoft account, `(tid, oid)`, at the tool server,
    // which got the raw ID token Microsoft issued and this flow's nonce, nothing else: never
    // a bare `(tid, oid)` or a name it would have to take on pmos-web's word (U1).
    assert_eq!(h.links(), ["ana"]);
    let bodies = h.tools.directory.link_bodies.lock().unwrap().clone();
    assert_eq!(
        bodies,
        [json!({
            "id_token": sign(&claims, keys::KEY_A_DER, "key-a"),
            "nonce": begun.nonce,
        })]
    );
    assert_eq!(
        h.tools.directory.get("ana").unwrap().entra,
        Some((TENANT.into(), ANA_OID.into(), false))
    );
    // The same session as development sign-in: /api/me, roles from the user database.
    let (status, me) = h.me(&session).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["handle"], "ana");
    assert_eq!(me["roles"], json!(["pm"]));
    let csrf = me["csrf_token"].as_str().unwrap().to_string();

    // Logout ends the local session only.
    let out = h
        .http
        .post(format!("{}/api/auth/logout", h.web))
        .header("cookie", format!("__Host-pmos={session}"))
        .header("x-pmos-csrf", csrf)
        .header("origin", h.web.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
    assert_eq!(h.me(&session).await.0, StatusCode::UNAUTHORIZED);
    // Discovery and keys were fetched once and cached.
    assert_eq!(h.idp.discovery_hits.load(Ordering::SeqCst), 1);
    assert_eq!(h.idp.jwks_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn never_falls_back_to_the_email_claim_and_keeps_safe_return_paths() {
    let h = harness().await;
    // No preferred_username, and Ana's address in `email`: never used, so no first link.
    let landed = h
        .sign_in_with(|_, mut c| {
            c.as_object_mut().unwrap().remove("preferred_username");
            c["email"] = json!("ana@contoso.example");
            sign(&c, keys::KEY_A_DER, "key-a")
        })
        .await;
    landed.assert_refused("not_a_member");
    // Nor when `preferred_username` names nobody and `email` names Ana.
    let landed = h
        .sign_in_with(|_, mut c| {
            c["preferred_username"] = json!("a.silva@contoso.example");
            c["email"] = json!("ana@contoso.example");
            sign(&c, keys::KEY_A_DER, "key-a")
        })
        .await;
    landed.assert_refused("not_a_member");
    assert!(h.links().is_empty());

    for (asked, lands) in [
        ("/#/drafts/apr_1", "/#/drafts/apr_1"),
        ("//evil.example/x", "/"),
        ("https://evil.example/", "/"),
        ("/\\evil.example", "/"),
    ] {
        let begun = h.begin(Some(asked)).await;
        let claims = h.claims(&begun, "ana@contoso.example");
        let code = h.issue(&begun.challenge, sign(&claims, keys::KEY_A_DER, "key-a"));
        let landed = h
            .callback(
                &[("code", &code), ("state", &begun.state)],
                Some(&begun.cookie),
            )
            .await;
        assert_eq!(landed.status, StatusCode::SEE_OTHER);
        assert_eq!(landed.location, lands, "{asked}");
        assert!(landed.session().is_some());
    }
}

// State, the flow cookie and PKCE.

#[tokio::test]
async fn refuses_a_wrong_missing_reused_or_expired_state() {
    let h = harness().await;
    // Wrong state.
    let begun = h.begin(None).await;
    let code = h.issue(
        &begun.challenge,
        good(&begun, h.claims(&begun, "ana@contoso.example")),
    );
    h.callback(
        &[("code", &code), ("state", "not-the-state")],
        Some(&begun.cookie),
    )
    .await
    .assert_refused("failed");
    // No flow cookie (another browser, or a forged link): expired.
    h.callback(&[("code", &code), ("state", &begun.state)], None)
        .await
        .assert_refused("expired");
    // A tampered flow cookie.
    let tampered = format!("{}x", begun.cookie);
    h.callback(&[("code", &code), ("state", &begun.state)], Some(&tampered))
        .await
        .assert_refused("failed");
    // No code or state at all.
    h.callback(&[], Some(&begun.cookie))
        .await
        .assert_refused("failed");
    // The person cancelled at Microsoft.
    h.callback(
        &[("error", "access_denied"), ("state", &begun.state)],
        Some(&begun.cookie),
    )
    .await
    .assert_refused("cancelled");

    // Reused state: a successful sign-in, then the same callback again with a fresh code.
    let begun = h.begin(None).await;
    let code = h.issue(
        &begun.challenge,
        good(&begun, h.claims(&begun, "ana@contoso.example")),
    );
    let ok = h
        .callback(
            &[("code", &code), ("state", &begun.state)],
            Some(&begun.cookie),
        )
        .await;
    assert!(ok.session().is_some());
    let again = h.issue(
        &begun.challenge,
        good(&begun, h.claims(&begun, "ana@contoso.example")),
    );
    h.callback(
        &[("code", &again), ("state", &begun.state)],
        Some(&begun.cookie),
    )
    .await
    .assert_refused("failed");

    // A flow older than ten minutes.
    let begun = h.begin(None).await;
    h.advance(Duration::minutes(11));
    let code = h.issue(
        &begun.challenge,
        good(&begun, h.claims(&begun, "ana@contoso.example")),
    );
    h.callback(
        &[("code", &code), ("state", &begun.state)],
        Some(&begun.cookie),
    )
    .await
    .assert_refused("expired");
}

#[tokio::test]
async fn a_code_for_another_pkce_challenge_is_refused_at_the_token_endpoint() {
    let h = harness().await;
    // An attacker injects a code issued for their own flow (another challenge) into the
    // victim's callback: the victim's verifier doesn't match it.
    let victim = h.begin(None).await;
    let attacker = h.begin(None).await;
    let code = h.issue(
        &attacker.challenge,
        good(&victim, h.claims(&victim, "ana@contoso.example")),
    );
    h.callback(
        &[("code", &code), ("state", &victim.state)],
        Some(&victim.cookie),
    )
    .await
    .assert_refused("failed");
    // The server always sent a verifier; the mock refuses without one, as Entra does.
    let forms = h.idp.forms.lock().unwrap().clone();
    assert!(forms.iter().all(|f| f.contains_key("code_verifier")));
    let no_verifier = h
        .http
        .post(format!(
            "{}/{TENANT}/oauth2/v2.0/token",
            h.idp.base.lock().unwrap()
        ))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT),
            ("client_secret", SECRET),
            ("code", &h.issue(&victim.challenge, "t".into())),
            ("redirect_uri", &format!("{}/api/auth/callback", h.web)),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(no_verifier.status(), StatusCode::BAD_REQUEST);
}

// The ID token.

#[tokio::test]
async fn refuses_every_bad_id_token() {
    let h = harness().await;
    type Make = Box<dyn FnOnce(&Begun, Value) -> String>;
    let with = |key: &'static str, value: Value| -> Make {
        Box::new(move |_, mut c: Value| {
            c[key] = value;
            sign(&c, keys::KEY_A_DER, "key-a")
        })
    };
    let without = |key: &'static str| -> Make {
        Box::new(move |_, mut c: Value| {
            c.as_object_mut().unwrap().remove(key);
            sign(&c, keys::KEY_A_DER, "key-a")
        })
    };
    let now = h.now();
    let base = h.idp.base.lock().unwrap().clone();
    let cases: Vec<(&str, Make)> = vec![
        ("wrong nonce", with("nonce", json!("another-nonce"))),
        ("no nonce", without("nonce")),
        (
            "wrong audience",
            with("aud", json!("99999999-9999-4999-8999-999999999999")),
        ),
        (
            "two audiences",
            with(
                "aud",
                json!([CLIENT, "99999999-9999-4999-8999-999999999999"]),
            ),
        ),
        (
            "wrong issuer",
            with(
                "iss",
                json!(format!("{base}/99999999-9999-4999-8999-999999999999/v2.0")),
            ),
        ),
        (
            "v1 issuer",
            with("iss", json!(format!("https://sts.windows.net/{TENANT}/"))),
        ),
        (
            "multi-tenant issuer",
            with("iss", json!(format!("{base}/common/v2.0"))),
        ),
        (
            "issuer template",
            with("iss", json!(format!("{base}/{{tenantid}}/v2.0"))),
        ),
        (
            "wrong tenant",
            with("tid", json!("99999999-9999-4999-8999-999999999999")),
        ),
        ("no tenant", without("tid")),
        ("expired", with("exp", json!(now - 61))),
        ("not yet valid", with("nbf", json!(now + 120))),
        ("issued in the future", with("iat", json!(now + 120))),
        ("no exp", without("exp")),
        ("no object id", without("oid")),
        ("an object id that isn't a GUID", with("oid", json!("ana"))),
        (
            "alg none",
            Box::new(|_, c: Value| {
                format!(
                    "{}.{}.",
                    b64(&serde_json::to_vec(&json!({"alg": "none", "kid": "key-a"})).unwrap()),
                    b64(&serde_json::to_vec(&c).unwrap())
                )
            }),
        ),
        (
            "HS256 with the public key as secret",
            Box::new(|_, c: Value| {
                let mut header = Header::new(Algorithm::HS256);
                header.kid = Some("key-a".into());
                let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(keys::KEY_A_N)
                    .unwrap();
                jsonwebtoken::encode(&header, &c, &EncodingKey::from_secret(&secret)).unwrap()
            }),
        ),
        (
            "signed by another key under a known kid",
            Box::new(|_, c: Value| sign(&c, keys::KEY_B_DER, "key-a")),
        ),
        (
            "unknown kid",
            Box::new(|_, c: Value| sign(&c, keys::KEY_B_DER, "key-b")),
        ),
        (
            "no kid",
            Box::new(|_, c: Value| {
                jsonwebtoken::encode(
                    &Header::new(Algorithm::RS256),
                    &c,
                    &EncodingKey::from_rsa_der(&der(keys::KEY_A_DER)),
                )
                .unwrap()
            }),
        ),
        (
            "a tampered payload",
            Box::new(|_, mut c: Value| {
                let token = sign(&c, keys::KEY_A_DER, "key-a");
                c["preferred_username"] = json!("someone-else@contoso.example");
                let parts: Vec<&str> = token.split('.').collect();
                format!(
                    "{}.{}.{}",
                    parts[0],
                    b64(&serde_json::to_vec(&c).unwrap()),
                    parts[2]
                )
            }),
        ),
        ("garbage", Box::new(|_, _| "not.a.jwt".into())),
    ];
    for (name, make) in cases {
        let landed = h.sign_in_with(make).await;
        assert_eq!(
            landed.location, "/#/signed-out?reason=failed",
            "{name}: {}",
            landed.location
        );
        landed.assert_refused("failed");
    }
    // Inside the 60 seconds of skew: still fine, both ways.
    let landed = h
        .sign_in_with(|_, mut c| {
            c["exp"] = json!(now - 30);
            c["nbf"] = json!(now - 3600);
            c["iat"] = json!(now - 3600);
            sign(&c, keys::KEY_A_DER, "key-a")
        })
        .await;
    assert!(landed.session().is_some(), "{}", landed.location);
    let landed = h
        .sign_in_with(|_, mut c| {
            c["nbf"] = json!(now + 30);
            c["iat"] = json!(now + 30);
            sign(&c, keys::KEY_A_DER, "key-a")
        })
        .await;
    assert!(landed.session().is_some(), "{}", landed.location);
    // The unknown kids above refetched the keys at most once (rate limited).
    assert!(h.idp.jwks_hits.load(Ordering::SeqCst) <= 2);
}

/// Members only (spec §7): the optional `acct` claim must be there and say member; `idp`, when
/// present, must name this tenant. Checked before any lookup in the user database.
#[tokio::test]
async fn guests_other_identity_providers_and_a_missing_acct_claim_are_refused() {
    let h = harness().await;
    let set = |key: &'static str, value: Value| {
        move |_: &Begun, mut c: Value| {
            c[key] = value;
            sign(&c, keys::KEY_A_DER, "key-a")
        }
    };
    let base = h.idp.base.lock().unwrap().clone();
    let lookups = || {
        h.tools
            .directory
            .resolves
            .load(std::sync::atomic::Ordering::SeqCst)
    };
    // A guest (`acct: 1`), in either spelling.
    h.sign_in_with(set("acct", json!(1)))
        .await
        .assert_refused("not_a_member");
    h.sign_in_with(set("acct", json!("1")))
        .await
        .assert_refused("not_a_member");
    h.sign_in_with(set("acct", json!(7)))
        .await
        .assert_refused("not_a_member");
    // No `acct` at all: the registration lacks the optional claim. Fails closed.
    h.sign_in_with(|_, mut c| {
        c.as_object_mut().unwrap().remove("acct");
        sign(&c, keys::KEY_A_DER, "key-a")
    })
    .await
    .assert_refused("entra_misconfigured");
    // Signed in by another identity provider: a personal Microsoft account, another tenant.
    for idp in [
        "live.com".to_string(),
        "https://sts.windows.net/9188040d-6c67-4c5b-b112-36a304b66dad/".to_string(),
        "https://sts.windows.net/99999999-9999-4999-8999-999999999999/".to_string(),
        format!("{base}/99999999-9999-4999-8999-999999999999/v2.0"),
    ] {
        h.sign_in_with(set("idp", json!(idp)))
            .await
            .assert_refused("not_a_member");
    }
    assert_eq!(lookups(), 0, "refused before any lookup");
    assert!(h.links().is_empty());
    let reasons: Vec<Value> = h.events().iter().map(|e| e["reason"].clone()).collect();
    assert_eq!(reasons.len(), 8);
    assert_eq!(reasons[3], "entra_misconfigured");
    // A member's `idp`, when Entra sends one, names this tenant: the token's own issuer, or
    // the tenant's v1 issuer. Both sign in; so does `acct: "0"`.
    let issuer = h.issuer();
    h.sign_in_with(set("idp", json!(issuer)))
        .await
        .session()
        .expect("idp is the issuer");
    h.sign_in_with(set(
        "idp",
        json!(format!("https://sts.windows.net/{TENANT}/")),
    ))
    .await
    .session()
    .expect("idp is the tenant's v1 issuer");
    h.sign_in_with(set("acct", json!("0")))
        .await
        .session()
        .expect("acct as a string");
}

/// The first link is by `preferred_username` only, to an active user whose sign-in name an
/// admin confirmed, holding neither Admin nor Auditor (spec §7 step 2).
#[tokio::test]
async fn the_first_link_needs_a_confirmed_name_and_no_admin_or_auditor_role() {
    let h = harness().await;
    // A Slack joiner whose sign-in name no admin has confirmed.
    h.tools.directory.update("ana", |u| u.confirmed = false);
    h.sign_in_with(good)
        .await
        .assert_refused("sign_in_unconfirmed");
    h.tools.directory.update("ana", |u| u.confirmed = true);
    // An Admin, and an Auditor, without a link the host set: refused, never auto-linked.
    for roles in [vec!["pm", "admin"], vec!["auditor"]] {
        h.tools.directory.update("ana", |u| {
            u.roles = roles.iter().map(|r| r.to_string()).collect()
        });
        h.sign_in_with(good)
            .await
            .assert_refused("entra_needs_host");
    }
    assert!(h.links().is_empty());
    // Linked automatically before (as a PM), not confirmed on the host: still refused.
    h.tools.directory.update("ana", |u| {
        u.entra = Some((TENANT.into(), ANA_OID.into(), false))
    });
    h.past_the_cache();
    h.sign_in_with(good)
        .await
        .assert_refused("entra_needs_host");
    // The host confirms it (`pmos-admin users set-entra`): in.
    h.tools.directory.update("ana", |u| {
        u.entra = Some((TENANT.into(), ANA_OID.into(), true))
    });
    h.past_the_cache();
    let session = h.sign_in_with(good).await.session().expect("signed in");
    assert_eq!(h.me(&session).await.1["roles"], json!(["auditor"]));
    let reasons: Vec<Value> = h
        .events()
        .iter()
        .filter(|e| e["action"] == "web.sign_in_refused")
        .map(|e| e["reason"].clone())
        .collect();
    assert_eq!(
        reasons,
        [
            "sign_in_unconfirmed",
            "entra_needs_host",
            "entra_needs_host",
            "entra_needs_host"
        ]
    );
}

/// Granting Admin or Auditor to someone with an automatic link marks it `needs_host`: their
/// session ends within 10 seconds, until the host confirms the link.
#[tokio::test]
async fn a_session_ends_when_its_link_needs_the_host() {
    let h = harness().await;
    let session = h.sign_in_with(good).await.session().expect("signed in");
    h.tools
        .directory
        .update("ana", |u| u.roles = vec!["pm".into(), "auditor".into()]);
    h.past_the_cache();
    let (status, body) = h.me(&session).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "entra_needs_host");
    assert_eq!(h.me(&session).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_deactivated_user_is_refused_at_sign_in_and_their_session_ends() {
    let h = harness().await;
    // Not linked yet: the first link refuses a deactivated user.
    h.tools
        .directory
        .update("ana", |u| u.status = "deactivated".into());
    h.sign_in_with(good).await.assert_refused("deactivated");
    assert!(h.links().is_empty());
    h.tools
        .directory
        .update("ana", |u| u.status = "active".into());
    h.past_the_cache();
    let session = h.sign_in_with(good).await.session().expect("signed in");
    // Linked: deactivated now, the session ends within 10 seconds, and sign-in is refused.
    h.tools
        .directory
        .update("ana", |u| u.status = "deactivated".into());
    h.past_the_cache();
    let (status, body) = h.me(&session).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "not_a_member");
    h.sign_in_with(good).await.assert_refused("deactivated");
}

/// The tool server unreachable at sign-in: no session (`users_unavailable`), recorded.
#[tokio::test]
async fn sign_in_fails_closed_when_the_user_database_is_unreachable() {
    let h = harness().await;
    h.tools.directory.set_down(true);
    h.sign_in_with(good)
        .await
        .assert_refused("users_unavailable");
    assert_eq!(
        h.events().last().unwrap()["reason"],
        json!("users_unavailable")
    );
    // A session from before the outage: reads of pmos-web's own store only, for 5 minutes.
    h.tools.directory.set_down(false);
    h.past_the_cache();
    let session = h.sign_in_with(good).await.session().expect("signed in");
    h.tools.directory.set_down(true);
    h.past_the_cache();
    assert_eq!(h.me(&session).await.0, StatusCode::OK);
    h.advance(Duration::minutes(5));
    let (status, body) = h.me(&session).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "users_unavailable");
}

#[tokio::test]
async fn an_unknown_kid_refetches_the_keys_so_rotation_works() {
    let h = harness_with(|c| {
        c.entra.as_mut().unwrap().jwks_refetch_after = std::time::Duration::ZERO;
    })
    .await;
    assert!(h.sign_in_with(good).await.session().is_some());
    assert_eq!(h.idp.jwks_hits.load(Ordering::SeqCst), 1);
    // Microsoft rotates: key B appears and signs new tokens.
    h.idp.keys.lock().unwrap().push(jwk("key-b", keys::KEY_B_N));
    let landed = h
        .sign_in_with(|_, c| sign(&c, keys::KEY_B_DER, "key-b"))
        .await;
    assert!(landed.session().is_some(), "{}", landed.location);
    assert_eq!(h.idp.jwks_hits.load(Ordering::SeqCst), 2);
    // A known kid doesn't refetch.
    assert!(h.sign_in_with(good).await.session().is_some());
    assert_eq!(h.idp.jwks_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unknown_kids_are_rate_limited() {
    let h = harness().await;
    assert!(h.sign_in_with(good).await.session().is_some());
    for _ in 0..5 {
        h.sign_in_with(|_, c| sign(&c, keys::KEY_B_DER, "key-b"))
            .await
            .assert_refused("failed");
    }
    // Fetched once at the first sign-in; the unknown kids came within the minute.
    assert_eq!(h.idp.jwks_hits.load(Ordering::SeqCst), 1);
}

// Membership.

/// Entra sign-ins and refusals reach the tool server's audit log (`POST
/// /internal/admin/events`): a sign-in for the member, a refusal for no one with its code,
/// and never the email tried, only a short keyed hash of it. A callback no flow started is
/// not recorded (anyone can send one).
#[tokio::test]
async fn entra_sign_ins_and_refusals_are_audited() {
    let h = harness().await;
    h.sign_in_with(good).await.session().expect("signed in");
    let zoe = |_: &Begun, mut c: Value| {
        c["oid"] = json!("22222222-0000-0000-0000-000000000002");
        c["preferred_username"] = json!("zoe@contoso.example");
        sign(&c, keys::KEY_A_DER, "key-a")
    };
    h.sign_in_with(zoe).await.assert_refused("not_a_member");
    // A callback with no flow behind it: refused, not recorded.
    let res = h
        .http
        .get(format!("{}/api/auth/callback?code=x&state=y", h.web))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let events = h.tools.events.lock().unwrap().clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, "/internal/admin/events?user=ana");
    assert_eq!(
        events[0].1,
        json!({"action": "web.sign_in", "provider": "entra"})
    );
    assert_eq!(events[1].0, "/internal/admin/events");
    assert_eq!(events[1].1["action"], "web.sign_in_refused");
    assert_eq!(events[1].1["reason"], "not_a_member");
    let attempt = events[1].1["attempt"].as_str().unwrap();
    assert!(attempt.len() == 12 && attempt.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!events[1].2.contains("zoe") && !events[1].2.contains("contoso"));
    for (_, _, raw) in &events {
        assert!(!raw.contains(SECRET) && !raw.contains(TOOLS_TOKEN));
        assert!(!raw.contains(ANA_OID), "never an oid");
    }
}

/// Spec §7 step 3: a sign-in whose name is already linked to a different Microsoft account is
/// refused, audited and flagged; and a known account signing in with another user's sign-in
/// name is flagged (it signs in as itself).
#[tokio::test]
async fn conflicts_are_refused_audited_and_flagged() {
    let h = harness().await;
    h.sign_in_with(good).await.session().expect("ana linked");
    // Someone else in the tenant now holds Ana's sign-in name (a reassigned mailbox): another
    // account, another `oid`. Refused, and the tool server audited the conflict.
    let reassigned = |_: &Begun, mut c: Value| {
        c["oid"] = json!("11111111-2222-3333-4444-555555555555");
        sign(&c, keys::KEY_A_DER, "key-a")
    };
    h.sign_in_with(reassigned)
        .await
        .assert_refused("not_a_member");
    assert_eq!(*h.tools.directory.conflicts.lock().unwrap(), ["ana"]);
    assert_eq!(h.links(), ["ana"], "nothing new linked");
    let refused = h.events().last().cloned().unwrap();
    assert_eq!(refused["action"], "web.sign_in_refused");
    assert_eq!(refused["reason"], "not_a_member");
    // Ana's own account presenting Ben's sign-in name: she signs in as Ana (the name is
    // ignored after the first link), and the conflict is flagged at the tool server.
    h.tools.directory.update("ben", |u| {
        u.sign_in_name = Some("ben@contoso.example".into());
        u.confirmed = true;
    });
    let as_ben = |_: &Begun, mut c: Value| {
        c["preferred_username"] = json!("Ben@contoso.example");
        sign(&c, keys::KEY_A_DER, "key-a")
    };
    let session = h.sign_in_with(as_ben).await.session().expect("signed in");
    assert_eq!(h.me(&session).await.1["handle"], "ana");
    let flagged: Vec<Value> = h
        .events()
        .into_iter()
        .filter(|e| e["action"] == "users.identity_conflict")
        .collect();
    assert_eq!(
        flagged,
        [json!({"action": "users.identity_conflict", "provider": "entra", "member": "ben"})]
    );
}

/// After the first link, the Microsoft account decides, not the name: a renamed account still
/// signs in, a reassigned name doesn't, and an admin's unlink ends the old account's session.
#[tokio::test]
async fn the_microsoft_account_decides_after_the_first_link() {
    let h = harness().await;
    let session = h.sign_in_with(good).await.session().expect("signed in");
    // Ana's account renamed in the tenant: still Ana.
    let renamed = |_: &Begun, mut c: Value| {
        c["preferred_username"] = json!("ana.silva@contoso.example");
        sign(&c, keys::KEY_A_DER, "key-a")
    };
    assert!(h.sign_in_with(renamed).await.session().is_some());
    // An admin unlinks it (the mailbox was reassigned): her session ends within 10 seconds.
    h.tools.directory.update("ana", |u| u.entra = None);
    h.past_the_cache();
    let (status, body) = h.me(&session).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "signed_out");
    // The account that now holds the name links on its next sign-in.
    let new_owner = |_: &Begun, mut c: Value| {
        c["oid"] = json!("11111111-2222-3333-4444-555555555555");
        sign(&c, keys::KEY_A_DER, "key-a")
    };
    assert!(h.sign_in_with(new_owner).await.session().is_some());
    assert_eq!(h.links(), ["ana", "ana"]);
}

#[tokio::test]
async fn someone_not_on_the_team_gets_no_session() {
    let h = harness().await;
    let as_user = |name: &'static str| {
        move |_: &Begun, mut c: Value| {
            c["preferred_username"] = json!(name);
            sign(&c, keys::KEY_A_DER, "key-a")
        }
    };
    // A valid token for someone not in the user database.
    h.sign_in_with(as_user("zoe@contoso.example"))
        .await
        .assert_refused("not_a_member");
    // A handle is never matched: only a confirmed sign-in name.
    h.sign_in_with(as_user("ben"))
        .await
        .assert_refused("not_a_member");
    // `team.yaml` decides nothing: someone only there doesn't sign in.
    std::fs::write(
        h._dir.path().join("team.yaml"),
        "members:\n  - {handle: zoe, slack-id: U0ZOE, email: zoe@contoso.example, roles: [pm, admin]}\n",
    )
    .unwrap();
    h.sign_in_with(as_user("zoe@contoso.example"))
        .await
        .assert_refused("not_a_member");
    assert!(h.links().is_empty());
}

// Providers and start-up.

#[tokio::test]
async fn providers_say_which_sign_in_is_on() {
    let both = harness_with(|c| c.dev_login = true).await;
    let providers: Value = both
        .http
        .get(format!("{}/api/auth/providers", both.web))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers, json!({"providers": ["entra", "dev"]}));

    let dev_only = harness_with(|c| {
        c.dev_login = true;
        c.entra = None;
    })
    .await;
    let get = |path: &str| dev_only.http.get(format!("{}{path}", dev_only.web)).send();
    let providers: Value = get("/api/auth/providers")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers, json!({"providers": ["dev"]}));
    for path in ["/api/auth/login", "/api/auth/callback?code=x&state=y"] {
        let res = get(path).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["code"], "not_found");
    }
}

#[tokio::test]
async fn an_unreachable_identity_provider_is_a_friendly_redirect() {
    let h = harness_with(|c| {
        let public = c.public_url.clone().unwrap();
        c.entra = Some(entra_config(&public, "http://127.0.0.1:9"));
    })
    .await;
    let res = h
        .http
        .get(format!("{}/api/auth/login", h.web))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        res.headers()["location"],
        "/#/signed-out?reason=unavailable"
    );
    assert!(res.headers().get("set-cookie").is_none());
}

fn lookup(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |k| {
        pairs
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
    }
}

fn env(dev: bool, entra: &[&str]) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("PMOS_WEB_BOTS_TOKEN", BOTS_TOKEN.to_string()),
        ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS_TOKEN.to_string()),
    ];
    if dev {
        out.push(("PMOS_WEB_DEV_LOGIN", "1".into()));
    }
    let all = [
        ("PMOS_WEB_ENTRA_TENANT_ID", TENANT),
        ("PMOS_WEB_ENTRA_CLIENT_ID", CLIENT),
        ("PMOS_WEB_ENTRA_CLIENT_SECRET", SECRET),
        ("PMOS_WEB_PUBLIC_URL", "https://pmos.contoso.example"),
        (
            "PMOS_WEB_ENTRA_AUTHORITY",
            "https://login.microsoftonline.us",
        ),
    ];
    for (k, v) in all {
        if entra.contains(&k) {
            out.push((k, v.to_string()));
        }
    }
    out
}

#[test]
fn start_up_needs_entra_fully_configured_or_development_sign_in() {
    const FULL: [&str; 4] = [
        "PMOS_WEB_ENTRA_TENANT_ID",
        "PMOS_WEB_ENTRA_CLIENT_ID",
        "PMOS_WEB_ENTRA_CLIENT_SECRET",
        "PMOS_WEB_PUBLIC_URL",
    ];
    // Neither.
    let err = Config::from_lookup(lookup(env(false, &[]))).unwrap_err();
    assert!(err.contains("no sign-in provider"), "{err}");
    // Entra alone, fully configured.
    let cfg = Config::from_lookup(lookup(env(false, &FULL))).unwrap();
    let entra = cfg.entra.as_ref().unwrap();
    assert!(!cfg.dev_login);
    assert_eq!(
        entra.redirect_uri,
        "https://pmos.contoso.example/api/auth/callback"
    );
    assert_eq!(
        entra.issuer(),
        format!("https://login.microsoftonline.com/{TENANT}/v2.0")
    );
    let providers: Vec<&str> = pmos_web::app::providers(&cfg)
        .unwrap()
        .iter()
        .map(|p| p.name())
        .collect();
    assert_eq!(providers, ["entra"]);
    // Both: refused, unless the explicit development override is set (and main then logs a
    // loud warning).
    let err = Config::from_lookup(lookup(env(true, &FULL))).unwrap_err();
    assert!(err.contains("PMOS_WEB_DEV_LOGIN_WITH_ENTRA"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
    let mut overridden = env(true, &FULL);
    overridden.push(("PMOS_WEB_DEV_LOGIN_WITH_ENTRA", "1".into()));
    let cfg = Config::from_lookup(lookup(overridden.clone())).unwrap();
    assert!(cfg.dev_login && cfg.entra.is_some());
    // And never on an address others can reach.
    overridden.push(("PMOS_WEB_BIND", "0.0.0.0:8770".into()));
    let err = Config::from_lookup(lookup(overridden)).unwrap_err();
    assert!(err.contains("loopback"), "{err}");
    // A sovereign cloud's authority.
    let mut with_authority = FULL.to_vec();
    with_authority.push("PMOS_WEB_ENTRA_AUTHORITY");
    let cfg = Config::from_lookup(lookup(env(false, &with_authority))).unwrap();
    assert_eq!(
        cfg.entra.unwrap().issuer(),
        format!("https://login.microsoftonline.us/{TENANT}/v2.0")
    );
    // Half-configured never starts, with or without development sign-in.
    for missing in FULL {
        let some: Vec<&str> = FULL.iter().copied().filter(|k| *k != missing).collect();
        for dev in [false, true] {
            let err = Config::from_lookup(lookup(env(dev, &some))).unwrap_err();
            assert!(err.contains(missing), "{missing}: {err}");
            assert!(!err.contains(SECRET));
        }
    }
    let err = Config::from_lookup(lookup(env(true, &["PMOS_WEB_ENTRA_AUTHORITY"]))).unwrap_err();
    assert!(err.contains("half-configured"), "{err}");
    // A multi-tenant authority.
    let mut multi = env(false, &FULL);
    multi[2].1 = "organizations".into();
    let err = Config::from_lookup(lookup(multi)).unwrap_err();
    assert!(err.contains("multi-tenant"), "{err}");
    // The authority override is https unless loopback.
    let mut plain = env(false, &FULL);
    plain.push((
        "PMOS_WEB_ENTRA_AUTHORITY",
        "http://idp.contoso.example".into(),
    ));
    assert!(Config::from_lookup(lookup(plain))
        .unwrap_err()
        .contains("https unless"));
    // The config's Debug never shows the client secret.
    let cfg = Config::from_lookup(lookup(env(false, &FULL))).unwrap();
    assert!(!format!("{cfg:?}").contains(SECRET));
}

// Logs.

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every log line of this test binary, at every level: one global subscriber, so the
/// server's tasks on any thread are captured (other tests' lines land here too).
fn captured_logs() -> Captured {
    static LOGS: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let captured = Captured::default();
        let writer = captured.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .init();
        captured
    })
    .clone()
}

#[tokio::test]
async fn logs_never_contain_a_secret_code_or_token() {
    let captured = captured_logs();

    let h = harness().await;
    let mut flow_secrets = Vec::new();
    // A sign-in, a refused token, a non-member, a refused code, a bad state.
    for make in [
        Box::new(good) as Box<dyn FnOnce(&Begun, Value) -> String>,
        Box::new(|_: &Begun, mut c: Value| {
            c["aud"] = json!("someone-else");
            sign(&c, keys::KEY_A_DER, "key-a")
        }),
        Box::new(|_: &Begun, mut c: Value| {
            c["preferred_username"] = json!("zoe@contoso.example");
            sign(&c, keys::KEY_A_DER, "key-a")
        }),
    ] {
        let begun = h.begin(None).await;
        let claims = h.claims(&begun, "ana@contoso.example");
        let code = h.issue(&begun.challenge, make(&begun, claims));
        let landed = h
            .callback(
                &[("code", &code), ("state", &begun.state)],
                Some(&begun.cookie),
            )
            .await;
        flow_secrets.extend([
            begun.state.clone(),
            begun.nonce.clone(),
            begun.cookie.split_once('=').unwrap().1.to_string(),
        ]);
        if let Some(s) = landed.session() {
            flow_secrets.push(s);
        }
    }
    let begun = h.begin(None).await;
    let other = h.begin(None).await;
    let code = h.issue(
        &other.challenge,
        good(&begun, h.claims(&begun, "ana@contoso.example")),
    );
    h.callback(
        &[("code", &code), ("state", &begun.state)],
        Some(&begun.cookie),
    )
    .await
    .assert_refused("failed");
    h.callback(&[("code", &code), ("state", "wrong")], Some(&other.cookie))
        .await
        .assert_refused("failed");
    let verifiers: Vec<String> = h
        .idp
        .forms
        .lock()
        .unwrap()
        .iter()
        .map(|f| f["code_verifier"].clone())
        .collect();

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("signed in"), "{logs}");
    assert!(logs.contains("sign-in refused"), "{logs}");
    assert!(logs.contains("invalid_grant"), "{logs}");
    let secrets = h.idp.secrets_seen.lock().unwrap().clone();
    for s in secrets
        .iter()
        .chain(&flow_secrets)
        .chain(&verifiers)
        .map(String::as_str)
        .chain([SECRET])
    {
        // The id token's pieces too: a log line could carry just one.
        for piece in s.split('.').filter(|p| p.len() >= 16) {
            assert!(!logs.contains(piece), "a secret in the logs: {piece}");
        }
    }
    // Nor an object id or a sign-in name (the user database's keys).
    assert!(!logs.contains(ANA_OID), "an oid in the logs");
    assert!(
        !logs.contains("ana@contoso.example"),
        "a sign-in name in the logs"
    );
    assert!(
        !logs.contains("zoe@contoso.example"),
        "a sign-in name in the logs"
    );
    // The hash of nothing logged either (sanity: the secret's digest isn't a log key).
    let digest = b64(&sha2::Sha256::digest(SECRET.as_bytes()));
    assert!(!logs.contains(&digest));
}

/// With Entra sign-in on, `/readyz` checks that the tool server validates first-link tokens
/// for the same tenant and client id (U1): not configured there, or another tenant or client,
/// is `entra_link: down` (and the server not ready); the same ids are `ok`. With Entra off
/// there is no such check.
#[tokio::test]
async fn readyz_checks_the_tool_servers_entra_settings() {
    let h = harness().await;
    let check = || async {
        let res = h
            .http
            .get(format!("{}/readyz", h.web))
            .send()
            .await
            .unwrap();
        let status = res.status();
        let body: Value = res.json().await.unwrap();
        (status, body["checks"]["entra_link"].clone())
    };
    let (status, link) = check().await;
    assert_eq!(link, "down");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    for (tenant, client) in [
        ("11111111-2222-3333-4444-555555555555", CLIENT),
        (TENANT, "11111111-2222-3333-4444-555555555555"),
    ] {
        *h.tools.directory.entra_link.lock().unwrap() = Some((tenant.into(), client.into()));
        assert_eq!(check().await.1, "down", "{tenant} {client}");
    }
    *h.tools.directory.entra_link.lock().unwrap() = Some((TENANT.to_uppercase(), CLIENT.into()));
    assert_eq!(check().await.1, "ok");

    let off = harness_with(|c| {
        c.entra = None;
        c.dev_login = true;
    })
    .await;
    let body: Value = off
        .http
        .get(format!("{}/readyz", off.web))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["checks"].get("entra_link").is_none(), "{body}");
}
