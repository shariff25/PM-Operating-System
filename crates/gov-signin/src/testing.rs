//! For tests only (feature `test-support`): two RSA keys, a token signer, valid claims for a
//! tenant, and a fake identity provider serving the discovery document and the JWKS, which
//! can be taken down and whose keys can be rotated.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

#[path = "testing_keys.rs"]
pub mod keys;

/// A key's PKCS#1 DER from its base64.
pub fn der(b64_der: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(b64_der)
        .expect("test key base64")
}

/// A JWKS entry for an RSA signing key.
pub fn jwk(kid: &str, n: &str) -> Value {
    json!({"kty": "RSA", "use": "sig", "kid": kid, "n": n, "e": "AQAB"})
}

/// `claims` signed with RS256 under `kid`.
pub fn sign(claims: &Value, key_der: &str, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    jsonwebtoken::encode(&header, claims, &EncodingKey::from_rsa_der(&der(key_der)))
        .expect("sign a test token")
}

/// `claims` with `alg: none` and no signature.
pub fn unsigned(claims: &Value, kid: &str) -> String {
    let b64 = |v: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
    };
    format!(
        "{}.{}.",
        b64(&json!({"alg": "none", "typ": "JWT", "kid": kid})),
        b64(claims)
    )
}

/// A member's ID token claims as Entra issues them for `client` in `tenant` (issuer under
/// `authority`), valid from `now` for an hour.
pub fn member_claims(
    authority: &str,
    tenant: &str,
    client: &str,
    oid: &str,
    preferred_username: &str,
    nonce: &str,
    now: i64,
) -> Value {
    json!({
        "iss": format!("{authority}/{tenant}/v2.0"),
        "aud": client,
        "exp": now + 3600,
        "iat": now,
        "nbf": now,
        "nonce": nonce,
        "tid": tenant,
        "oid": oid,
        "acct": 0,
        "auth_time": now,
        "preferred_username": preferred_username,
        "ver": "2.0",
    })
}

#[derive(Default)]
struct IdpState {
    base: Mutex<String>,
    tenant: Mutex<String>,
    keys: Mutex<Vec<Value>>,
    down: AtomicBool,
    discovery_hits: AtomicUsize,
    jwks_hits: AtomicUsize,
    /// How long the keys take to answer, in milliseconds.
    jwks_delay_ms: std::sync::atomic::AtomicU64,
    /// Where the discovery document says the keys are, when not here.
    keys_at: Mutex<Option<String>>,
    /// Where the keys' address redirects to, when it does.
    keys_moved_to: Mutex<Option<String>>,
    /// Bytes of padding in the keys' answer.
    keys_padding: AtomicUsize,
    /// The client this provider answers, and its secret.
    client: Mutex<(String, String)>,
    /// Codes given out and not yet redeemed: the nonce, the PKCE challenge and the
    /// redirect address each was asked with.
    codes: Mutex<std::collections::HashMap<String, (String, String, String)>>,
    /// The claims of whoever is at the keyboard, over a member's.
    person: Mutex<serde_json::Map<String, Value>>,
    /// What the last request to authorize asked for.
    asked: Mutex<std::collections::HashMap<String, String>>,
}

/// The front door: gives a code and sends the browser back.
async fn authorize(
    State(idp): State<Arc<IdpState>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    *idp.asked.lock().unwrap() = q.clone();
    let mut codes = idp.codes.lock().unwrap();
    let code = format!(
        "code-{}",
        codes.len() + 1 + idp.jwks_hits.load(Ordering::SeqCst)
    );
    codes.insert(
        code.clone(),
        (get("nonce"), get("code_challenge"), get("redirect_uri")),
    );
    let to = format!("{}?code={code}&state={}", get("redirect_uri"), get("state"));
    (StatusCode::FOUND, [(axum::http::header::LOCATION, to)]).into_response()
}

/// The back door: a code, the client's secret and the PKCE verifier for an ID token.
async fn token(
    State(idp): State<Arc<IdpState>>,
    Path(tenant): Path<String>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> Response {
    use sha2::Digest;
    let get = |k: &str| form.get(k).cloned().unwrap_or_default();
    let (client, secret) = idp.client.lock().unwrap().clone();
    let Some((nonce, challenge, redirect)) = idp.codes.lock().unwrap().remove(&get("code")) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let proved = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(get("code_verifier").as_bytes()));
    if get("grant_type") != "authorization_code"
        || get("client_id") != client
        || get("client_secret") != secret
        || get("redirect_uri") != redirect
        || proved != challenge
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let base = idp.base.lock().unwrap().clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut claims = member_claims(&base, &tenant, &client, "", "", &nonce, now);
    for (k, v) in idp.person.lock().unwrap().iter() {
        claims[k] = v.clone();
    }
    let id_token = sign(&claims, keys::KEY_A_DER, "key-a");
    Json(json!({"token_type": "Bearer", "id_token": id_token})).into_response()
}

