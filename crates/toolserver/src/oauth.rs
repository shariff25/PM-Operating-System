//! Linking each PM's own accounts: Atlassian OAuth 2.0 (3LO) for Jira and Confluence, and
//! Slack OAuth v2 user tokens.
//!
//! Flow: the Slack bot or an admin page asks `GET /internal/oauth/{provider}/start?user=` for
//! an authorize URL and sends the PM there. The provider redirects the PM's browser to the
//! public `GET /oauth/{provider}/callback`. The tool server exchanges the code and seals the
//! tokens in the vault. The `state` parameter is HMAC-signed, names the user and provider,
//! expires in 10 minutes and is accepted once.
//!
//! [`OAuth`] is also the connectors' [`TokenSource`]. It refreshes an expiring Atlassian
//! token (rotating refresh tokens: each refresh returns a new refresh token) under a per-user
//! lock, so concurrent calls make one refresh.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use crate::session::valid_handle;
use crate::store::{StoreError, StoreResult};
use crate::team::Team;
use crate::vault::{Credential, CredentialInfo, Provider, Secret, Vault};

/// How long a `state` value is accepted.
pub const STATE_TTL_MINUTES: i64 = 10;
/// Refresh an access token this long before it expires.
pub const REFRESH_MARGIN_SECONDS: i64 = 60;

/// Where the providers' OAuth and API endpoints live. Tests point these at local mocks.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `https://auth.atlassian.com`
    pub atlassian_auth: String,
    /// `https://api.atlassian.com`
    pub atlassian_api: String,
    /// `https://slack.com`
    pub slack: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            atlassian_auth: "https://auth.atlassian.com".into(),
            atlassian_api: "https://api.atlassian.com".into(),
            slack: "https://slack.com".into(),
        }
    }
}

