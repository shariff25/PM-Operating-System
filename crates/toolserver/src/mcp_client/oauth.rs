//! Per-PM OAuth to an MCP server (`docs/mcp-servers-spec.md` §2.7, slice M4).
//!
//! - **Pre-registered clients only.** The client id is the record's; dynamic client
//!   registration is never used, whatever the server's metadata offers.
//! - **Metadata pinned to the record.** Before every link, the server's protected-resource
//!   metadata must name the record's `resource` and issuer, and the authorization server's
//!   metadata the record's issuer and endpoints. Nothing a metadata document names is ever
//!   fetched: a mismatch (another host, a cloud metadata address) refuses the link.
//! - **The authorization request.** Authorization code with PKCE (S256), the record's exact
//!   scopes, the `resource` indicator, and a `state` signed with HMAC that names the user and
//!   the server, is good for 10 minutes and is accepted once. The PKCE verifier stays in the
//!   tool server.
//! - **The identity check.** The callback calls the record's approved "who am I" tool with the
//!   new token and seals the tokens only if the email it names is one of the PM's addresses in
//!   the user database (their sign-in name or Atlassian email). Otherwise nothing is kept.
//! - **Tokens.** Sealed in the vault's format with its keys, bound to (user, `mcp:<server>`,
//!   field), in their own table (`mcp_credentials`). Never in a sandbox, a log, an audit event,
//!   a tool result or a request to another host: the token is sent only to the record's server
//!   and token endpoint. Refreshed under a per-(user, server) lock; only `invalid_grant` unlinks.
//!   A person who is not active in the user database gets no token.
//! - **Scopes.** What the grant gave is kept; a tool that needs a scope the grant lacks answers
//!   "reconnect".

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use pmos_contract::mcp::{AuthKind, ServerRecord};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;

use super::mapping::get_path;
use super::server::{CallKind, McpServer, ServerAuth, UserTokens};
use super::transport::{http_client, TestRoutes};
use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::connectors::ConnectorError;
use crate::store::{StoreError, StoreResult};
use crate::users::access::People;
use crate::vault::{KeyRing, Secret};

/// How long a `state` is accepted.
pub const STATE_TTL_MINUTES: i64 = 10;
/// Refresh this long before a token expires.
pub const REFRESH_MARGIN_SECONDS: i64 = 60;
/// The most a token endpoint's or metadata answer may hold.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The audit event of a link.
pub const LINK_EVENT: &str = "mcp.link";

/// A credential as stored: tokens sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedMcp {
    pub user: String,
    pub server: String,
    pub key_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Space-separated, as granted.
    pub scopes: String,
    /// The email the identity check matched.
    pub account: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Storage for sealed MCP credentials; implementations never see a plaintext token.
#[async_trait]
pub trait McpCredentials: Send + Sync {
    async fn get(&self, user: &str, server: &str) -> StoreResult<Option<SealedMcp>>;
    async fn put(&self, c: &SealedMcp) -> StoreResult<()>;
    /// Replace only if the stored row is still the one read (`updated_at` is `expected`).
    async fn replace(&self, c: &SealedMcp, expected: DateTime<Utc>) -> StoreResult<bool>;
    async fn delete(&self, user: &str, server: &str) -> StoreResult<bool>;
    async fn list(&self, user: &str) -> StoreResult<Vec<SealedMcp>>;
}

#[derive(Default)]
pub struct MemoryMcpCredentials {
    rows: Mutex<BTreeMap<(String, String), SealedMcp>>,
}

