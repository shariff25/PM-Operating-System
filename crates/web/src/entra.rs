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
//!    else `not_a_member`. These checks live in `pmos-entra` (`crates/entra`), the one copy
//!    the tool server uses too.
//! 3. The app looks the person up in the user database by `(tid, oid)`, and only for a first
//!    link by `preferred_username`. A first link sends the tool server the raw ID token and
//!    the flow's `nonce`, never a bare `(tid, oid)`: the tool server validates the token
//!    itself and links only the identity it names. The `email` claim is never read. Entra
//!    never decides membership or roles.
//!
//! Tokens, codes, verifiers and the client secret are never logged; failures log a short
//! fixed reason.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use axum::http::HeaderMap;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::{AuthProvider, Claimed, SignInError, SignInInput};
use crate::session;

/// The ID token rules, shared with the tool server (`crates/entra`): one copy, so the two
/// can't drift.
pub use pmos_entra::{Failure, Identity, Tenant, DEFAULT_AUTHORITY, SKEW_SECONDS};

/// The cookie that carries a sign-in in progress.
pub const FLOW_COOKIE: &str = "__Host-pmos-entra";
/// How long a sign-in may take, from `/api/auth/login` to the callback.
pub const FLOW_SECONDS: i64 = 600;
/// The scopes asked for: an ID token with the person's `oid` and `preferred_username`
/// (`profile`), nothing else. Not `email`: that claim is never read.
pub const SCOPES: &str = "openid profile";
/// The largest token response read.
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
        if !pmos_entra::secure_or_loopback(public_url) {
            return Err(
                "PMOS_WEB_PUBLIC_URL must be https with Entra ID sign-in (http only on loopback)"
                    .into(),
            );
        }
        let tenant = Tenant::from_settings(
            [VARS[0], VARS[1], AUTHORITY_VAR],
            &set(VARS[0]).unwrap_or_default(),
            &set(VARS[1]).unwrap_or_default(),
            set(AUTHORITY_VAR).as_deref(),
        )?;
        let client_secret = set("PMOS_WEB_ENTRA_CLIENT_SECRET")
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(Some(Self {
            tenant_id: tenant.tenant_id,
            client_id: tenant.client_id,
            client_secret,
            authority: tenant.authority,
            redirect_uri: format!("{}/api/auth/callback", public_url.trim_end_matches('/')),
            jwks_refetch_after: tenant.jwks_refetch_after,
        }))
    }

    /// The tenant and application tokens must be for.
    pub fn tenant(&self) -> Tenant {
        Tenant {
            tenant_id: self.tenant_id.clone(),
            client_id: self.client_id.clone(),
            authority: self.authority.clone(),
            jwks_refetch_after: self.jwks_refetch_after,
        }
    }

    /// The only issuer accepted: `<authority>/<tenant id>/v2.0`.
    pub fn issuer(&self) -> String {
        self.tenant().issuer()
    }
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

/// A validated sign-in: who the ID token names, and the token itself, which the tool server
/// validates again for a first link (it never takes `pmos-web`'s word for who someone is).
/// `Debug` shows neither.
pub struct SignedIn {
    pub identity: Identity,
    pub id_token: String,
}

impl std::fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedIn")
            .field("identity", &self.identity)
            .field("id_token", &"<redacted>")
            .finish()
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

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: Option<String>,
}

/// The Entra ID provider: builds the authorize redirect, checks the callback's flow cookie,
/// exchanges the code and validates the ID token (through [`pmos_entra::Verifier`]).
pub struct Entra {
    config: EntraConfig,
    http: reqwest::Client,
    verifier: pmos_entra::Verifier,
    /// Signs the flow cookie: derived from the client secret, so every `pmos-web` with the
    /// same configuration accepts the others' cookies and nothing new needs storing.
    flow_key: [u8; 32],
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
        let verifier = pmos_entra::Verifier::new(config.tenant())?;
        let mut mac = HmacSha256::new_from_slice(config.client_secret.as_bytes())
            .expect("HMAC takes any key length");
        mac.update(b"pmos-web entra flow cookie v1");
        let flow_key = mac.finalize().into_bytes().into();
        Ok(Self {
            config,
            http,
            verifier,
            flow_key,
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
        let meta = self.verifier.discovery().await?;
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

    /// Exchange the code for an ID token (client secret and PKCE verifier).
    async fn exchange(&self, code: &str, verifier: &str) -> Result<String, Failure> {
        let meta = self.verifier.discovery().await?;
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

    /// Validate an ID token and return who it names: `(tid, oid)` and `preferred_username`.
    pub async fn validate_identity(
        &self,
        token: &str,
        nonce: &str,
        now: DateTime<Utc>,
    ) -> Result<Identity, Failure> {
        self.verifier.validate(token, nonce, now).await
    }

    /// Exchange the code and validate the ID token: who it names, and the token.
    pub async fn sign_in_identity(
        &self,
        code: &str,
        nonce: &str,
        verifier: &str,
        now: DateTime<Utc>,
    ) -> Result<SignedIn, Failure> {
        if code.is_empty() || code.len() > 4096 {
            return Err(Failure::Failed("malformed code"));
        }
        let id_token = self.exchange(code, verifier).await?;
        let identity = self.validate_identity(&id_token, nonce, now).await?;
        Ok(SignedIn { identity, id_token })
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
            Ok(signed) => Ok(Claimed(signed.identity.oid)),
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
