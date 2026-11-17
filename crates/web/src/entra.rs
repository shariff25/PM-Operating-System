//! Microsoft Entra ID (Azure AD) sign-in: the OpenID Connect authorization code flow with
//! PKCE, as a confidential client of one tenant (`docs/entra-sign-in.md`).
//!
//! 1. `GET /api/auth/login` makes a random `state`, `nonce` and PKCE verifier, keeps them in a
//!    short-lived signed `__Host-pmos-entra` cookie (ten minutes) and redirects to the
//!    tenant's authorize endpoint with the S256 challenge.
//! 2. `GET /api/auth/callback` checks `state` against that cookie (its CSRF defence: the
//!    callback is a cross-site GET, so no origin check applies), refuses a `state` already
//!    used, exchanges the code with the client secret and the verifier, and validates the
//!    ID token: RS256 only, signed by a key in the tenant's JWKS, `iss` exactly the tenant's
//!    v2.0 issuer, `aud` the client id, `exp`/`nbf`/`iat` within 60 seconds of skew, the
//!    flow's `nonce`, and `tid` the configured tenant. Then the claims PM OS keys on
//!    (`docs/entra-sign-in.md`, "What Microsoft says about the claims"): `oid` must be a GUID;
//!    the optional `acct` claim must be present (else `entra_misconfigured`) and `0`, a member
//!    of the tenant (a guest, `1`, is `not_a_member`); and `idp`, when present, must name this
//!    tenant (the token's own issuer, or this tenant's v1 issuer `https://sts.windows.net/<tid>/`),
//!    else `not_a_member`.
//! 3. The app looks the person up in the user database by `(tid, oid)`, and only for a first
//!    link by `preferred_username`. The `email` claim is never read. Entra never decides
//!    membership or roles.
//!
//! Tokens, codes, verifiers and the client secret are never logged; failures log a short
//! fixed reason.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use axum::http::HeaderMap;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use pmos_contract::model::valid_email;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::{AuthProvider, Claimed, SignInError, SignInInput};
use crate::session;

/// Microsoft's public cloud.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";
/// The cookie that carries a sign-in in progress.
pub const FLOW_COOKIE: &str = "__Host-pmos-entra";
/// How long a sign-in may take, from `/api/auth/login` to the callback.
pub const FLOW_SECONDS: i64 = 600;
/// The clock skew allowed on `exp`, `nbf` and `iat`.
pub const SKEW_SECONDS: i64 = 60;
/// The scopes asked for: an ID token with the person's `oid` and `preferred_username`
/// (`profile`), nothing else. Not `email`: that claim is never read.
pub const SCOPES: &str = "openid profile";
/// How long the discovery document and the keys are kept before a fresh fetch.
const METADATA_TTL: StdDuration = StdDuration::from_secs(24 * 60 * 60);
/// The largest discovery, JWKS or token response read.
const MAX_RESPONSE: usize = 256 * 1024;
/// The longest post-login path kept.
const MAX_RETURN: usize = 512;

type HmacSha256 = Hmac<Sha256>;

/// Entra ID settings (`PMOS_WEB_ENTRA_*`), checked at start-up. `Debug` never shows the
/// secret.
#[derive(Clone)]
pub struct EntraConfig {
    /// The directory (tenant) id, a GUID, lower-cased. `common`, `organizations` and
    /// `consumers` are refused: this is a single-tenant app.
    pub tenant_id: String,
    /// The application (client) id, a GUID, lower-cased.
    pub client_id: String,
    client_secret: String,
    /// `PMOS_WEB_ENTRA_AUTHORITY`, default [`DEFAULT_AUTHORITY`]: an origin, `https` unless
    /// loopback (tests point it at a mock).
    pub authority: String,
    /// `<PMOS_WEB_PUBLIC_URL>/api/auth/callback`.
    pub redirect_uri: String,
    /// The least time between two fetches of the keys when a token names an unknown `kid`.
    pub jwks_refetch_after: StdDuration,
}

impl std::fmt::Debug for EntraConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntraConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("authority", &self.authority)
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

/// The variables that configure Entra ID.
pub const VARS: [&str; 3] = [
    "PMOS_WEB_ENTRA_TENANT_ID",
    "PMOS_WEB_ENTRA_CLIENT_ID",
    "PMOS_WEB_ENTRA_CLIENT_SECRET",
];
pub const AUTHORITY_VAR: &str = "PMOS_WEB_ENTRA_AUTHORITY";