#[async_trait]
impl McpCredentials for MemoryMcpCredentials {
    async fn get(&self, user: &str, server: &str) -> StoreResult<Option<SealedMcp>> {
        Ok(self
            .rows
            .lock()
            .expect("rows")
            .get(&(user.into(), server.into()))
            .cloned())
    }
    async fn put(&self, c: &SealedMcp) -> StoreResult<()> {
        self.rows
            .lock()
            .expect("rows")
            .insert((c.user.clone(), c.server.clone()), c.clone());
        Ok(())
    }
    async fn replace(&self, c: &SealedMcp, expected: DateTime<Utc>) -> StoreResult<bool> {
        let mut rows = self.rows.lock().expect("rows");
        match rows.get_mut(&(c.user.clone(), c.server.clone())) {
            Some(row) if row.updated_at == expected => {
                *row = c.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    async fn delete(&self, user: &str, server: &str) -> StoreResult<bool> {
        Ok(self
            .rows
            .lock()
            .expect("rows")
            .remove(&(user.into(), server.into()))
            .is_some())
    }
    async fn list(&self, user: &str) -> StoreResult<Vec<SealedMcp>> {
        Ok(self
            .rows
            .lock()
            .expect("rows")
            .values()
            .filter(|c| c.user == user)
            .cloned()
            .collect())
    }
}

/// `mcp_credentials` in Postgres (migration 0030).
pub struct PgMcpCredentials {
    pool: sqlx::PgPool,
}

impl PgMcpCredentials {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str =
    "user_handle, server_id, key_id, access_token, refresh_token, expires_at, scopes, account, updated_at";

fn from_row(r: &sqlx::postgres::PgRow) -> StoreResult<SealedMcp> {
    Ok(SealedMcp {
        user: r.try_get("user_handle")?,
        server: r.try_get("server_id")?,
        key_id: r.try_get("key_id")?,
        access_token: r.try_get("access_token")?,
        refresh_token: r.try_get("refresh_token")?,
        expires_at: r.try_get("expires_at")?,
        scopes: r.try_get("scopes")?,
        account: r.try_get("account")?,
        updated_at: r.try_get("updated_at")?,
    })
}

#[async_trait]
impl McpCredentials for PgMcpCredentials {
    async fn get(&self, user: &str, server: &str) -> StoreResult<Option<SealedMcp>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM mcp_credentials WHERE user_handle = $1 AND server_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .bind(server)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(from_row).transpose()
    }
    async fn put(&self, c: &SealedMcp) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO mcp_credentials
                 (user_handle, server_id, key_id, access_token, refresh_token, expires_at, scopes, account, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (user_handle, server_id) DO UPDATE SET
                 key_id = EXCLUDED.key_id, access_token = EXCLUDED.access_token,
                 refresh_token = EXCLUDED.refresh_token, expires_at = EXCLUDED.expires_at,
                 scopes = EXCLUDED.scopes, account = EXCLUDED.account, updated_at = EXCLUDED.updated_at",
        )
        .bind(&c.user)
        .bind(&c.server)
        .bind(&c.key_id)
        .bind(&c.access_token)
        .bind(&c.refresh_token)
        .bind(c.expires_at)
        .bind(&c.scopes)
        .bind(&c.account)
        .bind(c.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn replace(&self, c: &SealedMcp, expected: DateTime<Utc>) -> StoreResult<bool> {
        let done = sqlx::query(
            "UPDATE mcp_credentials SET key_id = $3, access_token = $4, refresh_token = $5,
                 expires_at = $6, scopes = $7, account = $8, updated_at = $9
             WHERE user_handle = $1 AND server_id = $2 AND updated_at = $10",
        )
        .bind(&c.user)
        .bind(&c.server)
        .bind(&c.key_id)
        .bind(&c.access_token)
        .bind(&c.refresh_token)
        .bind(c.expires_at)
        .bind(&c.scopes)
        .bind(&c.account)
        .bind(c.updated_at)
        .bind(expected)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }
    async fn delete(&self, user: &str, server: &str) -> StoreResult<bool> {
        let done =
            sqlx::query("DELETE FROM mcp_credentials WHERE user_handle = $1 AND server_id = $2")
                .bind(user)
                .bind(server)
                .execute(&self.pool)
                .await?;
        Ok(done.rows_affected() == 1)
    }
    async fn list(&self, user: &str) -> StoreResult<Vec<SealedMcp>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM mcp_credentials WHERE user_handle = $1 ORDER BY server_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(from_row).collect()
    }
}

fn aad(user: &str, server: &str, field: &str) -> String {
    format!("pmos-vault|{user}|mcp:{server}|{field}")
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpOAuthError {
    #[error("no MCP server `{0}` takes per-user accounts here")]
    NotConfigured(String),
    #[error("`{0}` may not link accounts")]
    NotLinkable(String),
    #[error("this sign-in link is invalid or has expired")]
    BadState,
    /// The account the server named isn't the PM's.
    #[error("the account isn't {0}'s in the user database")]
    WrongAccount(String),
    #[error("the server's OAuth metadata doesn't match its record: {0}")]
    Metadata(String),
    #[error("the token exchange failed: {0}")]
    Exchange(String),
    #[error("storage: {0}")]
    Store(String),
}

impl From<StoreError> for McpOAuthError {
    fn from(e: StoreError) -> Self {
        Self::Store(e.0)
    }
}

/// What a person sees of their link to one server: never a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpLink {
    pub server: String,
    pub linked: bool,
    pub account: Option<String>,
    /// Scopes the record requests that the grant didn't give.
    pub missing_scopes: Vec<String>,
}

/// A refresh lock per (user, server).
type Locks = Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>;

struct Pending {
    user: String,
    server: String,
    verifier: String,
    expires: DateTime<Utc>,
}

/// Per-user OAuth for every `oauth_per_user` record.
pub struct McpOAuth {
    records: BTreeMap<String, ServerRecord>,
    http: BTreeMap<String, reqwest::Client>,
    routes: TestRoutes,
    keys: KeyRing,
    store: Arc<dyn McpCredentials>,
    people: Arc<People>,
    state_key: Vec<u8>,
    public_base_url: String,
    secrets: BTreeMap<String, Secret>,
    pending: Mutex<HashMap<String, Pending>>,
    locks: Locks,
    audit: Option<Arc<dyn AuditSink>>,
}

fn random_b64(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("the OS random source");
    B64.encode(bytes)
}

impl McpOAuth {
    /// `secret` reads a record's `client_secret_env`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        records: &[ServerRecord],
        keys: KeyRing,
        store: Arc<dyn McpCredentials>,
        people: Arc<People>,
        state_key: Vec<u8>,
        public_base_url: impl Into<String>,
        secret: impl Fn(&str) -> Option<String>,
        routes: TestRoutes,
        audit: Option<Arc<dyn AuditSink>>,
    ) -> Result<Self, String> {
        let mut by_id = BTreeMap::new();
        let mut http = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for r in records.iter().filter(|r| r.auth == AuthKind::OauthPerUser) {
            http.insert(r.id.clone(), http_client(r, &routes)?);
            if let Some(env) = r
                .oauth
                .as_ref()
                .and_then(|o| o.client_secret_env.as_deref())
            {
                let s = secret(env)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| format!("mcp/{}.yaml needs {env} set", r.id))?;
                secrets.insert(r.id.clone(), Secret::new(s));
            }
            by_id.insert(r.id.clone(), r.clone());
        }
        Ok(Self {
            records: by_id,
            http,
            routes,
            keys,
            store,
            people,
            state_key,
            public_base_url: public_base_url.into().trim_end_matches('/').to_string(),
            secrets,
            pending: Mutex::default(),
            locks: Mutex::default(),
            audit,
        })
    }

    fn record(&self, server: &str) -> Result<&ServerRecord, McpOAuthError> {
        self.records
            .get(server)
            .ok_or_else(|| McpOAuthError::NotConfigured(server.to_string()))
    }

    pub fn redirect_uri(&self, server: &str) -> String {
        format!("{}/oauth/mcp/{server}/callback", self.public_base_url)
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.state_key).expect("any key length");
        mac.update(b"pmos-mcp-oauth-state-v1|");
        mac.update(payload.as_bytes());
        B64.encode(mac.finalize().into_bytes())
    }

