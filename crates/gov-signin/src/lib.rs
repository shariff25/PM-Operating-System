//! Microsoft Entra ID tokens for Agent Governance (spec §7.6.4, point 1): the claim rules
//! of Governance's verifier, written again here because Governance depends on nothing of Governance.
//!
//! [`Verifier::validate`] checks: RS256 only, signed by a key the tenant publishes, `iss`
//! exactly the tenant's v2.0 issuer, `aud` this client alone, `exp`/`nbf`/`iat` within
//! [`SKEW_SECONDS`], the sign-in's `nonce`, `tid` the configured tenant, `oid` a GUID,
//! `acct` present and a member's, `auth_time` present, and `idp`, when present, this
//! tenant. A person is `(tid, oid)`: no name or email is read.
//!
//! The discovery document and the keys are cached for a day; every endpoint must be on
//! the configured authority's origin; no redirect is followed; a response is read up to
//! [`MAX_RESPONSE`]; an unknown `kid` refetches the keys at most once per
//! [`Tenant::jwks_refetch_after`]. What cannot be fetched fails closed.
//!
//! Tokens and object ids are never logged; a failure carries a short fixed reason.

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

/// Microsoft's public cloud.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";
/// The clock skew allowed on `exp`, `nbf` and `iat`.
pub const SKEW_SECONDS: i64 = 60;
/// How long the discovery document and the keys are kept before a fresh fetch.
pub const METADATA_TTL: StdDuration = StdDuration::from_secs(24 * 60 * 60);
/// The least time between two fetches of the keys for an unknown `kid`, by default.
pub const DEFAULT_REFETCH_AFTER: StdDuration = StdDuration::from_secs(60);
/// After a failed fetch, the least time before the next (shortened to the refetch interval
/// when that is shorter).
pub const FAILED_FETCH_BACKOFF: StdDuration = StdDuration::from_secs(5);
/// The longest a discovery or JWKS fetch may take. Under the tool server client's 5 seconds,
/// so a first link on a cold cache still answers within the caller's budget.
pub const FETCH_TIMEOUT: StdDuration = StdDuration::from_secs(4);
/// The largest discovery or JWKS response read.
pub const MAX_RESPONSE: usize = 256 * 1024;
/// The longest ID token accepted (Entra's are a few kilobytes).
pub const MAX_TOKEN: usize = 16 * 1024;

/// Is `v` a GUID (`8-4-4-4-12` hex digits)?
pub fn is_guid(v: &str) -> bool {
    let groups: Vec<&str> = v.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Does `url` name a loopback host (`localhost`, `127.0.0.0/8`, `::1`)?
pub fn is_loopback_host(url: &reqwest::Url) -> bool {
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
pub fn secure_or_loopback(value: &str) -> bool {
    reqwest::Url::parse(value)
        .is_ok_and(|u| u.scheme() == "https" || (u.scheme() == "http" && is_loopback_host(&u)))
}

/// A tenant or client id setting: trimmed, lower-cased, a GUID. `common`, `organizations` and
/// `consumers` are refused: Governance is a single-tenant app.
pub fn guid_setting(var: &str, raw: &str) -> Result<String, String> {
    let v = raw.trim().to_ascii_lowercase();
    if matches!(v.as_str(), "common" | "organizations" | "consumers") {
        return Err(format!(
            "{var} must be your tenant's own id: `{v}` is a multi-tenant authority, which Governance \
             refuses"
        ));
    }
    if !is_guid(&v) {
        return Err(format!(
            "{var} must be a GUID such as 00000000-0000-0000-0000-000000000000"
        ));
    }
    Ok(v)
}

/// An authority setting: `None` is [`DEFAULT_AUTHORITY`]; otherwise an origin with no path,
/// `https` unless it is loopback (tests point it at a mock).
pub fn authority_setting(var: &str, raw: Option<&str>) -> Result<String, String> {
    let Some(a) = raw else {
        return Ok(DEFAULT_AUTHORITY.to_string());
    };
    let url = reqwest::Url::parse(a.trim()).map_err(|_| format!("{var} must be a URL"))?;
    if url.path() != "/"
        || url.query().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(format!(
            "{var} must be an origin such as {DEFAULT_AUTHORITY}, with no path"
        ));
    }
    let origin = url.origin().ascii_serialization();
    if !secure_or_loopback(&origin) {
        return Err(format!("{var} must be https unless it is loopback"));
    }
    Ok(origin)
}

/// The one tenant and application Governance accepts tokens for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    /// The directory (tenant) id, a GUID, lower-cased.
    pub tenant_id: String,
    /// The application (client) id, a GUID, lower-cased: the only `aud` accepted.
    pub client_id: String,
    /// An origin: [`DEFAULT_AUTHORITY`], or a loopback mock in tests.
    pub authority: String,
    /// The least time between two fetches of the keys when a token names an unknown `kid`.
    pub jwks_refetch_after: StdDuration,
}