/// A fake Entra tenant: `GET /{tenant}/v2.0/.well-known/openid-configuration` and
/// `GET /{tenant}/discovery/v2.0/keys`, on loopback. Stops when dropped.
pub struct FakeIdp {
    /// The authority: `http://127.0.0.1:<port>`.
    pub base: String,
    state: Arc<IdpState>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for FakeIdp {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn discovery(State(idp): State<Arc<IdpState>>, Path(tenant): Path<String>) -> Response {
    idp.discovery_hits.fetch_add(1, Ordering::SeqCst);
    if idp.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if tenant != *idp.tenant.lock().unwrap() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let base = idp.base.lock().unwrap().clone();
    Json(json!({
        "issuer": format!("{base}/{tenant}/v2.0"),
        "authorization_endpoint": format!("{base}/{tenant}/oauth2/v2.0/authorize"),
        "token_endpoint": format!("{base}/{tenant}/oauth2/v2.0/token"),
        "jwks_uri": idp
            .keys_at
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(format!("{base}/{tenant}/discovery/v2.0/keys")),
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
    .into_response()
}

async fn jwks(State(idp): State<Arc<IdpState>>) -> Response {
    idp.jwks_hits.fetch_add(1, Ordering::SeqCst);
    let delay = idp.jwks_delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    if idp.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(to) = idp.keys_moved_to.lock().unwrap().clone() {
        return (StatusCode::FOUND, [(axum::http::header::LOCATION, to)]).into_response();
    }
    let padding = "x".repeat(idp.keys_padding.load(Ordering::SeqCst));
    Json(json!({ "keys": *idp.keys.lock().unwrap(), "padding": padding })).into_response()
}

impl FakeIdp {
    /// A tenant publishing key A as `key-a`.
    pub async fn start(tenant: &str) -> Self {
        let state = Arc::new(IdpState::default());
        *state.tenant.lock().unwrap() = tenant.to_string();
        state.keys.lock().unwrap().push(jwk("key-a", keys::KEY_A_N));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake identity provider");
        let base = format!("http://{}", listener.local_addr().unwrap());
        *state.base.lock().unwrap() = base.clone();
        let app = Router::new()
            .route(
                "/{tenant}/v2.0/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/{tenant}/discovery/v2.0/keys", get(jwks))
            .route("/{tenant}/oauth2/v2.0/authorize", get(authorize))
            .route("/{tenant}/oauth2/v2.0/token", axum::routing::post(token))
            .with_state(state.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            base,
            state,
            stop: Some(stop),
        }
    }

    /// Publish exactly these keys.
    pub fn set_keys(&self, keys: Vec<Value>) {
        *self.state.keys.lock().unwrap() = keys;
    }

    /// Answer `503` to everything (or stop doing so).
    pub fn set_down(&self, down: bool) {
        self.state.down.store(down, Ordering::SeqCst);
    }

    /// Make the keys answer only after `delay` (a slow tenant).
    pub fn set_jwks_delay(&self, delay: std::time::Duration) {
        self.state
            .jwks_delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }

    /// The client this provider redeems codes for.
    pub fn set_client(&self, client_id: &str, secret: &str) {
        *self.state.client.lock().unwrap() = (client_id.into(), secret.into());
    }

    /// A claim of whoever signs in next, over a member's: their `oid`, or one to leave
    /// wrong.
    pub fn set_claim(&self, name: &str, value: Value) {
        self.state.person.lock().unwrap().insert(name.into(), value);
    }

    /// What the last request to authorize asked for.
    pub fn asked(&self) -> std::collections::HashMap<String, String> {
        self.state.asked.lock().unwrap().clone()
    }

    /// Have the discovery document name another address for the keys.
    pub fn set_keys_at(&self, url: &str) {
        *self.state.keys_at.lock().unwrap() = Some(url.to_string());
    }

    /// Answer a request for the keys with a redirect.
    pub fn set_keys_moved_to(&self, url: &str) {
        *self.state.keys_moved_to.lock().unwrap() = Some(url.to_string());
    }

    pub fn set_keys_padding(&self, bytes: usize) {
        self.state.keys_padding.store(bytes, Ordering::SeqCst);
    }

    /// Where this provider serves its keys.
    pub fn keys_url(&self) -> String {
        format!(
            "{}/{}/discovery/v2.0/keys",
            self.base,
            self.state.tenant.lock().unwrap()
        )
    }

    pub fn jwks_hits(&self) -> usize {
        self.state.jwks_hits.load(Ordering::SeqCst)
    }

    pub fn discovery_hits(&self) -> usize {
        self.state.discovery_hits.load(Ordering::SeqCst)
    }
}