/// One OAuth app registration.
#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub client_id: String,
    pub client_secret: Secret,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Public base URL of the tool server, such as `https://pmos.example.com`. Callbacks are
    /// `{public_base_url}/oauth/{provider}/callback` and must be registered with each app.
    pub public_base_url: String,
    pub atlassian: Option<AppCredentials>,
    /// The Atlassian site to use, such as `https://example.atlassian.net`, when the PM's
    /// grant covers several sites.
    pub atlassian_site: Option<String>,
    pub slack: Option<AppCredentials>,
    /// Key for signing `state`.
    pub state_key: Vec<u8>,
    pub endpoints: Endpoints,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    #[error("{label} is not configured", label = .0.label())]
    NotConfigured(Provider),
    #[error("invalid user handle")]
    InvalidUser,
    #[error("the sign-in link is invalid or has expired; start again")]
    BadState,
    #[error("{label} declined the connection: {detail}", label = .0.label(), detail = .1)]
    Denied(Provider, String),
    #[error("{label} sign-in failed: {detail}", label = .0.label(), detail = .1)]
    Exchange(Provider, String),
    #[error("no Atlassian site matches; set PMOS_ATLASSIAN_SITE (sites: {0})")]
    SiteChoice(String),
    /// The account that signed in isn't the one `team.yaml` lists for this PM, so a link
    /// forwarded to someone else can't attach their account to the sender.
    #[error("the {label} account that signed in doesn't belong to this PM: {detail}", label = .0.label(), detail = .1)]
    WrongAccount(Provider, String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why no usable token was available.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("connect {label} in PM OS first", label = .0.label())]
    NotConnected(Provider),
    #[error("your {label} connection has expired; reconnect it in PM OS", label = .0.label())]
    Reconnect(Provider),
    #[error("{label} is unreachable: {detail}", label = .0.label(), detail = .1)]
    Unavailable(Provider, String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A live access token for one user, plus what calls need with it.
#[derive(Debug, Clone)]
pub struct AccessToken {
    pub token: Secret,
    pub cloud_id: Option<String>,
    pub site_url: Option<String>,
}

#[async_trait]
pub trait TokenSource: Send + Sync {
    /// A token for `user` that is valid for at least [`REFRESH_MARGIN_SECONDS`].
    async fn token(&self, user: &str, provider: Provider) -> Result<AccessToken, TokenError>;
}

#[derive(Serialize, Deserialize)]
struct StateClaims {
    u: String,
    p: Provider,
    n: String,
    e: i64,
}

pub struct OAuth {
    config: OAuthConfig,
    vault: Vault,
    http: reqwest::Client,
    used_states: Mutex<HashMap<String, DateTime<Utc>>>,
    refresh_locks: Mutex<HashMap<(String, Provider), RefreshLock>>,
    team: Arc<Team>,
}

type HmacSha256 = Hmac<Sha256>;

/// Held while one call refreshes a user's token, so concurrent calls make one refresh.
type RefreshLock = Arc<tokio::sync::Mutex<()>>;

impl OAuth {
    pub fn new(config: OAuthConfig, vault: Vault) -> Self {
        let http = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(20))
            .build()
            .expect("http client builds");
        Self {
            config,
            vault,
            http,
            used_states: Mutex::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
            team: Team::open(),
        }
    }

    /// Check every linked account against `team` (Slack member id, Atlassian email).
    pub fn with_team(mut self, team: Arc<Team>) -> Self {
        self.team = team;
        self
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    fn app(&self, provider: Provider) -> Result<&AppCredentials, OAuthError> {
        match provider {
            Provider::Atlassian => self.config.atlassian.as_ref(),
            Provider::Slack => self.config.slack.as_ref(),
        }
        .ok_or(OAuthError::NotConfigured(provider))
    }

    pub fn redirect_uri(&self, provider: Provider) -> String {
        format!(
            "{}/oauth/{provider}/callback",
            self.config.public_base_url.trim_end_matches('/')
        )
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.config.state_key).expect("HMAC takes any key length");
        mac.update(payload.as_bytes());
        B64URL.encode(mac.finalize().into_bytes())
    }

    fn make_state(&self, user: &str, provider: Provider, now: DateTime<Utc>) -> String {
        let claims = StateClaims {
            u: user.to_string(),
            p: provider,
            n: uuid::Uuid::new_v4().simple().to_string(),
            e: (now + Duration::minutes(STATE_TTL_MINUTES)).timestamp(),
        };
        let payload = B64URL.encode(serde_json::to_vec(&claims).expect("claims serialize"));
        let sig = self.sign(&payload);
        format!("{payload}.{sig}")
    }

    /// Check a `state` and mark it used. Returns the user it was issued for.
    fn take_state(
        &self,
        state: &str,
        provider: Provider,
        now: DateTime<Utc>,
    ) -> Result<String, OAuthError> {
        let (payload, sig) = state.split_once('.').ok_or(OAuthError::BadState)?;
        let mut mac =
            HmacSha256::new_from_slice(&self.config.state_key).expect("HMAC takes any key length");
        mac.update(payload.as_bytes());
        let sig = B64URL.decode(sig).map_err(|_| OAuthError::BadState)?;
        mac.verify_slice(&sig).map_err(|_| OAuthError::BadState)?;
        let claims: StateClaims = B64URL
            .decode(payload)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(OAuthError::BadState)?;
        if claims.p != provider || claims.e <= now.timestamp() || !valid_handle(&claims.u) {
            return Err(OAuthError::BadState);
        }
        let mut used = self.used_states.lock().expect("state lock");
        used.retain(|_, expiry| *expiry > now);
        if used.contains_key(&claims.n) {
            return Err(OAuthError::BadState);
        }
        let expiry = DateTime::from_timestamp(claims.e, 0).unwrap_or(now);
        used.insert(claims.n, expiry);
        Ok(claims.u)
    }

    /// The URL to send `user` to, to link their `provider` account.
    pub fn authorize_url(
        &self,
        user: &str,
        provider: Provider,
        now: DateTime<Utc>,
    ) -> Result<String, OAuthError> {
        if !valid_handle(user) {
            return Err(OAuthError::InvalidUser);
        }
        let app = self.app(provider)?;
        let state = self.make_state(user, provider, now);
        let redirect = self.redirect_uri(provider);
        let url = match provider {
            Provider::Atlassian => {
                let mut scopes = app.scopes.clone();
                if !scopes.iter().any(|s| s == "offline_access") {
                    scopes.push("offline_access".into());
                }
                reqwest::Url::parse_with_params(
                    &format!("{}/authorize", self.config.endpoints.atlassian_auth),
                    &[
                        ("audience", "api.atlassian.com"),
                        ("client_id", app.client_id.as_str()),
                        ("scope", scopes.join(" ").as_str()),
                        ("redirect_uri", redirect.as_str()),
                        ("state", state.as_str()),
                        ("response_type", "code"),
                        ("prompt", "consent"),
                    ],
                )
            }
            Provider::Slack => reqwest::Url::parse_with_params(
                &format!("{}/oauth/v2/authorize", self.config.endpoints.slack),
                &[
                    ("client_id", app.client_id.as_str()),
                    // User scopes only: PM OS acts as the PM, never as a bot, for reads and posts.
                    ("user_scope", app.scopes.join(",").as_str()),
                    ("redirect_uri", redirect.as_str()),
                    ("state", state.as_str()),
                ],
            ),
        };
        Ok(url
            .map_err(|e| OAuthError::Exchange(provider, e.to_string()))?
            .to_string())
    }

    /// Finish linking: check `state`, exchange `code`, and seal the tokens in the vault.
    pub async fn complete(
        &self,
        provider: Provider,
        code: &str,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<CredentialInfo, OAuthError> {
        let user = self.take_state(state, provider, now)?;
        let credential = match provider {
            Provider::Atlassian => self.exchange_atlassian(&user, code, now).await?,
            Provider::Slack => self.exchange_slack(&user, code, now).await?,
        };
        self.check_identity(&credential).await?;
        self.vault.put(&credential).await?;
        tracing::info!(user = %user, provider = %provider, "account linked");
        Ok(CredentialInfo {
            provider,
            scopes: credential.scopes,
            expires_at: credential.expires_at,
            site_url: credential.site_url,
            external_id: credential.external_id,
            updated_at: credential.updated_at,
        })
    }

    /// The account that signed in must be the one `team.yaml` lists for this PM. Nothing is
    /// stored otherwise.
    async fn check_identity(&self, credential: &Credential) -> Result<(), OAuthError> {
        if !self.team.enforced() {
            return Ok(());
        }
        let provider = credential.provider;
        let wrong = |why: &str| Err(OAuthError::WrongAccount(provider, why.to_string()));
        let Some(member) = self.team.member(&credential.user) else {
            return wrong("not in team.yaml");
        };
        match provider {
            Provider::Slack => {
                if credential.external_id.as_deref() != Some(member.slack_id.as_str()) {
                    return wrong("the Slack user isn't the slack-id in team.yaml");
                }
            }
            Provider::Atlassian => {
                let Some(expected) = member.email.as_deref() else {
                    return wrong("team.yaml has no email for this PM");
                };
                let me = self
                    .http
                    .get(format!("{}/me", self.config.endpoints.atlassian_api))
                    .bearer_auth(credential.access_token.expose())
                    .send()
                    .await
                    .map_err(|e| OAuthError::Exchange(provider, e.to_string()))?;
                if !me.status().is_success() {
                    return Err(OAuthError::Exchange(
                        provider,
                        format!("profile lookup returned {}", me.status()),
                    ));
                }
                let me: Value = me
                    .json()
                    .await
                    .map_err(|e| OAuthError::Exchange(provider, e.to_string()))?;
                let email = me["email"].as_str().unwrap_or_default();
                if !email.eq_ignore_ascii_case(expected) {
                    return wrong("the Atlassian email isn't the email in team.yaml");
                }
            }
        }
        Ok(())
    }

    async fn post_atlassian_token(&self, body: Value) -> Result<Value, (u16, String)> {
        let res = self
            .http
            .post(format!(
                "{}/oauth/token",
                self.config.endpoints.atlassian_auth
            ))
            .json(&body)
            .send()
            .await
            .map_err(|e| (0, e.to_string()))?;
        let status = res.status().as_u16();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(json)
        } else {
            Err((
                status,
                json["error"].as_str().unwrap_or("error").to_string(),
            ))
        }
    }

    async fn exchange_atlassian(
        &self,
        user: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Credential, OAuthError> {
        let app = self.app(Provider::Atlassian)?;
        let tokens = self
            .post_atlassian_token(serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": app.client_id,
                "client_secret": app.client_secret.expose(),
                "code": code,
                "redirect_uri": self.redirect_uri(Provider::Atlassian),
            }))
            .await
            .map_err(|(_, e)| OAuthError::Exchange(Provider::Atlassian, e))?;
        let access = tokens["access_token"]
            .as_str()
            .ok_or_else(|| OAuthError::Exchange(Provider::Atlassian, "no access token".into()))?;

        // Which site: the accessible resources the grant covers.
        let sites: Vec<Value> = self
            .http
            .get(format!(
                "{}/oauth/token/accessible-resources",
                self.config.endpoints.atlassian_api
            ))
            .bearer_auth(access)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| OAuthError::Exchange(Provider::Atlassian, e.to_string()))?
            .json()
            .await
            .map_err(|e| OAuthError::Exchange(Provider::Atlassian, e.to_string()))?;
        let site = match &self.config.atlassian_site {
            Some(want) => sites.iter().find(|s| {
                s["url"].as_str().map(|u| u.trim_end_matches('/'))
                    == Some(want.trim_end_matches('/'))
            }),
            None if sites.len() == 1 => sites.first(),
            None => None,
        }
        .ok_or_else(|| {
            let urls: Vec<&str> = sites.iter().filter_map(|s| s["url"].as_str()).collect();
            OAuthError::SiteChoice(urls.join(", "))
        })?;

        Ok(Credential {
            user: user.to_string(),
            provider: Provider::Atlassian,
            access_token: Secret::new(access),
            refresh_token: tokens["refresh_token"].as_str().map(Secret::new),
            expires_at: tokens["expires_in"]
                .as_i64()
                .map(|s| now + Duration::seconds(s)),
            scopes: tokens["scope"].as_str().unwrap_or_default().to_string(),
            cloud_id: site["id"].as_str().map(String::from),
            site_url: site["url"].as_str().map(String::from),
            external_id: None,
            updated_at: now,
        })
    }

    async fn post_slack_oauth(&self, form: &[(&str, &str)]) -> Result<Value, String> {
        let app = self.app(Provider::Slack).map_err(|e| e.to_string())?;
        let json: Value = self
            .http
            .post(format!(
                "{}/api/oauth.v2.access",
                self.config.endpoints.slack
            ))
            .basic_auth(&app.client_id, Some(app.client_secret.expose()))
            .form(form)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if json["ok"].as_bool() == Some(true) {
            Ok(json)
        } else {
            Err(json["error"].as_str().unwrap_or("error").to_string())
        }
    }

    async fn exchange_slack(
        &self,
        user: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Credential, OAuthError> {
        let redirect = self.redirect_uri(Provider::Slack);
        let json = self
            .post_slack_oauth(&[("code", code), ("redirect_uri", redirect.as_str())])
            .await
            .map_err(|e| OAuthError::Exchange(Provider::Slack, e))?;
        slack_user_credential(user, &json, now)
            .ok_or_else(|| OAuthError::Exchange(Provider::Slack, "no user token granted".into()))
    }

    fn lock_for(&self, user: &str, provider: Provider) -> RefreshLock {
        self.refresh_locks
            .lock()
            .expect("refresh lock map")
            .entry((user.to_string(), provider))
            .or_default()
            .clone()
    }

    async fn refresh(
        &self,
        current: Credential,
        now: DateTime<Utc>,
    ) -> Result<Credential, TokenError> {
        let provider = current.provider;
        let Some(refresh) = &current.refresh_token else {
            return Err(TokenError::Reconnect(provider));
        };
        let refreshed = match provider {
            Provider::Atlassian => {
                let app = self
                    .app(provider)
                    .map_err(|e| TokenError::Unavailable(provider, e.to_string()))?;
                match self
                    .post_atlassian_token(serde_json::json!({
                        "grant_type": "refresh_token",
                        "client_id": app.client_id,
                        "client_secret": app.client_secret.expose(),
                        "refresh_token": refresh.expose(),
                    }))
                    .await
                {
                    Ok(t) => Credential {
                        access_token: Secret::new(t["access_token"].as_str().unwrap_or_default()),
                        // Rotating refresh tokens: keep the new one; the old one stops working.
                        refresh_token: t["refresh_token"]
                            .as_str()
                            .map(Secret::new)
                            .or(current.refresh_token.clone()),
                        expires_at: t["expires_in"].as_i64().map(|s| now + Duration::seconds(s)),
                        scopes: t["scope"]
                            .as_str()
                            .map(String::from)
                            .unwrap_or(current.scopes.clone()),
                        updated_at: now,
                        ..current.clone()
                    },
                    // Only a refused grant means the PM's link is dead. Anything else, such as
                    // `invalid_client` after a mistyped client secret, or a firewall page, is
                    // ours to fix: unlinking would make every PM reconnect.
                    Err((_, e)) if e == "invalid_grant" => return self.unlink(&current).await,
                    Err((status, e)) => {
                        if (400..500).contains(&status) {
                            tracing::error!(
                                status,
                                error = %e,
                                "Atlassian refused the token refresh; check PMOS_ATLASSIAN_CLIENT_ID/SECRET"
                            );
                        }
                        return Err(TokenError::Unavailable(provider, e));
                    }
                }
            }
            Provider::Slack => match self
                .post_slack_oauth(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh.expose()),
                ])
                .await
            {
                Ok(json) => slack_user_credential(&current.user, &json, now)
                    .map(|c| Credential {
                        refresh_token: c.refresh_token.or(current.refresh_token.clone()),
                        external_id: c.external_id.or(current.external_id.clone()),
                        ..c
                    })
                    .ok_or(TokenError::Reconnect(provider))?,
                Err(e) if e == "invalid_refresh_token" || e == "invalid_grant" => {
                    return self.unlink(&current).await
                }
                Err(e) => return Err(TokenError::Unavailable(provider, e)),
            },
        };
        if refreshed.access_token.expose().is_empty() {
            return Err(TokenError::Reconnect(provider));
        }
        self.save_refreshed(&current, refreshed).await
    }

    /// Store a refreshed credential, only over the row it was refreshed from. If the PM
    /// unlinked meanwhile, the unlink stands. Atlassian rotates refresh tokens, so losing
    /// the new one forces a reconnect: retry before giving up.
    async fn save_refreshed(
        &self,
        current: &Credential,
        refreshed: Credential,
    ) -> Result<Credential, TokenError> {
        let provider = current.provider;
        let mut attempt = 0;
        loop {
            match self.vault.replace(&refreshed, current.updated_at).await {
                Ok(true) => return Ok(refreshed),
                Ok(false) => {
                    tracing::info!(user = %current.user, provider = %provider, "unlinked during refresh; not restoring it");
                    return Err(TokenError::NotConnected(provider));
                }
                Err(e) if attempt < 2 => {
                    attempt += 1;
                    tracing::warn!(error = %e, attempt, "saving a refreshed token failed; retrying");
                    tokio::time::sleep(StdDuration::from_millis(200 * attempt)).await;
                }
                Err(e) => {
                    tracing::error!(
                        user = %current.user,
                        provider = %provider,
                        error = %e,
                        "REFRESHED TOKEN NOT SAVED: the PM may need to reconnect"
                    );
                    return Err(e.into());
                }
            }
        }
    }

    /// Unlink an account (the PM disconnects, or a platform owner revokes). Waits for any
    /// refresh in flight, so that refresh can't write the account back.
    pub async fn unlink_account(&self, user: &str, provider: Provider) -> StoreResult<bool> {
        let lock = self.lock_for(user, provider);
        let _guard = lock.lock().await;
        self.vault.delete(user, provider).await
    }

    /// The provider refused the refresh token: the link is dead. Remove it so the PM is
    /// asked to reconnect, rather than retrying a revoked grant forever.
    async fn unlink(&self, current: &Credential) -> Result<Credential, TokenError> {
        self.vault.delete(&current.user, current.provider).await?;
        tracing::warn!(user = %current.user, provider = %current.provider, "refresh refused; unlinked");
        Err(TokenError::Reconnect(current.provider))
    }

    pub async fn token_at(
        &self,
        user: &str,
        provider: Provider,
        now: DateTime<Utc>,
    ) -> Result<AccessToken, TokenError> {
        let fresh = |c: &Credential| {
            c.expires_at
                .is_none_or(|e| e - now > Duration::seconds(REFRESH_MARGIN_SECONDS))
        };
        let current = self
            .vault
            .get(user, provider)
            .await?
            .ok_or(TokenError::NotConnected(provider))?;
        let credential = if fresh(&current) {
            current
        } else {
            let lock = self.lock_for(user, provider);
            let _guard = lock.lock().await;
            // Another call may have refreshed while this one waited.
            let latest = self
                .vault
                .get(user, provider)
                .await?
                .ok_or(TokenError::NotConnected(provider))?;
            if fresh(&latest) {
                latest
            } else {
                self.refresh(latest, now).await?
            }
        };
        Ok(AccessToken {
            token: credential.access_token,
            cloud_id: credential.cloud_id,
            site_url: credential.site_url,
        })
    }
}

