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
        "jwks_uri": format!("{base}/{tenant}/discovery/v2.0/keys"),
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
    .into_response()
}

async fn jwks(State(idp): State<Arc<IdpState>>) -> Response {
    idp.jwks_hits.fetch_add(1, Ordering::SeqCst);
    if idp.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({ "keys": *idp.keys.lock().unwrap() })).into_response()
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

    pub fn jwks_hits(&self) -> usize {
        self.state.jwks_hits.load(Ordering::SeqCst)
    }

    pub fn discovery_hits(&self) -> usize {
        self.state.discovery_hits.load(Ordering::SeqCst)
    }
}