impl EntraConfig {
    /// `None` when no `PMOS_WEB_ENTRA_*` variable is set. Any of them set without the rest
    /// (or without `PMOS_WEB_PUBLIC_URL`) is an error: a half-configured provider never
    /// starts. Errors never contain the secret.
    pub fn from_lookup(
        set: impl Fn(&str) -> Option<String>,
        public_url: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let any = VARS
            .iter()
            .chain([&AUTHORITY_VAR])
            .any(|k| set(k).is_some());
        if !any {
            return Ok(None);
        }
        let missing: Vec<&str> = VARS.iter().copied().filter(|k| set(k).is_none()).collect();
        if !missing.is_empty() {
            return Err(format!(
                "Entra ID sign-in is half-configured: {} also needed (or unset every \
                 PMOS_WEB_ENTRA_* variable)",
                missing.join(", ")
            ));
        }
        let public_url = public_url.ok_or(
            "PMOS_WEB_PUBLIC_URL is required with Entra ID sign-in: the redirect URI is \
             <PMOS_WEB_PUBLIC_URL>/api/auth/callback",
        )?;
        if !secure_or_loopback(public_url) {
            return Err(
                "PMOS_WEB_PUBLIC_URL must be https with Entra ID sign-in (http only on loopback)"
                    .into(),
            );
        }
        let guid = |k: &str| -> Result<String, String> {
            let v = set(k).unwrap_or_default().trim().to_ascii_lowercase();
            if matches!(v.as_str(), "common" | "organizations" | "consumers") {
                return Err(format!(
                    "{k} must be your tenant's own id: `{v}` is a multi-tenant authority, which \
                     PM OS refuses"
                ));
            }
            if !is_guid(&v) {
                return Err(format!(
                    "{k} must be a GUID such as 00000000-0000-0000-0000-000000000000"
                ));
            }
            Ok(v)
        };
        let tenant_id = guid("PMOS_WEB_ENTRA_TENANT_ID")?;
        let client_id = guid("PMOS_WEB_ENTRA_CLIENT_ID")?;
        let client_secret = set("PMOS_WEB_ENTRA_CLIENT_SECRET")
            .unwrap_or_default()
            .trim()
            .to_string();
        let authority = match set(AUTHORITY_VAR) {
            None => DEFAULT_AUTHORITY.to_string(),
            Some(a) => {
                let url = reqwest::Url::parse(a.trim())
                    .map_err(|_| format!("{AUTHORITY_VAR} must be a URL"))?;
                if url.path() != "/"
                    || url.query().is_some()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err(format!(
                        "{AUTHORITY_VAR} must be an origin such as {DEFAULT_AUTHORITY}, with no \
                         path"
                    ));
                }
                let origin = url.origin().ascii_serialization();
                if !secure_or_loopback(&origin) {
                    return Err(format!(
                        "{AUTHORITY_VAR} must be https unless it is loopback"
                    ));
                }
                origin
            }
        };
        Ok(Some(Self {
            tenant_id,
            client_id,
            client_secret,
            authority,
            redirect_uri: format!("{}/api/auth/callback", public_url.trim_end_matches('/')),
            jwks_refetch_after: StdDuration::from_secs(60),
        }))
    }

    /// The only issuer accepted: `<authority>/<tenant id>/v2.0`.
    pub fn issuer(&self) -> String {
        format!("{}/{}/v2.0", self.authority, self.tenant_id)
    }

    fn discovery_url(&self) -> String {
        format!("{}/.well-known/openid-configuration", self.issuer())
    }
}