impl Tenant {
    /// Checked settings: `tenant` and `client` GUIDs, `authority` an origin (default
    /// [`DEFAULT_AUTHORITY`]). The variable names are for the error messages.
    pub fn from_settings(
        vars: [&str; 3],
        tenant: &str,
        client: &str,
        authority: Option<&str>,
    ) -> Result<Self, String> {
        Ok(Self {
            tenant_id: guid_setting(vars[0], tenant)?,
            client_id: guid_setting(vars[1], client)?,
            authority: authority_setting(vars[2], authority)?,
            jwks_refetch_after: DEFAULT_REFETCH_AFTER,
        })
    }

    /// The only issuer accepted: `<authority>/<tenant id>/v2.0`.
    pub fn issuer(&self) -> String {
        format!("{}/{}/v2.0", self.authority, self.tenant_id)
    }

    /// `<issuer>/.well-known/openid-configuration`.
    pub fn discovery_url(&self) -> String {
        format!("{}/.well-known/openid-configuration", self.issuer())
    }

    /// This tenant's v1 issuer, `https://sts.windows.net/<tid>/`: what `idp` may also hold for
    /// a member (it names the same tenant).
    pub fn v1_issuer(&self) -> String {
        format!("https://sts.windows.net/{}/", self.tenant_id)
    }
}

/// Why a token, or a sign-in, was refused. `reason()` is the code Governance shows or answers;
/// `why()` a fixed phrase for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No sign-in in progress, or one past its time.
    Expired,
    /// The person cancelled at Microsoft (`error=access_denied`).
    Cancelled,
    /// Microsoft or the network failed: the discovery document or the keys couldn't be read.
    Unavailable(&'static str),
    /// Anything else: a bad state, code or token.
    Failed(&'static str),
    /// The token lacks an optional claim the app registration must add: `acct`, without
    /// which a member cannot be told from a guest, or `auth_time`, without which a recent
    /// sign-in cannot be told from an old one. Everyone is refused.
    Misconfigured(&'static str),
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
            Failure::Misconfigured(_) => "entra_misconfigured",
            Failure::NotMember(_) => "not_a_member",
        }
    }

    pub fn why(&self) -> &'static str {
        match self {
            Failure::Expired => "no sign-in in progress, or it expired",
            Failure::Cancelled => "cancelled at the identity provider",
            Failure::Unavailable(w)
            | Failure::Failed(w)
            | Failure::NotMember(w)
            | Failure::Misconfigured(w) => w,
        }
    }
}

/// The tenant's OpenID configuration: what Governance uses of it.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
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
    /// The last successful fetch.
    fetched: Option<Instant>,
    /// The last fetch started, successful or not.
    attempted: Option<Instant>,
    /// Whether that attempt failed.
    failed: bool,
}

#[derive(Default)]
struct MetadataCache {
    doc: Option<(Instant, Discovery)>,
    /// The last failed fetch.
    failed_at: Option<Instant>,
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
    /// When the person last proved who they are to the tenant.
    auth_time: Option<i64>,
}

/// Who a checked ID token names. `Debug` does not show the `oid`.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// The `oid` claim, lower-cased: always a GUID.
    pub oid: String,
    /// The `tid` claim, lower-cased: always the configured tenant.
    pub tid: String,
    pub issued_at: i64,
    pub expires_at: i64,
    /// `auth_time`: never later than the checker's clock plus the skew.
    pub auth_time: i64,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("oid", &"<redacted>")
            .field("tid", &self.tid)
            .finish()
    }
}

/// Is `acct` a member's (`0`)?
fn acct_is_member(acct: &serde_json::Value) -> bool {
    acct.as_i64() == Some(0) || acct.as_str().map(str::trim) == Some("0")
}

/// Constant-time equality of two strings of any length.
pub fn token_eq(presented: &str, expected: &str) -> bool {
    let p = Sha256::digest(presented.as_bytes());
    let e = Sha256::digest(expected.as_bytes());
    bool::from(p.as_slice().ct_eq(e.as_slice()))
}

