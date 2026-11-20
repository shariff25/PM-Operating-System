//! Microsoft Entra ID (Azure AD) ID tokens: the one copy of the claim rules PM OS applies,
//! used by `pmos-web` when someone signs in and by the tool server when `pmos-web` asks it to
//! store a first link (`docs/entra-sign-in.md`, `docs/user-database-spec.md` §7).
//!
//! [`Verifier::validate`] checks, in this order: RS256 only, signed by a key in the tenant's
//! published JWKS (found through the tenant's OpenID configuration), `iss` exactly the tenant's
//! v2.0 issuer, `aud` the PM OS client id, `exp`/`nbf`/`iat` within [`SKEW_SECONDS`], the
//! sign-in's `nonce`, and `tid` the configured tenant. Then the claims PM OS keys on: `oid`
//! must be a GUID; the optional `acct` claim must be present (else
//! [`Failure::Misconfigured`]) and `0`, a member of the tenant (a guest is
//! [`Failure::NotMember`]); and `idp`, when present, must name this tenant (the token's own
//! issuer, or this tenant's v1 issuer `https://sts.windows.net/<tid>/`). `preferred_username`
//! is kept only when it is an email address; `email` is never read.
//!
//! **Keys and metadata.** The discovery document and the keys are cached for a day. A token
//! naming an unknown `kid` refetches the keys, at most once per
//! [`Tenant::jwks_refetch_after`] (a minute by default), so attacker-chosen `kid`s can't make
//! PM OS hammer Microsoft. A failed fetch is not retried for [`FAILED_FETCH_BACKOFF`] (or the
//! refetch interval, if that is shorter), and every fetch has a [`FETCH_TIMEOUT`]. Anything
//! that can't be fetched fails closed: no key, no sign-in.
//!
//! Tokens, `oid`s and names are never logged; failures carry a short fixed reason.

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use pmos_contract::model::valid_email;
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
/// so a first link on a cold cache still answers within `pmos-web`'s budget.
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
/// `consumers` are refused: PM OS is a single-tenant app.
pub fn guid_setting(var: &str, raw: &str) -> Result<String, String> {
    let v = raw.trim().to_ascii_lowercase();
    if matches!(v.as_str(), "common" | "organizations" | "consumers") {
        return Err(format!(
            "{var} must be your tenant's own id: `{v}` is a multi-tenant authority, which PM OS \
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

/// The one tenant and application PM OS accepts tokens for.
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

/// Why a token, or a sign-in, was refused. `reason()` is the code PM OS shows or answers;
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

/// The tenant's OpenID configuration: what PM OS uses of it.
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
    /// `iat`: with the `oid`, what makes a token usable for one first link only.
    pub issued_at: i64,
    /// `exp`: how long that use must be remembered.
    pub expires_at: i64,
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
    keys: tokio::sync::Mutex<KeyCache>,
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
            keys: tokio::sync::Mutex::new(KeyCache::default()),
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
        let body = res.bytes().await.map_err(|_| Failure::Unavailable(what))?;
        if body.len() > MAX_RESPONSE {
            return Err(Failure::Unavailable(what));
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
            tracing::error!(
                expected = %self.tenant.issuer(),
                "Entra discovery document names another issuer"
            );
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
            tracing::error!("Entra discovery document points off the authority");
            return Err(Failure::Unavailable(
                "discovery endpoints off the authority",
            ));
        }
        Ok(meta)
    }

    /// The key a token's `kid` names. The keys are fetched when there are none or they are a
    /// day old (after a failure, not again for the backoff), and an unknown `kid` refetches
    /// them at most once per `jwks_refetch_after`. Fails closed: a key that can't be read
    /// signs nothing in.
    async fn key(&self, kid: &str) -> Result<DecodingKey, Failure> {
        let mut cache = self.keys.lock().await;
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
            match self.fetch_keys().await {
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
        } else if stale {
            return Err(Failure::Unavailable("keys unavailable"));
        }
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
            None => return Err(Failure::Misconfigured),
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
        let preferred_username = claims
            .preferred_username
            .map(|e| e.trim().to_lowercase())
            .filter(|e| valid_email(e));
        Ok(Identity {
            oid,
            tid: self.tenant.tenant_id.to_ascii_lowercase(),
            preferred_username,
            issued_at: claims.iat,
            expires_at: claims.exp,
        })
    }
}

/// The tokens already used for a first link, so a captured one can't be replayed: each
/// `(oid, iat)` is remembered until its token expires (plus the skew). In memory: the tool
/// server runs one replica (`docs/user-database-spec.md` §12, "Out of scope"), and a restart
/// forgets, which leaves a token's remaining lifetime (an hour at most) open after a restart.
#[derive(Default)]
pub struct UsedTokens {
    used: std::sync::Mutex<HashMap<(String, i64), i64>>,
}

impl UsedTokens {
    /// Record `identity`'s token as used at `now`. `false` if it was already used.
    pub fn first_use(&self, identity: &Identity, now: DateTime<Utc>) -> bool {
        let t = now.timestamp();
        let mut used = self.used.lock().unwrap_or_else(|p| p.into_inner());
        used.retain(|_, until| *until > t);
        let key = (identity.oid.clone(), identity.issued_at);
        if used.contains_key(&key) {
            return false;
        }
        used.insert(key, identity.expires_at + SKEW_SECONDS);
        true
    }

    /// How many are remembered now (for tests).
    pub fn len(&self) -> usize {
        self.used.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests;