/// The user token in an `oauth.v2.access` response: under `authed_user` for a code exchange,
/// at the top level (with `token_type: user`) for a refresh.
fn slack_user_credential(user: &str, json: &Value, now: DateTime<Utc>) -> Option<Credential> {
    let (tokens, external_id) = match json.get("authed_user") {
        Some(authed) if authed["access_token"].is_string() => (authed, authed["id"].as_str()),
        _ if json["token_type"] == "user" => (json, json["user_id"].as_str()),
        _ => return None,
    };
    Some(Credential {
        user: user.to_string(),
        provider: Provider::Slack,
        access_token: Secret::new(tokens["access_token"].as_str()?),
        refresh_token: tokens["refresh_token"].as_str().map(Secret::new),
        expires_at: tokens["expires_in"]
            .as_i64()
            .map(|s| now + Duration::seconds(s)),
        scopes: tokens["scope"].as_str().unwrap_or_default().to_string(),
        cloud_id: None,
        site_url: None,
        external_id: external_id.map(String::from),
        updated_at: now,
    })
}

#[async_trait]
impl TokenSource for OAuth {
    async fn token(&self, user: &str, provider: Provider) -> Result<AccessToken, TokenError> {
        self.token_at(user, provider, Utc::now()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{KeyRing, MemoryCredentials};

    fn oauth() -> OAuth {
        let vault = Vault::new(
            KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
            Arc::new(MemoryCredentials::default()),
        );
        OAuth::new(
            OAuthConfig {
                public_base_url: "https://pmos.example.com/".into(),
                atlassian: Some(AppCredentials {
                    client_id: "atl-client".into(),
                    client_secret: Secret::new("atl-secret"),
                    scopes: vec!["read:jira-work".into(), "write:jira-work".into()],
                }),
                atlassian_site: None,
                slack: None,
                state_key: b"state-key-for-tests".to_vec(),
                endpoints: Endpoints::default(),
            },
            vault,
        )
    }

    fn now() -> DateTime<Utc> {
        "2026-11-14T10:00:00Z".parse().unwrap()
    }

    fn state_of(url: &str) -> String {
        reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn atlassian_authorize_url() {
        let o = oauth();
        let url = reqwest::Url::parse(&o.authorize_url("ana", Provider::Atlassian, now()).unwrap())
            .unwrap();
        assert_eq!(url.host_str(), Some("auth.atlassian.com"));
        assert_eq!(url.path(), "/authorize");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["audience"], "api.atlassian.com");
        assert_eq!(q["client_id"], "atl-client");
        assert_eq!(q["scope"], "read:jira-work write:jira-work offline_access");
        assert_eq!(
            q["redirect_uri"],
            "https://pmos.example.com/oauth/atlassian/callback"
        );
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["prompt"], "consent");
        assert!(!url.as_str().contains("atl-secret"));
    }

    #[test]
    fn unconfigured_provider_and_bad_user() {
        let o = oauth();
        assert_eq!(
            o.authorize_url("ana", Provider::Slack, now()),
            Err(OAuthError::NotConfigured(Provider::Slack))
        );
        assert_eq!(
            o.authorize_url("../x", Provider::Atlassian, now()),
            Err(OAuthError::InvalidUser)
        );
    }

    #[test]
    fn state_is_single_use_signed_and_expiring() {
        let o = oauth();
        let state = state_of(&o.authorize_url("ana", Provider::Atlassian, now()).unwrap());
        assert_eq!(
            o.take_state(&state, Provider::Atlassian, now()),
            Ok("ana".into())
        );
        assert_eq!(
            o.take_state(&state, Provider::Atlassian, now()),
            Err(OAuthError::BadState),
            "reused"
        );

        let state = state_of(&o.authorize_url("ana", Provider::Atlassian, now()).unwrap());
        assert_eq!(
            o.take_state(&state, Provider::Slack, now()),
            Err(OAuthError::BadState),
            "wrong provider"
        );
        let late = now() + Duration::minutes(STATE_TTL_MINUTES);
        assert_eq!(
            o.take_state(&state, Provider::Atlassian, late),
            Err(OAuthError::BadState)
        );

        // Forged: a payload naming another user with the original signature.
        let (_, sig) = state.split_once('.').unwrap();
        let forged_payload = B64URL.encode(
            serde_json::to_vec(&StateClaims {
                u: "bo".into(),
                p: Provider::Atlassian,
                n: "n".into(),
                e: (now() + Duration::minutes(5)).timestamp(),
            })
            .unwrap(),
        );
        assert_eq!(
            o.take_state(
                &format!("{forged_payload}.{sig}"),
                Provider::Atlassian,
                now()
            ),
            Err(OAuthError::BadState)
        );
        assert_eq!(
            o.take_state("garbage", Provider::Atlassian, now()),
            Err(OAuthError::BadState)
        );
    }

    #[test]
    fn slack_tokens_from_exchange_and_refresh_shapes() {
        let exchange = serde_json::json!({
            "ok": true,
            "access_token": "xoxb-bot",
            "token_type": "bot",
            "authed_user": {"id": "U1", "scope": "chat:write", "access_token": "xoxp-user", "token_type": "user"}
        });
        let c = slack_user_credential("ana", &exchange, now()).unwrap();
        assert_eq!(c.access_token.expose(), "xoxp-user", "never the bot token");
        assert_eq!(c.external_id.as_deref(), Some("U1"));
        assert!(c.expires_at.is_none());

        let refresh = serde_json::json!({
            "ok": true, "access_token": "xoxe.xoxp-new", "token_type": "user",
            "expires_in": 43200, "refresh_token": "xoxe-1-new"
        });
        let c = slack_user_credential("ana", &refresh, now()).unwrap();
        assert_eq!(c.access_token.expose(), "xoxe.xoxp-new");
        assert_eq!(c.expires_at, Some(now() + Duration::seconds(43200)));

        let bot_only = serde_json::json!({"ok": true, "access_token": "xoxb", "token_type": "bot"});
        assert!(slack_user_credential("ana", &bot_only, now()).is_none());
    }
}