fn is_guid(v: &str) -> bool {
    let groups: Vec<&str> = v.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some(h) if h.eq_ignore_ascii_case("localhost") => true,
        Some(h) => h
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// `https`, or `http` to a loopback host.
fn secure_or_loopback(value: &str) -> bool {
    reqwest::Url::parse(value)
        .is_ok_and(|u| u.scheme() == "https" || (u.scheme() == "http" && is_loopback_host(&u)))
}

/// The post-login path: a same-origin relative path (`/…`, never `//…` or `/\…`, no
/// control characters, at most 512 bytes), else `/`.
pub fn safe_return(path: Option<&str>) -> String {
    match path {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && p.len() <= MAX_RETURN
                && !p.chars().any(|c| c.is_control() || c.is_whitespace()) =>
        {
            p.to_string()
        }
        _ => "/".to_string(),
    }
}

// The flow cookie.

/// A sign-in in progress, as the flow cookie carries it.
#[derive(Serialize, Deserialize)]
struct Flow {
    /// `state`, `nonce`, PKCE verifier, return path, expiry (Unix seconds).
    s: String,
    n: String,
    v: String,
    r: String,
    exp: i64,
}

/// What [`Entra::finish`] hands the callback when the browser's flow checks out.
pub struct Started {
    pub nonce: String,
    pub verifier: String,
    pub return_to: String,
}

/// Why a sign-in didn't finish. `reason()` is what the app is told
/// (`/#/signed-out?reason=…`); the `&'static str` is a fixed phrase for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No flow cookie, or one past its ten minutes.
    Expired,
    /// The person cancelled at Microsoft (`error=access_denied`).
    Cancelled,
    /// Microsoft or the network failed.
    Unavailable(&'static str),
    /// Anything else: a bad state, code or token.
    Failed(&'static str),
    /// The token has no `acct` claim: the app registration doesn't add the optional claim,
    /// so PM OS can't tell a member from a guest and refuses everyone (`entra_misconfigured`).
    Misconfigured,
    /// A guest (`acct` not `0`), or an account from another identity provider (`idp`):
    /// `not_a_member`, before any lookup.
    NotMember(&'static str),
}

impl Failure {
    pub fn reason(&self) -> &'static str {
        match self {
            Failure::Expired => "expired",
            Failure::Cancelled => "cancelled",
            Failure::Unavailable(_) => "unavailable",
            Failure::Failed(_) => "failed",
            Failure::Misconfigured => "entra_misconfigured",
            Failure::NotMember(_) => "not_a_member",
        }
    }

    pub fn why(&self) -> &'static str {
        match self {
            Failure::Expired => "no sign-in in progress, or it expired",
            Failure::Cancelled => "cancelled at the identity provider",
            Failure::Misconfigured => {
                "the ID token has no acct claim (add it in Token configuration)"
            }
            Failure::Unavailable(w) | Failure::Failed(w) | Failure::NotMember(w) => w,
        }
    }
}

/// `Set-Cookie` for a sign-in in progress.
fn flow_cookie(value: &str) -> String {
    format!("{FLOW_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={FLOW_SECONDS}")
}

/// `Set-Cookie` that removes the flow cookie.
pub fn clear_flow_cookie() -> String {
    format!("{FLOW_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.trim().to_string())
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 code challenge of a PKCE verifier.
pub fn challenge(verifier: &str) -> String {
    b64(&Sha256::digest(verifier.as_bytes()))
}

// Provider metadata and keys.

#[derive(Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Default)]
struct KeyCache {
    /// RSA signing keys by `kid`: `(n, e)`.
    keys: HashMap<String, (String, String)>,
    fetched: Option<Instant>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct IdClaims {
    iss: String,
    aud: Audience,
    exp: i64,
    iat: i64,
    nbf: Option<i64>,
    nonce: Option<String>,
    tid: Option<String>,
    /// The person's object id in the tenant: stable, never reassigned, unlike a name.
    oid: Option<String>,
    /// The optional claim: `0` for a member of the tenant, `1` for a guest. A number in
    /// practice; a string `"0"` is read the same.
    acct: Option<serde_json::Value>,
    /// The identity provider that authenticated the person, when it isn't the tenant itself
    /// (absent, or equal to the issuer, for a member).
    idp: Option<String>,
    preferred_username: Option<String>,
    // `email` is deliberately not read: Microsoft says it is unverified and mutable, and it
    // must never decide who someone is (`docs/entra-sign-in.md`).
}

/// Who a checked ID token names: its `(tid, oid)`, the key PM OS signs people in by, and its
/// `preferred_username`, used only to find a person for their first link. `Debug` shows
/// neither the `oid` nor the name.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// The `oid` claim, lower-cased: always a GUID.
    pub oid: String,
    /// The `tid` claim, lower-cased: always the configured tenant.
    pub tid: String,
    /// The `preferred_username` claim, trimmed and lower-cased, when it is an email address.
    pub preferred_username: Option<String>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("oid", &"<redacted>")
            .field("tid", &self.tid)
            .field("preferred_username", &"<redacted>")
            .finish()
    }
}

/// Is `acct` a member's (`0`)? `Some(false)` for a guest or any other value.
fn acct_is_member(acct: &serde_json::Value) -> bool {
    acct.as_i64() == Some(0) || acct.as_str().map(str::trim) == Some("0")
}

/// The Entra ID provider: builds the authorize redirect, checks the callback's flow cookie,
/// exchanges the code and validates the ID token.
pub struct Entra {
    config: EntraConfig,
    http: reqwest::Client,
    /// Signs the flow cookie: derived from the client secret, so every `pmos-web` with the
    /// same configuration accepts the others' cookies and nothing new needs storing.
    flow_key: [u8; 32],
    metadata: tokio::sync::Mutex<Option<(Instant, Discovery)>>,
    keys: tokio::sync::Mutex<KeyCache>,
    /// The hashes of `state`s already used, with their expiry: a state works once.
    used: Mutex<HashMap<[u8; 32], i64>>,
}

impl Entra {
    pub fn new(config: EntraConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(StdDuration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        let mut mac = HmacSha256::new_from_slice(config.client_secret.as_bytes())
            .expect("HMAC takes any key length");
        mac.update(b"pmos-web entra flow cookie v1");
        let flow_key = mac.finalize().into_bytes().into();
        Ok(Self {
            config,
            http,
            flow_key,
            metadata: tokio::sync::Mutex::new(None),
            keys: tokio::sync::Mutex::new(KeyCache::default()),
            used: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &EntraConfig {
        &self.config
    }

    fn mac(&self, payload: &str) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.flow_key).expect("HMAC takes any key");
        mac.update(payload.as_bytes());
        mac
    }

    /// Start a sign-in: the authorize URL to redirect to and the flow cookie to set.
    pub async fn begin(
        &self,
        now: DateTime<Utc>,
        return_to: Option<&str>,
    ) -> Result<(String, String), Failure> {
        let meta = self.discovery().await?;
        let flow = Flow {
            s: session::random_token(),
            n: session::random_token(),
            v: session::random_token(),
            r: safe_return(return_to),
            exp: now.timestamp() + FLOW_SECONDS,
        };
        let mut url = reqwest::Url::parse(&meta.authorization_endpoint)
            .map_err(|_| Failure::Unavailable("bad authorization endpoint"))?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.config.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", SCOPES)
            .append_pair("state", &flow.s)
            .append_pair("nonce", &flow.n)
            .append_pair("code_challenge", &challenge(&flow.v))
            .append_pair("code_challenge_method", "S256");
        let payload = b64(&serde_json::to_vec(&flow).expect("a flow serializes"));
        let tag = b64(&self.mac(&payload).finalize().into_bytes());
        Ok((url.into(), flow_cookie(&format!("{payload}.{tag}"))))
    }

    /// Check the callback's `state` against the browser's flow cookie, once.
    pub fn finish(
        &self,
        headers: &HeaderMap,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<Started, Failure> {
        let Some(cookie) = read_cookie(headers, FLOW_COOKIE) else {
            return Err(Failure::Expired);
        };
        let (payload, tag) = cookie
            .split_once('.')
            .ok_or(Failure::Failed("malformed flow cookie"))?;
        let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| Failure::Failed("malformed flow cookie"))?;
        self.mac(payload)
            .verify_slice(&tag)
            .map_err(|_| Failure::Failed("flow cookie signature"))?;
        let flow: Flow = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(Failure::Failed("malformed flow cookie"))?;
        if now.timestamp() >= flow.exp {
            return Err(Failure::Expired);
        }
        if !pmos_runtime::token_eq(state, &flow.s) {
            return Err(Failure::Failed("state does not match"));
        }
        let hash: [u8; 32] = Sha256::digest(flow.s.as_bytes()).into();
        {
            let mut used = self.used.lock().expect("used states lock");
            let t = now.timestamp();
            used.retain(|_, exp| *exp > t);
            if used.insert(hash, flow.exp).is_some() {
                return Err(Failure::Failed("state already used"));
            }
        }
        Ok(Started {
            nonce: flow.n,
            verifier: flow.v,
            return_to: safe_return(Some(&flow.r)),
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        what: &'static str,
    ) -> Result<T, Failure> {
        let res = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| Failure::Unavailable(what))?;
        if !res.status().is_success() {
            return Err(Failure::Unavailable(what));
        }
        let body = res.bytes().await.map_err(|_| Failure::Unavailable(what))?;
        if body.len() > MAX_RESPONSE {
            return Err(Failure::Unavailable(what));
        }
        serde_json::from_slice(&body).map_err(|_| Failure::Unavailable(what))
    }

    /// The tenant's discovery document, cached for a day. Its issuer must be exactly the
    /// configured one, and every endpoint on the authority's own origin.
    async fn discovery(&self) -> Result<Discovery, Failure> {
        let mut cached = self.metadata.lock().await;
        if let Some((at, meta)) = cached.as_ref() {
            if at.elapsed() < METADATA_TTL {
                return Ok(meta.clone());
            }
        }
        let meta: Discovery = self
            .get_json(
                &self.config.discovery_url(),
                "discovery document unavailable",
            )
            .await?;
        if meta.issuer != self.config.issuer() {
            tracing::error!(
                expected = %self.config.issuer(),
                "Entra discovery document names another issuer"
            );
            return Err(Failure::Unavailable("discovery issuer mismatch"));
        }
        let ours = |u: &str| {
            reqwest::Url::parse(u)
                .is_ok_and(|u| u.origin().ascii_serialization() == self.config.authority)
        };
        if ![
            &meta.authorization_endpoint,
            &meta.token_endpoint,
            &meta.jwks_uri,
        ]
        .iter()
        .all(|u| ours(u))
        {
            tracing::error!("Entra discovery document points off the authority");
            return Err(Failure::Unavailable(
                "discovery endpoints off the authority",
            ));
        }
        *cached = Some((Instant::now(), meta.clone()));
        Ok(meta)
    }

    /// The key a token's `kid` names. An unknown `kid` refetches the keys, at most once per
    /// `jwks_refetch_after`, so a stream of bad tokens can't hammer Microsoft.
    async fn key(&self, kid: &str) -> Result<DecodingKey, Failure> {
        let mut cache = self.keys.lock().await;
        let stale = cache.fetched.is_none_or(|at| at.elapsed() >= METADATA_TTL);
        let may_refetch = cache
            .fetched
            .is_none_or(|at| at.elapsed() >= self.config.jwks_refetch_after);
        if stale || (!cache.keys.contains_key(kid) && may_refetch) {
            let meta = self.discovery().await?;
            let jwks: Jwks = self.get_json(&meta.jwks_uri, "keys unavailable").await?;
            cache.keys = jwks
                .keys
                .into_iter()
                .filter(|k| {
                    k.kty == "RSA"
                        && k.use_.as_deref().is_none_or(|u| u == "sig")
                        && k.alg.as_deref().is_none_or(|a| a == "RS256")
                })
                .filter_map(|k| Some((k.kid?, (k.n?, k.e?))))
                .collect();
            cache.fetched = Some(Instant::now());
        }
        let (n, e) = cache
            .keys
            .get(kid)
            .ok_or(Failure::Failed("unknown signing key"))?;
        DecodingKey::from_rsa_components(n, e).map_err(|_| Failure::Failed("unusable signing key"))
    }

    /// Exchange the code for an ID token (client secret and PKCE verifier).
    async fn exchange(&self, code: &str, verifier: &str) -> Result<String, Failure> {
        let meta = self.discovery().await?;
        let res = self
            .http
            .post(&meta.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.config.redirect_uri),
                ("client_id", &self.config.client_id),
                ("client_secret", &self.config.client_secret),
                ("code_verifier", verifier),
                ("scope", SCOPES),
            ])
            .send()
            .await
            .map_err(|_| Failure::Unavailable("token endpoint unreachable"))?;
        let status = res.status();
        let body = res
            .bytes()
            .await
            .map_err(|_| Failure::Unavailable("token endpoint unreachable"))?;
        if body.len() > MAX_RESPONSE {
            return Err(Failure::Failed("token response too large"));
        }
        if status.is_server_error() {
            return Err(Failure::Unavailable("token endpoint failed"));
        }
        if !status.is_success() {
            // Only Entra's error code, never its description or anything sent.
            let code = serde_json::from_slice::<TokenError>(&body)
                .ok()
                .and_then(|e| e.error)
                .filter(|e| {
                    e.len() <= 64 && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
            tracing::warn!(status = status.as_u16(), error = ?code, "Entra refused the code");
            return Err(Failure::Failed("code refused"));
        }
        serde_json::from_slice::<TokenResponse>(&body)
            .ok()
            .and_then(|t| t.id_token)
            .ok_or(Failure::Failed("no id token"))
    }

    /// This tenant's v1 issuer, `https://sts.windows.net/<tid>/`: what `idp` may also hold for
    /// a member (it names the same tenant).
    fn v1_issuer(&self) -> String {
        format!("https://sts.windows.net/{}/", self.config.tenant_id)
    }

    /// Validate an ID token and return who it names: `(tid, oid)` and `preferred_username`.
    pub async fn validate_identity(
        &self,
        token: &str,
        nonce: &str,
        now: DateTime<Utc>,
    ) -> Result<Identity, Failure> {
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| Failure::Failed("malformed token"))?;
        if header.alg != Algorithm::RS256 {
            return Err(Failure::Failed("token not RS256"));
        }
        let kid = header.kid.ok_or(Failure::Failed("token without kid"))?;
        let key = self.key(&kid).await?;
        // The signature only: every claim is checked below against the app's clock.
        let mut v = Validation::new(Algorithm::RS256);
        v.validate_exp = false;
        v.validate_nbf = false;
        v.validate_aud = false;
        v.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<IdClaims>(token, &key, &v)
            .map_err(|_| Failure::Failed("token signature or shape"))?
            .claims;
        if claims.iss != self.config.issuer() {
            return Err(Failure::Failed("token issuer"));
        }
        let aud_ok = match &claims.aud {
            Audience::One(a) => *a == self.config.client_id,
            Audience::Many(list) => list.len() == 1 && list[0] == self.config.client_id,
        };
        if !aud_ok {
            return Err(Failure::Failed("token audience"));
        }
        let t = now.timestamp();
        if t >= claims.exp + SKEW_SECONDS {
            return Err(Failure::Failed("token expired"));
        }
        if claims.nbf.is_some_and(|nbf| t + SKEW_SECONDS < nbf) {
            return Err(Failure::Failed("token not yet valid"));
        }
        if t + SKEW_SECONDS < claims.iat || claims.iat > claims.exp {
            return Err(Failure::Failed("token issued in the future"));
        }
        if !claims
            .nonce
            .as_deref()
            .is_some_and(|n| pmos_runtime::token_eq(n, nonce))
        {
            return Err(Failure::Failed("token nonce"));
        }
        if !claims
            .tid
            .as_deref()
            .is_some_and(|tid| tid.eq_ignore_ascii_case(&self.config.tenant_id))
        {
            return Err(Failure::Failed("token tenant"));
        }
        let oid = claims
            .oid
            .map(|o| o.trim().to_ascii_lowercase())
            .filter(|o| is_guid(o))
            .ok_or(Failure::Failed("token without an object id"))?;
        // Members only (spec §7): the optional `acct` claim must be there, so a registration
        // that forgot it fails closed rather than letting guests through.
        match &claims.acct {
            None => return Err(Failure::Misconfigured),
            Some(acct) if !acct_is_member(acct) => {
                return Err(Failure::NotMember("a guest account"))
            }
            Some(_) => {}
        }
        // `idp` is absent for a member, or names this tenant; anything else is someone who
        // signed in with another identity provider.
        if let Some(idp) = claims.idp.as_deref().map(str::trim) {
            if idp != claims.iss && !idp.eq_ignore_ascii_case(&self.v1_issuer()) {
                return Err(Failure::NotMember("another identity provider"));
            }
        }
        let preferred_username = claims
            .preferred_username
            .map(|e| e.trim().to_lowercase())
            .filter(|e| valid_email(e));
        Ok(Identity {
            oid,
            tid: self.config.tenant_id.to_ascii_lowercase(),
            preferred_username,
        })
    }

    /// Exchange the code and validate the ID token: who it names.
    pub async fn sign_in_identity(
        &self,
        code: &str,
        nonce: &str,
        verifier: &str,
        now: DateTime<Utc>,
    ) -> Result<Identity, Failure> {
        if code.is_empty() || code.len() > 4096 {
            return Err(Failure::Failed("malformed code"));
        }
        let token = self.exchange(code, verifier).await?;
        self.validate_identity(&token, nonce, now).await
    }
}

#[async_trait]
impl AuthProvider for Entra {
    fn name(&self) -> &'static str {
        "entra"
    }

    fn is_insecure(&self) -> bool {
        false
    }

    async fn authenticate(&self, input: &SignInInput) -> Result<Claimed, SignInError> {
        let SignInInput::Entra {
            code,
            nonce,
            verifier,
            now,
        } = input
        else {
            return Err(SignInError::Unsupported);
        };
        // The Entra key's `oid`: the callback looks the person up by `(tid, oid)` itself.
        match self.sign_in_identity(code, nonce, verifier, *now).await {
            Ok(identity) => Ok(Claimed(identity.oid)),
            Err(f) => Err(SignInError::Rejected(f)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.trim().is_empty())
        }
    }

    const TENANT: &str = "11111111-2222-3333-4444-555555555555";
    const CLIENT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const SECRET: &str = "the-client-secret-value";

    fn full() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PMOS_WEB_ENTRA_TENANT_ID", TENANT),
            ("PMOS_WEB_ENTRA_CLIENT_ID", CLIENT),
            ("PMOS_WEB_ENTRA_CLIENT_SECRET", SECRET),
        ]
    }

    #[test]
    fn reads_a_full_config_and_hides_the_secret() {
        let cfg = EntraConfig::from_lookup(lookup(&full()), Some("https://pmos.example.com"))
            .unwrap()
            .unwrap();
        assert_eq!(
            cfg.redirect_uri,
            "https://pmos.example.com/api/auth/callback"
        );
        assert_eq!(
            cfg.issuer(),
            format!("https://login.microsoftonline.com/{TENANT}/v2.0")
        );
        assert!(!format!("{cfg:?}").contains(SECRET));
        assert!(EntraConfig::from_lookup(lookup(&[]), None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn refuses_half_configs_and_multi_tenant_authorities() {
        let public = Some("https://pmos.example.com");
        let err = EntraConfig::from_lookup(lookup(&full()[..2]), public).unwrap_err();
        assert!(
            err.contains("half-configured") && err.contains("CLIENT_SECRET"),
            "{err}"
        );
        let err = EntraConfig::from_lookup(lookup(&full()), None).unwrap_err();
        assert!(err.contains("PMOS_WEB_PUBLIC_URL is required"), "{err}");
        let err = EntraConfig::from_lookup(
            lookup(&[(AUTHORITY_VAR, "https://login.microsoftonline.com")]),
            public,
        )
        .unwrap_err();
        assert!(err.contains("half-configured"), "{err}");
        for tenant in ["common", "organizations", "Consumers"] {
            let mut pairs = full();
            pairs[0].1 = tenant;
            let err = EntraConfig::from_lookup(lookup(&pairs), public).unwrap_err();
            assert!(err.contains("multi-tenant"), "{err}");
        }
        let mut pairs = full();
        pairs[1].1 = "my-app";
        assert!(EntraConfig::from_lookup(lookup(&pairs), public)
            .unwrap_err()
            .contains("GUID"));
        let err =
            EntraConfig::from_lookup(lookup(&full()), Some("http://pmos.example.com")).unwrap_err();
        assert!(err.contains("https"), "{err}");
        assert!(
            EntraConfig::from_lookup(lookup(&full()), Some("http://127.0.0.1:8770"))
                .unwrap()
                .is_some()
        );
        for (authority, want) in [
            ("http://idp.example.com", "https unless"),
            ("https://idp.example.com/tenant", "no path"),
        ] {
            let pairs = [full(), vec![(AUTHORITY_VAR, authority)]].concat();
            let err = EntraConfig::from_lookup(lookup(&pairs), public).unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(!err.contains(SECRET));
        }
        let pairs = [full(), vec![(AUTHORITY_VAR, "http://127.0.0.1:9999/")]].concat();
        let cfg = EntraConfig::from_lookup(lookup(&pairs), public)
            .unwrap()
            .unwrap();
        assert_eq!(cfg.authority, "http://127.0.0.1:9999");
    }

    #[test]
    fn return_paths_stay_on_this_origin() {
        assert_eq!(safe_return(Some("/drafts")), "/drafts");
        assert_eq!(safe_return(Some("/#/drafts/apr_1")), "/#/drafts/apr_1");
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "evil",
            "/a\nb",
            "/a b",
            "",
        ] {
            assert_eq!(safe_return(Some(bad)), "/", "{bad}");
        }
        assert_eq!(safe_return(Some(&format!("/{}", "a".repeat(600)))), "/");
        assert_eq!(safe_return(None), "/");
    }

    #[test]
    fn pkce_challenge_is_s256() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