/// Validates ID tokens for one [`Tenant`]: fetches and caches the tenant's discovery document
/// and keys.
pub struct Verifier {
    tenant: Tenant,
    http: reqwest::Client,
    metadata: tokio::sync::Mutex<MetadataCache>,
    /// The keys. Never held across a fetch (the re-check of eb24d28, LC): a token under a
    /// known key is checked at once, even while the keys are being fetched.
    keys: std::sync::Mutex<KeyCache>,
    /// One fetch of the keys at a time: concurrent requests for an unknown `kid` wait for the
    /// one fetch in flight (at most [`FETCH_TIMEOUT`]) and read what it found.
    refresh: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier")
            .field("tenant", &self.tenant)
            .finish()
    }
}

impl Verifier {
    pub fn new(tenant: Tenant) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self {
            tenant,
            http,
            metadata: tokio::sync::Mutex::new(MetadataCache::default()),
            keys: std::sync::Mutex::new(KeyCache::default()),
            refresh: tokio::sync::Mutex::new(()),
        })
    }

    pub fn tenant(&self) -> &Tenant {
        &self.tenant
    }

    fn backoff(&self) -> StdDuration {
        FAILED_FETCH_BACKOFF.min(self.tenant.jwks_refetch_after)
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
        if !res.status().is_success()
            || res
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE as u64)
        {
            return Err(Failure::Unavailable(what));
        }
        let mut res = res;
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|_| Failure::Unavailable(what))? {
            if body.len() + chunk.len() > MAX_RESPONSE {
                return Err(Failure::Unavailable(what));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| Failure::Unavailable(what))
    }

    /// The tenant's discovery document, cached for a day. Its issuer must be exactly the
    /// configured one, and every endpoint on the authority's own origin. After a failed
    /// fetch, the next waits for the backoff (failing closed meanwhile).
    pub async fn discovery(&self) -> Result<Discovery, Failure> {
        let mut cache = self.metadata.lock().await;
        if let Some((at, meta)) = cache.doc.as_ref() {
            if at.elapsed() < METADATA_TTL {
                return Ok(meta.clone());
            }
        }
        if cache
            .failed_at
            .is_some_and(|at| at.elapsed() < self.backoff())
        {
            return Err(Failure::Unavailable("discovery document unavailable"));
        }
        match self.fetch_discovery().await {
            Ok(meta) => {
                cache.doc = Some((Instant::now(), meta.clone()));
                cache.failed_at = None;
                Ok(meta)
            }
            Err(f) => {
                cache.failed_at = Some(Instant::now());
                Err(f)
            }
        }
    }

    async fn fetch_discovery(&self) -> Result<Discovery, Failure> {
        let meta: Discovery = self
            .get_json(
                &self.tenant.discovery_url(),
                "discovery document unavailable",
            )
            .await?;
        if meta.issuer != self.tenant.issuer() {
            return Err(Failure::Unavailable("discovery issuer mismatch"));
        }
        let ours = |u: &str| {
            reqwest::Url::parse(u)
                .is_ok_and(|u| u.origin().ascii_serialization() == self.tenant.authority)
        };
        if ![
            &meta.authorization_endpoint,
            &meta.token_endpoint,
            &meta.jwks_uri,
        ]
        .iter()
        .all(|u| ours(u))
        {
            return Err(Failure::Unavailable(
                "discovery endpoints off the authority",
            ));
        }
        Ok(meta)
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, KeyCache> {
        self.keys.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The key `kid` names in the cache, while the cache is fresh.
    fn cached(&self, kid: &str) -> Option<Result<DecodingKey, Failure>> {
        let cache = self.cache();
        let fresh = cache.fetched.is_some_and(|at| at.elapsed() < METADATA_TTL);
        let (n, e) = cache.keys.get(kid).filter(|_| fresh)?;
        Some(
            DecodingKey::from_rsa_components(n, e)
                .map_err(|_| Failure::Failed("unusable signing key")),
        )
    }

    /// The key a token's `kid` names. The keys are fetched when there are none or they are a
    /// day old (after a failure, not again for the backoff), and an unknown `kid` refetches
    /// them at most once per `jwks_refetch_after`. A fetch runs outside the cache's lock, one
    /// at a time (concurrent requests wait for it and read its result), within
    /// [`FETCH_TIMEOUT`] for the discovery document and the keys together, so a first link
    /// on a cold cache answers inside the caller's 5 seconds (LC). Fails closed: a key that
    /// can't be read signs nothing in.
    async fn key(&self, kid: &str) -> Result<DecodingKey, Failure> {
        if let Some(found) = self.cached(kid) {
            return found;
        }
        let _one = self.refresh.lock().await;
        let fetch = {
            let mut cache = self.cache();
            let stale = cache.fetched.is_none_or(|at| at.elapsed() >= METADATA_TTL);
            let unknown = !cache.keys.contains_key(kid);
            let gap = if cache.failed {
                self.backoff()
            } else {
                self.tenant.jwks_refetch_after
            };
            let may_fetch = cache.attempted.is_none_or(|at| at.elapsed() >= gap);
            if (stale || unknown) && may_fetch {
                cache.attempted = Some(Instant::now());
                true
            } else if stale {
                return Err(Failure::Unavailable("keys unavailable"));
            } else {
                // Fresh, and either another request just fetched it, or the kid is unknown
                // and refetched too recently.
                false
            }
        };
        if fetch {
            let fetched = tokio::time::timeout(FETCH_TIMEOUT, self.fetch_keys())
                .await
                .unwrap_or(Err(Failure::Unavailable("keys unavailable")));
            let mut cache = self.cache();
            match fetched {
                Ok(keys) => {
                    cache.keys = keys;
                    cache.fetched = Some(Instant::now());
                    cache.failed = false;
                }
                Err(f) => {
                    cache.failed = true;
                    return Err(f);
                }
            }
        }
        let cache = self.cache();
        let (n, e) = cache
            .keys
            .get(kid)
            .ok_or(Failure::Failed("unknown signing key"))?;
        DecodingKey::from_rsa_components(n, e).map_err(|_| Failure::Failed("unusable signing key"))
    }

    async fn fetch_keys(&self) -> Result<HashMap<String, (String, String)>, Failure> {
        let meta = self.discovery().await?;
        let jwks: Jwks = self.get_json(&meta.jwks_uri, "keys unavailable").await?;
        Ok(jwks
            .keys
            .into_iter()
            .filter(|k| {
                k.kty == "RSA"
                    && k.use_.as_deref().is_none_or(|u| u == "sig")
                    && k.alg.as_deref().is_none_or(|a| a == "RS256")
            })
            .filter_map(|k| Some((k.kid?, (k.n?, k.e?))))
            .collect())
    }

    /// Validate an ID token issued for the sign-in that sent `nonce`, and return who it
    /// names. Every rule in the module docs; nothing in the token is trusted before its
    /// signature checks out.
    pub async fn validate(
        &self,
        token: &str,
        nonce: &str,
        now: DateTime<Utc>,
    ) -> Result<Identity, Failure> {
        if token.is_empty() || token.len() > MAX_TOKEN {
            return Err(Failure::Failed("malformed token"));
        }
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| Failure::Failed("malformed token"))?;
        if header.alg != Algorithm::RS256 {
            return Err(Failure::Failed("token not RS256"));
        }
        let kid = header.kid.ok_or(Failure::Failed("token without kid"))?;
        if kid.is_empty() || kid.len() > 256 {
            return Err(Failure::Failed("token without kid"));
        }
        let key = self.key(&kid).await?;
        // The signature only: every claim is checked below against the caller's clock.
        let mut v = Validation::new(Algorithm::RS256);
        v.validate_exp = false;
        v.validate_nbf = false;
        v.validate_aud = false;
        v.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<IdClaims>(token, &key, &v)
            .map_err(|_| Failure::Failed("token signature or shape"))?
            .claims;
        if claims.iss != self.tenant.issuer() {
            return Err(Failure::Failed("token issuer"));
        }
        let aud_ok = match &claims.aud {
            Audience::One(a) => *a == self.tenant.client_id,
            Audience::Many(list) => list.len() == 1 && list[0] == self.tenant.client_id,
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
            .is_some_and(|n| !nonce.is_empty() && token_eq(n, nonce))
        {
            return Err(Failure::Failed("token nonce"));
        }
        if !claims
            .tid
            .as_deref()
            .is_some_and(|tid| tid.eq_ignore_ascii_case(&self.tenant.tenant_id))
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
            None => return Err(Failure::Misconfigured("no acct claim")),
            Some(acct) if !acct_is_member(acct) => {
                return Err(Failure::NotMember("a guest account"))
            }
            Some(_) => {}
        }
        // `idp` is absent for a member, or names this tenant; anything else is someone who
        // signed in with another identity provider.
        if let Some(idp) = claims.idp.as_deref().map(str::trim) {
            if idp != claims.iss && !idp.eq_ignore_ascii_case(&self.tenant.v1_issuer()) {
                return Err(Failure::NotMember("another identity provider"));
            }
        }
        let auth_time = claims
            .auth_time
            .ok_or(Failure::Misconfigured("no auth_time claim"))?;
        if auth_time > t + SKEW_SECONDS {
            return Err(Failure::Failed("token authenticated in the future"));
        }
        Ok(Identity {
            oid,
            tid: self.tenant.tenant_id.to_ascii_lowercase(),
            issued_at: claims.iat,
            expires_at: claims.exp,
            auth_time,
        })
    }
}

#[cfg(test)]
mod tests;