    async fn get_json(&self, server: &str, url: &str) -> Result<Value, McpOAuthError> {
        let http = &self.http[server];
        let res = http
            .get(self.routes.rewrite(url))
            .send()
            .await
            .map_err(|e| McpOAuthError::Metadata(format!("{url}: {}", e.without_url())))?;
        if !res.status().is_success() {
            return Err(McpOAuthError::Metadata(format!(
                "{url} answered {}",
                res.status().as_u16()
            )));
        }
        let body = res
            .bytes()
            .await
            .map_err(|e| McpOAuthError::Metadata(format!("{url}: {}", e.without_url())))?;
        if body.len() > MAX_ANSWER_BYTES {
            return Err(McpOAuthError::Metadata(format!(
                "{url}'s answer is too large"
            )));
        }
        serde_json::from_slice(&body)
            .map_err(|_| McpOAuthError::Metadata(format!("{url} isn't JSON")))
    }

    /// The server's and its authorization server's metadata must say what the record says.
    pub async fn check_metadata(&self, server: &str) -> Result<(), McpOAuthError> {
        let r = self.record(server)?;
        let o = r
            .oauth
            .as_ref()
            .ok_or_else(|| McpOAuthError::NotConfigured(server.into()))?;
        let u = pmos_contract::mcp::parse_url(&r.url).map_err(McpOAuthError::Metadata)?;
        let origin = match u.port {
            Some(p) => format!("https://{}:{p}", u.host),
            None => format!("https://{}", u.host),
        };
        let prm = self
            .get_json(
                server,
                &format!("{origin}/.well-known/oauth-protected-resource"),
            )
            .await?;
        if prm["resource"].as_str() != Some(o.resource.as_str()) {
            return Err(McpOAuthError::Metadata(
                "its protected-resource metadata names another resource".into(),
            ));
        }
        let names_issuer = prm["authorization_servers"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(o.issuer.as_str())));
        if !names_issuer {
            return Err(McpOAuthError::Metadata(
                "its protected-resource metadata doesn't name the record's authorization server"
                    .into(),
            ));
        }
        let issuer = o.issuer.trim_end_matches('/');
        let asm = self
            .get_json(
                server,
                &format!("{issuer}/.well-known/oauth-authorization-server"),
            )
            .await?;
        for (field, want) in [
            ("issuer", o.issuer.as_str()),
            ("authorization_endpoint", o.authorization_endpoint.as_str()),
            ("token_endpoint", o.token_endpoint.as_str()),
        ] {
            if asm[field].as_str() != Some(want) {
                return Err(McpOAuthError::Metadata(format!(
                    "the authorization server's `{field}` isn't the record's"
                )));
            }
        }
        let s256 = asm["code_challenge_methods_supported"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "S256"));
        if !s256 {
            return Err(McpOAuthError::Metadata(
                "the authorization server doesn't offer PKCE S256".into(),
            ));
        }
        Ok(())
    }

    /// Where to send `user` to link their account at `server`.
    pub async fn authorize_url(
        &self,
        user: &str,
        server: &str,
        now: DateTime<Utc>,
    ) -> Result<String, McpOAuthError> {
        let r = self.record(server)?;
        let o = r
            .oauth
            .as_ref()
            .ok_or_else(|| McpOAuthError::NotConfigured(server.into()))?;
        let bot = crate::session::is_bot_identity(user);
        let person = match self.people.user(user).await? {
            Some(p) => p.active(),
            None => false,
        };
        if !(person || bot) {
            return Err(McpOAuthError::NotLinkable(user.to_string()));
        }
        self.check_metadata(server).await?;
        let verifier = random_b64(32);
        let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
        let nonce = random_b64(18);
        let expires = now + Duration::minutes(STATE_TTL_MINUTES);
        let payload = B64.encode(format!("{server}|{user}|{}|{nonce}", expires.timestamp()));
        let state = format!("{payload}.{}", self.sign(&payload));
        {
            let mut pending = self.pending.lock().expect("pending");
            pending.retain(|_, p| p.expires > now);
            pending.insert(
                nonce,
                Pending {
                    user: user.into(),
                    server: server.into(),
                    verifier,
                    expires,
                },
            );
        }
        let url = reqwest::Url::parse_with_params(
            &o.authorization_endpoint,
            &[
                ("response_type", "code"),
                ("client_id", o.client_id.as_str()),
                ("redirect_uri", self.redirect_uri(server).as_str()),
                ("scope", o.scopes.join(" ").as_str()),
                ("state", state.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("resource", o.resource.as_str()),
            ],
        )
        .map_err(|e| McpOAuthError::Metadata(e.to_string()))?;
        Ok(url.into())
    }

    fn take_state(
        &self,
        server: &str,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<Pending, McpOAuthError> {
        let (payload, sig) = state.split_once('.').ok_or(McpOAuthError::BadState)?;
        let expected = self.sign(payload);
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            expected.as_bytes(),
            sig.as_bytes(),
        )) {
            return Err(McpOAuthError::BadState);
        }
        let text = B64.decode(payload).map_err(|_| McpOAuthError::BadState)?;
        let text = String::from_utf8(text).map_err(|_| McpOAuthError::BadState)?;
        let parts: Vec<&str> = text.split('|').collect();
        let [s, _user, exp, nonce] = parts[..] else {
            return Err(McpOAuthError::BadState);
        };
        let exp: i64 = exp.parse().map_err(|_| McpOAuthError::BadState)?;
        if s != server || exp < now.timestamp() {
            return Err(McpOAuthError::BadState);
        }
        let p = self
            .pending
            .lock()
            .expect("pending")
            .remove(nonce)
            .ok_or(McpOAuthError::BadState)?;
        if p.server != server || p.expires < now {
            return Err(McpOAuthError::BadState);
        }
        Ok(p)
    }

    async fn token_request(
        &self,
        server: &str,
        form: &[(&str, &str)],
    ) -> Result<Value, (Option<String>, String)> {
        let r = &self.records[server];
        let o = r.oauth.as_ref().expect("an oauth record");
        let mut form: Vec<(&str, &str)> = form.to_vec();
        let secret = self.secrets.get(server);
        if let Some(s) = secret {
            form.push(("client_secret", s.expose()));
        }
        let res = self.http[server]
            .post(self.routes.rewrite(&o.token_endpoint))
            .form(&form)
            .send()
            .await
            .map_err(|e| (None, e.without_url().to_string()))?;
        let status = res.status();
        let body = res
            .bytes()
            .await
            .map_err(|e| (None, e.without_url().to_string()))?;
        if body.len() > MAX_ANSWER_BYTES {
            return Err((None, "the token endpoint's answer is too large".into()));
        }
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if !status.is_success() {
            let code = json["error"].as_str().map(String::from);
            return Err((
                code,
                format!("the token endpoint answered {}", status.as_u16()),
            ));
        }
        Ok(json)
    }

    fn seal(
        &self,
        user: &str,
        server: &str,
        json: &Value,
        account: Option<String>,
        now: DateTime<Utc>,
        keep_refresh: Option<&str>,
    ) -> Result<SealedMcp, McpOAuthError> {
        let access = json["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| McpOAuthError::Exchange("no access token".into()))?;
        let refresh = json["refresh_token"].as_str().filter(|t| !t.is_empty());
        let requested = self.records[server]
            .oauth
            .as_ref()
            .map(|o| o.scopes.join(" "))
            .unwrap_or_default();
        let scopes = json["scope"]
            .as_str()
            .map(String::from)
            .unwrap_or(requested);
        let sealed_refresh = match (refresh, keep_refresh) {
            (Some(r), _) => Some(self.keys.seal_bound(r, &aad(user, server, "refresh"))?),
            (None, Some(old)) => Some(old.to_string()),
            (None, None) => None,
        };
        Ok(SealedMcp {
            user: user.into(),
            server: server.into(),
            key_id: self.keys.active_id().into(),
            access_token: self.keys.seal_bound(access, &aad(user, server, "access"))?,
            refresh_token: sealed_refresh,
            expires_at: json["expires_in"]
                .as_i64()
                .map(|s| now + Duration::seconds(s.clamp(0, 86_400 * 365))),
            scopes,
            account,
            updated_at: now,
        })
    }

    /// The email the server's approved identity tool names for `token`.
    async fn identity(
        &self,
        server: &str,
        user: &str,
        token: &str,
    ) -> Result<Option<String>, McpOAuthError> {
        let r = &self.records[server];
        let check = r
            .identity_check
            .as_ref()
            .ok_or_else(|| McpOAuthError::NotConfigured(server.into()))?;
        struct One(Secret);
        #[async_trait]
        impl UserTokens for One {
            async fn token(&self, _: &str, _: &str) -> Result<Secret, ConnectorError> {
                Ok(self.0.clone())
            }
        }
        let probe = McpServer::new(
            r.clone(),
            ServerAuth::PerUser(Arc::new(One(Secret::new(token)))),
            self.audit.clone(),
            &self.routes,
        )
        .map_err(McpOAuthError::Metadata)?;
        let out = probe
            .call_upstream(
                user,
                &check.tool,
                &check.digest,
                &Default::default(),
                Map::new(),
                CallKind::Read,
            )
            .await
            .map_err(|e| McpOAuthError::Exchange(format!("the identity check failed: {e}")))?;
        Ok(get_path(&out, &check.email)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase()))
    }

    async fn record_link(&self, user: &str, server: &str, outcome: Outcome, detail: &str) {
        let Some(audit) = &self.audit else { return };
        let event = AuditEvent {
            at: Utc::now(),
            client: Client::System,
            run_id: format!("mcp:{server}"),
            user: user.into(),
            skill: None,
            bot: None,
            persona: None,
            tool: LINK_EVENT.into(),
            outcome,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: Some(format!("server {server}: {detail}")),
        };
        if let Err(e) = audit.record(event).await {
            tracing::error!(error = %e, "an MCP link could not be audited");
        }
    }

    /// The callback: exchange the code, check the account, seal the tokens. Returns the user.
    pub async fn complete(
        &self,
        server: &str,
        code: &str,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<String, McpOAuthError> {
        self.record(server)?;
        let p = self.take_state(server, state, now)?;
        let o = self.records[server].oauth.clone().expect("oauth");
        let redirect = self.redirect_uri(server);
        let json = self
            .token_request(
                server,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", redirect.as_str()),
                    ("client_id", o.client_id.as_str()),
                    ("code_verifier", p.verifier.as_str()),
                    ("resource", o.resource.as_str()),
                ],
            )
            .await
            .map_err(|(_, e)| McpOAuthError::Exchange(e))?;
        let access = json["access_token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let account = self.identity(server, &p.user, &access).await?;
        let addresses: Vec<String> = match self.people.user(&p.user).await? {
            Some(u) if u.active() => [u.sign_in_name.clone(), u.atlassian_email.clone()]
                .into_iter()
                .flatten()
                .map(|a| a.trim().to_ascii_lowercase())
                .collect(),
            _ => Vec::new(),
        };
        let matches = account.as_ref().is_some_and(|a| addresses.contains(a));
        if !matches {
            self.record_link(
                &p.user,
                server,
                Outcome::Denied,
                "refused: the account isn't the PM's",
            )
            .await;
            return Err(McpOAuthError::WrongAccount(p.user));
        }
        let sealed = self.seal(&p.user, server, &json, account, now, None)?;
        self.store.put(&sealed).await?;
        self.record_link(&p.user, server, Outcome::Ok, "linked")
            .await;
        Ok(p.user)
    }

    fn lock_for(&self, user: &str, server: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .expect("locks")
            .entry((user.into(), server.into()))
            .or_default()
            .clone()
    }

    /// Scopes `user`'s grant at `server` gave.
    pub async fn granted(&self, user: &str, server: &str) -> Vec<String> {
        match self.store.get(user, server).await {
            Ok(Some(c)) => c.scopes.split_whitespace().map(String::from).collect(),
            _ => Vec::new(),
        }
    }

    pub async fn links(&self, user: &str) -> Result<Vec<McpLink>, McpOAuthError> {
        let mut out = Vec::new();
        for (id, r) in &self.records {
            let c = self.store.get(user, id).await?;
            let granted: Vec<&str> = c
                .as_ref()
                .map(|c| c.scopes.split_whitespace().collect())
                .unwrap_or_default();
            out.push(McpLink {
                server: id.clone(),
                linked: c.is_some(),
                account: c.as_ref().and_then(|c| c.account.clone()),
                missing_scopes: match &c {
                    Some(_) => r
                        .oauth
                        .iter()
                        .flat_map(|o| o.scopes.iter())
                        .filter(|s| !granted.contains(&s.as_str()))
                        .cloned()
                        .collect(),
                    None => Vec::new(),
                },
            });
        }
        Ok(out)
    }

    pub async fn unlink(&self, user: &str, server: &str) -> Result<bool, McpOAuthError> {
        Ok(self.store.delete(user, server).await?)
    }

    async fn access(
        &self,
        user: &str,
        server: &str,
        now: DateTime<Utc>,
    ) -> Result<Secret, ConnectorError> {
        let connect = || {
            ConnectorError::NotConnected(format!("connect your {server} account in PM OS first"))
        };
        if !crate::session::is_bot_identity(user) {
            let active = self
                .people
                .user(user)
                .await
                .map_err(|e| ConnectorError::Unavailable(e.0))?
                .is_some_and(|u| u.active());
            if !active {
                return Err(connect());
            }
        }
        let store_err = |e: StoreError| ConnectorError::Unavailable(e.0);
        let current = self
            .store
            .get(user, server)
            .await
            .map_err(store_err)?
            .ok_or_else(connect)?;
        let fresh = current
            .expires_at
            .is_none_or(|t| t > now + Duration::seconds(REFRESH_MARGIN_SECONDS));
        let unseal = |c: &SealedMcp| -> Result<Secret, ConnectorError> {
            self.keys
                .unseal_bound(&c.access_token, &aad(user, server, "access"))
                .map(Secret::new)
                .map_err(store_err)
        };
        if fresh {
            return unseal(&current);
        }
        let lock = self.lock_for(user, server);
        let _held = lock.lock().await;
        // Another call may have refreshed it while this one waited.
        let current = self
            .store
            .get(user, server)
            .await
            .map_err(store_err)?
            .ok_or_else(connect)?;
        if current
            .expires_at
            .is_none_or(|t| t > now + Duration::seconds(REFRESH_MARGIN_SECONDS))
        {
            return unseal(&current);
        }
        let Some(sealed_refresh) = current.refresh_token.clone() else {
            return Err(ConnectorError::Reconnect(format!(
                "your {server} sign-in expired; reconnect it in PM OS"
            )));
        };
        let refresh = self
            .keys
            .unseal_bound(&sealed_refresh, &aad(user, server, "refresh"))
            .map_err(store_err)?;
        let o = self.records[server].oauth.clone().expect("oauth");
        match self
            .token_request(
                server,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh.as_str()),
                    ("client_id", o.client_id.as_str()),
                    ("resource", o.resource.as_str()),
                ],
            )
            .await
        {
            Ok(json) => {
                let next = self
                    .seal(
                        user,
                        server,
                        &json,
                        current.account.clone(),
                        now,
                        Some(&sealed_refresh),
                    )
                    .map_err(|e| ConnectorError::Unavailable(e.to_string()))?;
                if !self
                    .store
                    .replace(&next, current.updated_at)
                    .await
                    .map_err(store_err)?
                {
                    return Err(ConnectorError::Unavailable(
                        "the account changed meanwhile; try again".into(),
                    ));
                }
                unseal(&next)
            }
            // Only a refused grant unlinks.
            Err((Some(code), _)) if code == "invalid_grant" => {
                let _ = self.store.delete(user, server).await;
                self.record_link(
                    user,
                    server,
                    Outcome::Ok,
                    "unlinked: the server refused the grant",
                )
                .await;
                Err(ConnectorError::Reconnect(format!(
                    "your {server} sign-in was revoked; reconnect it in PM OS"
                )))
            }
            Err((_, e)) => Err(ConnectorError::Unavailable(format!("{server}: {e}"))),
        }
    }
}

#[async_trait]
impl UserTokens for McpOAuth {
    async fn token(&self, user: &str, server: &str) -> Result<Secret, ConnectorError> {
        self.access(user, server, Utc::now()).await
    }

    /// `None` when nothing is linked: the call then answers "connect first".
    async fn granted(&self, user: &str, server: &str) -> Option<Vec<String>> {
        match self.store.get(user, server).await {
            Ok(Some(c)) => Some(c.scopes.split_whitespace().map(String::from).collect()),
            _ => None,
        }
    }
}
