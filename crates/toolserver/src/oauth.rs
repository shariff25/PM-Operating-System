//! Linking each PM's own accounts: Atlassian OAuth 2.0 (3LO) for Jira and Confluence, Slack
//! OAuth v2 user tokens, and GitHub App user tokens (`docs/onboarding-spec.md` §1).
//!
//! Flow: the Slack bot or an admin page asks `GET /internal/oauth/{provider}/start?user=` for
//! an authorize URL and sends the PM there. The provider redirects the PM's browser to the
//! public `GET /oauth/{provider}/callback`. The tool server exchanges the code and seals the
//! tokens in the vault. The `state` parameter is HMAC-signed, names the user and provider,
//! expires in 10 minutes and is accepted once.
//!
//! [`OAuth`] is also the connectors' [`TokenSource`]. It refreshes an expiring Atlassian or
//! GitHub token (rotating refresh tokens: each refresh returns a new refresh token) under a
//! per-user lock, so concurrent calls make one refresh.
//!
//! Every link is checked against the user database before anything is stored
//! (`docs/user-database-spec.md` §6): the person must be an active PM OS user, and the account
//! must be theirs. Slack: the member id is their Slack identity. Atlassian and GitHub: **after
//! the first link the account id decides** (the Atlassian `account_id`, the GitHub numeric user
//! id, stored as an identity); a later link with another account is refused `409
//! different_account` until an admin unlinks. Before the first link, the Atlassian email must be
//! their `atlassian_email`, and a GitHub account (an active member of `PMOS_GITHUB_ORG`) must
//! have their `github_login`, or, with none recorded, a verified email equal to their
//! `atlassian_email`. The GitHub login and the Atlassian account id are kept as the
//! credential's `external_id`.

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

use crate::session::{is_bot_identity, valid_handle};

/// Who may have linked accounts: a PM, or a team bot's service identity.
fn linkable(user: &str) -> bool {
    valid_handle(user) || is_bot_identity(user)
}
use crate::connectors::registry::{link_statuses, registry, LinkStatus};
use crate::store::{StoreError, StoreResult};
use crate::users::access::{may_use, People};
use crate::users::{Actor, IdentityKind, Op, Refusal, UserError};
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
    /// `https://github.com`: `/login/oauth/authorize` and `/login/oauth/access_token`.
    pub github_auth: String,
    /// `https://api.github.com`
    pub github_api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            atlassian_auth: "https://auth.atlassian.com".into(),
            atlassian_api: "https://api.atlassian.com".into(),
            slack: "https://slack.com".into(),
            github_auth: "https://github.com".into(),
            github_api: "https://api.github.com".into(),
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
    /// The GitHub App. Its permissions apply, so `scopes` is unused.
    pub github: Option<AppCredentials>,
    /// The GitHub org a linked GitHub account must be an active member of.
    pub github_org: Option<String>,
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
    /// The account that signed in isn't the one the user database lists for this PM, so a link
    /// forwarded to someone else can't attach their account to the sender.
    #[error("the {label} account that signed in doesn't belong to this PM: {detail}", label = .0.label(), detail = .1)]
    WrongAccount(Provider, String),
    /// The PM linked another account of this kind first: its account id decides. An admin
    /// unlinks it before another can be linked (`409 different_account`).
    #[error("this PM linked a different {label} account first; ask a platform owner to unlink it before linking another", label = .0.label())]
    DifferentAccount(Provider),
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
    /// Whose accounts may be linked, and which accounts are theirs.
    people: Arc<People>,
}

type HmacSha256 = Hmac<Sha256>;

/// Held while one call refreshes a user's token, so concurrent calls make one refresh.
type RefreshLock = Arc<tokio::sync::Mutex<()>>;

impl OAuth {
    /// `people`: the user directory every link is checked against (an open one, in tests,
    /// checks nothing).
    pub fn new(config: OAuthConfig, vault: Vault, people: Arc<People>) -> Self {
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
            people,
        }
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    fn app(&self, provider: Provider) -> Result<&AppCredentials, OAuthError> {
        match provider {
            Provider::Atlassian => self.config.atlassian.as_ref(),
            Provider::Slack => self.config.slack.as_ref(),
            // GitHub linking needs the org to check membership against.
            Provider::Github => self
                .config
                .github
                .as_ref()
                .filter(|_| self.config.github_org.is_some()),
        }
        .ok_or(OAuthError::NotConfigured(provider))
    }

    /// The scopes PM OS requests from `provider` now (empty when it is not configured or has
    /// no scope list, as for a GitHub App). Not secret.
    pub fn requested_scopes(&self, provider: Provider) -> Vec<String> {
        match provider {
            Provider::Atlassian => self.config.atlassian.as_ref(),
            Provider::Slack => self.config.slack.as_ref(),
            Provider::Github => None,
        }
        .map(|a| a.scopes.clone())
        .unwrap_or_default()
    }

    /// Where each registered provider stands for `user`, with granted scopes and any requested
    /// scope that was not granted. Read-only; no tokens.
    pub async fn link_statuses(
        &self,
        user: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<LinkStatus>> {
        let linked = self.vault.list(user).await?;
        Ok(link_statuses(&linked, now, |p| self.requested_scopes(p)))
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
        if claims.p != provider || claims.e <= now.timestamp() || !linkable(&claims.u) {
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

    /// The URL to send `user` to, to link their `provider` account. `user` is a PM's handle,
    /// or a team bot's service identity `bots.<name>` for a platform owner linking the bot's
    /// own read-only account.
    pub fn authorize_url(
        &self,
        user: &str,
        provider: Provider,
        now: DateTime<Utc>,
    ) -> Result<String, OAuthError> {
        // GitHub serves PMs only: a team bot has no memory and commits nothing.
        if !linkable(user) || (!registry().spec(provider).team_bots && is_bot_identity(user)) {
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
            // No scope: a GitHub App's permissions apply.
            Provider::Github => reqwest::Url::parse_with_params(
                &format!(
                    "{}/login/oauth/authorize",
                    self.config.endpoints.github_auth
                ),
                &[
                    ("client_id", app.client_id.as_str()),
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
        let mut credential = match provider {
            Provider::Atlassian => self.exchange_atlassian(&user, code, now).await?,
            Provider::Slack => self.exchange_slack(&user, code, now).await?,
            Provider::Github => self.exchange_github(&user, code, now).await?,
        };
        let key = if provider == Provider::Github {
            let (login, id) = self.check_github(&credential).await?;
            credential.external_id = Some(login);
            id.map(|id| (IdentityKind::Github, id))
        } else {
            let account = self.check_identity(&credential).await?;
            if let Some(account) = &account {
                credential.external_id = Some(account.clone());
            }
            account.map(|a| (IdentityKind::Atlassian, a))
        };
        // The first link stores the account id; a later one with another account is refused
        // here, before anything is stored, even if a concurrent link got there first.
        if let Some((kind, subject)) = key {
            self.record_link(&user, provider, kind, subject, now)
                .await?;
        }
        self.vault.put(&credential).await?;
        self.settle_after_save(&user, provider, now).await?;
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

    /// After a credential is saved, check the person again (L9): a deactivation that
    /// committed between the checks above and the save suspended the credentials stored then,
    /// not this one. So the person is read again, from the store: deactivated now, their
    /// credentials are suspended here, as the deactivation would have (a reactivation within
    /// the grace restores them, the sweeper deletes them after); otherwise no longer allowed to
    /// use PM OS, this credential is deleted. Either way the link is refused. A deactivation
    /// after this read finds the credential stored and suspends it itself.
    pub(crate) async fn settle_after_save(
        &self,
        user: &str,
        provider: Provider,
        now: DateTime<Utc>,
    ) -> Result<(), OAuthError> {
        if is_bot_identity(user) || !self.people.enforced() {
            return Ok(());
        }
        let member = self.people.user(user).await?;
        if member.as_ref().is_some_and(may_use) {
            return Ok(());
        }
        if member.as_ref().is_some_and(|m| !m.active()) {
            self.vault.suspend(user, now).await?;
            tracing::warn!(user, provider = %provider, "deactivated while linking: the new credential is suspended");
        } else {
            self.vault.delete(user, provider).await?;
            tracing::warn!(user, provider = %provider, "no longer may use PM OS while linking: the new credential is deleted");
        }
        Err(OAuthError::WrongAccount(
            provider,
            "not an active PM OS user".into(),
        ))
    }

    /// Store the account id of `user`'s first Atlassian or GitHub link; a later link must
    /// present the same one.
    async fn record_link(
        &self,
        user: &str,
        provider: Provider,
        kind: IdentityKind,
        subject: String,
        now: DateTime<Utc>,
    ) -> Result<(), OAuthError> {
        let op = Op::LinkAccount {
            handle: user.to_string(),
            kind,
            subject,
        };
        match self.people.apply(&Actor::link(user), op, now).await {
            Ok(_) => {
                tracing::info!(user, provider = %provider, "first link: account id recorded");
                Ok(())
            }
            Err(e) => match e.refusal() {
                Some(Refusal::Unchanged) => Ok(()),
                Some(Refusal::DifferentAccount) => Err(OAuthError::DifferentAccount(provider)),
                Some(Refusal::Taken) => Err(OAuthError::WrongAccount(
                    provider,
                    "that account is linked to another PM OS user".into(),
                )),
                Some(_) => Err(OAuthError::WrongAccount(
                    provider,
                    "not an active PM OS user".into(),
                )),
                None => match e {
                    UserError::Store(e) => Err(OAuthError::Store(e)),
                    UserError::Refused { .. } => unreachable!("a refusal has a code"),
                },
            },
        }
    }

    /// The account that signed in must belong to this PM in the user database. Nothing is
    /// stored otherwise. Returns the Atlassian account id to record (Slack: none).
    ///
    /// A team bot's identity (`bots.<name>`) isn't a PM, so there is nothing to check it
    /// against: its link URL comes only from a platform owner's service-token call.
    async fn check_identity(&self, credential: &Credential) -> Result<Option<String>, OAuthError> {
        if is_bot_identity(&credential.user) {
            tracing::info!(identity = %credential.user, provider = %credential.provider, "linking a team bot's own account");
            return Ok(None);
        }
        if !self.people.enforced() {
            return Ok(None);
        }
        let provider = credential.provider;
        let wrong = |why: &str| Err(OAuthError::WrongAccount(provider, why.to_string()));
        let Some(member) = self.people.user(&credential.user).await?.filter(may_use) else {
            return wrong("not an active PM OS user");
        };
        match provider {
            Provider::Slack => {
                let slack = member
                    .identity(IdentityKind::Slack)
                    .map(|i| i.subject.as_str());
                if slack.is_none() || credential.external_id.as_deref() != slack {
                    return wrong("the Slack user isn't this PM's Slack account");
                }
                Ok(None)
            }
            Provider::Atlassian => {
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
                let Some(account) = me["account_id"]
                    .as_str()
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(String::from)
                else {
                    return Err(OAuthError::Exchange(
                        provider,
                        "the profile has no account id".into(),
                    ));
                };
                // After the first link the account id decides; the email is ignored.
                if let Some(linked) = member.identity(IdentityKind::Atlassian) {
                    if linked.subject != account {
                        return Err(OAuthError::DifferentAccount(provider));
                    }
                    return Ok(Some(account));
                }
                let Some(expected) = member.atlassian_email.as_deref() else {
                    return wrong("no Atlassian email is recorded for this PM");
                };
                let email = me["email"].as_str().unwrap_or_default();
                if !email.eq_ignore_ascii_case(expected) {
                    return wrong("the Atlassian email isn't this PM's");
                }
                Ok(Some(account))
            }
            Provider::Github => unreachable!("GitHub links are checked by check_github"),
        }
    }

    /// A GET to the GitHub API with a just-issued user token.
    async fn github_get(&self, token: &Secret, path: &str) -> Result<(u16, Value), OAuthError> {
        let res = self
            .http
            .get(format!("{}{path}", self.config.endpoints.github_api))
            .bearer_auth(token.expose())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", crate::github::API_VERSION)
            .header(
                "User-Agent",
                concat!("pmos-toolserver/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| OAuthError::Exchange(Provider::Github, e.to_string()))?;
        let status = res.status().as_u16();
        Ok((status, res.json().await.unwrap_or(Value::Null)))
    }

    /// The GitHub account that signed in must be an active member of the org, and the PM's:
    /// the numeric id of their first link, else their `github_login`, else a verified email
    /// equal to their `atlassian_email`. Returns the login and, when checked, the numeric id
    /// to record. Nothing is stored otherwise.
    async fn check_github(
        &self,
        credential: &Credential,
    ) -> Result<(String, Option<String>), OAuthError> {
        let provider = Provider::Github;
        let wrong = |why: String| Err(OAuthError::WrongAccount(provider, why));
        let failed = |what: &str, status: u16| {
            OAuthError::Exchange(provider, format!("{what} returned {status}"))
        };
        let token = &credential.access_token;
        let (status, me) = self.github_get(token, "/user").await?;
        let login = match me["login"].as_str() {
            Some(login) if status == 200 => login.to_string(),
            _ => return Err(failed("the profile lookup", status)),
        };
        let id = me["id"].as_u64().map(|id| id.to_string());
        let org = self
            .config
            .github_org
            .as_deref()
            .ok_or(OAuthError::NotConfigured(provider))?;
        let (status, membership) = self
            .github_get(token, &format!("/user/memberships/orgs/{org}"))
            .await?;
        match status {
            200 if membership["state"] == "active" => {}
            200 | 403 | 404 => {
                return wrong(format!(
                    "the GitHub account isn't an active member of `{org}`"
                ))
            }
            s => return Err(failed("the org membership lookup", s)),
        }
        if !self.people.enforced() {
            return Ok((login, None));
        }
        let Some(member) = self.people.user(&credential.user).await?.filter(may_use) else {
            return wrong("not an active PM OS user".into());
        };
        let Some(id) = id else {
            return Err(failed("the profile lookup (no user id)", status));
        };
        // After the first link the numeric id decides; the login is ignored.
        if let Some(linked) = member.identity(IdentityKind::Github) {
            if linked.subject != id {
                return Err(OAuthError::DifferentAccount(provider));
            }
            return Ok((login, Some(id)));
        }
        if let Some(expected) = member.github_login.as_deref() {
            if !login.eq_ignore_ascii_case(expected) {
                return wrong(
                    "the GitHub login isn't the github login recorded for this PM".into(),
                );
            }
            return Ok((login, Some(id)));
        }
        let Some(expected) = member.atlassian_email.as_deref() else {
            return wrong("no GitHub login or email is recorded for this PM".into());
        };
        let (status, emails) = self.github_get(token, "/user/emails").await?;
        if status != 200 {
            return Err(failed("the email lookup", status));
        }
        let verified = emails.as_array().into_iter().flatten().any(|e| {
            e["verified"].as_bool() == Some(true)
                && e["email"]
                    .as_str()
                    .is_some_and(|a| a.eq_ignore_ascii_case(expected))
        });
        if !verified {
            return wrong("no verified GitHub email is this PM's email".into());
        }
        Ok((login, Some(id)))
    }

    /// POST to GitHub's token endpoint. GitHub answers some errors as `200` with
    /// `{"error": …}`, so the body decides, not only the status.
    async fn post_github_token(&self, form: &[(&str, &str)]) -> Result<Value, (u16, String)> {
        let res = self
            .http
            .post(format!(
                "{}/login/oauth/access_token",
                self.config.endpoints.github_auth
            ))
            .header("Accept", "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| (0, e.to_string()))?;
        let status = res.status().as_u16();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        match json["error"].as_str() {
            Some(e) => Err((status, e.to_string())),
            None if (200..300).contains(&status) => Ok(json),
            None => Err((status, "error".to_string())),
        }
    }

    async fn exchange_github(
        &self,
        user: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Credential, OAuthError> {
        let app = self.app(Provider::Github)?;
        let redirect = self.redirect_uri(Provider::Github);
        let tokens = self
            .post_github_token(&[
                ("client_id", app.client_id.as_str()),
                ("client_secret", app.client_secret.expose()),
                ("code", code),
                ("redirect_uri", redirect.as_str()),
            ])
            .await
            .map_err(|(_, e)| OAuthError::Exchange(Provider::Github, e))?;
        github_credential(user, &tokens, now)
            .ok_or_else(|| OAuthError::Exchange(Provider::Github, "no access token".into()))
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
            Provider::Github => {
                let app = self
                    .app(provider)
                    .map_err(|e| TokenError::Unavailable(provider, e.to_string()))?;
                match self
                    .post_github_token(&[
                        ("client_id", app.client_id.as_str()),
                        ("client_secret", app.client_secret.expose()),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh.expose()),
                    ])
                    .await
                {
                    Ok(t) => github_credential(&current.user, &t, now)
                        .map(|c| Credential {
                            // Rotating: keep the new one; the old one stops working.
                            refresh_token: c.refresh_token.or(current.refresh_token.clone()),
                            external_id: current.external_id.clone(),
                            ..c
                        })
                        .ok_or(TokenError::Reconnect(provider))?,
                    // As with Atlassian, only a refused refresh token unlinks.
                    Err((_, e)) if e == "bad_refresh_token" => return self.unlink(&current).await,
                    Err((status, e)) => {
                        if e == "incorrect_client_credentials" || (400..500).contains(&status) {
                            tracing::error!(
                                status,
                                error = %e,
                                "GitHub refused the token refresh; check PMOS_GITHUB_CLIENT_ID/SECRET"
                            );
                        }
                        return Err(TokenError::Unavailable(provider, e));
                    }
                }
            }
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

/// The user token in a GitHub token response. It expires in 8 hours and its refresh token
/// in about 6 months, unless the App has token expiry turned off.
fn github_credential(user: &str, json: &Value, now: DateTime<Utc>) -> Option<Credential> {
    Some(Credential {
        user: user.to_string(),
        provider: Provider::Github,
        access_token: Secret::new(json["access_token"].as_str().filter(|t| !t.is_empty())?),
        refresh_token: json["refresh_token"].as_str().map(Secret::new),
        expires_at: json["expires_in"]
            .as_i64()
            .map(|s| now + Duration::seconds(s)),
        scopes: json["scope"].as_str().unwrap_or_default().to_string(),
        cloud_id: None,
        site_url: None,
        external_id: None,
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
    use crate::vault::{CredentialStore, KeyRing, MemoryCredentials};

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
                github: Some(AppCredentials {
                    client_id: "gh-client".into(),
                    client_secret: Secret::new("gh-secret"),
                    scopes: Vec::new(),
                }),
                github_org: Some("example-org".into()),
                state_key: b"state-key-for-tests".to_vec(),
                endpoints: Endpoints::default(),
            },
            vault,
            Arc::new(People::open(Arc::new(crate::users::MemoryUsers::default()))),
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
    fn github_authorize_url_has_no_scope_and_serves_pms_only() {
        let o = oauth();
        let url =
            reqwest::Url::parse(&o.authorize_url("ana", Provider::Github, now()).unwrap()).unwrap();
        assert_eq!(url.host_str(), Some("github.com"));
        assert_eq!(url.path(), "/login/oauth/authorize");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "gh-client");
        assert_eq!(
            q["redirect_uri"],
            "https://pmos.example.com/oauth/github/callback"
        );
        assert!(!q.contains_key("scope"));
        assert!(!url.as_str().contains("gh-secret"));
        assert_eq!(
            o.authorize_url("bots.docs", Provider::Github, now()),
            Err(OAuthError::InvalidUser)
        );
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
        // A team bot's identity links; anything else under `bots.` doesn't.
        assert!(o
            .authorize_url("bots.docs", Provider::Atlassian, now())
            .is_ok());
        for bad in ["bots.", "bots.Docs", "bots.a--b"] {
            assert_eq!(
                o.authorize_url(bad, Provider::Atlassian, now()),
                Err(OAuthError::InvalidUser),
                "{bad}"
            );
        }
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

    /// L9 (review of `b469d5b`): a credential saved after the person was deactivated (the
    /// deactivation committed between the link's checks and its save) ends up suspended, and
    /// the link is refused; an active person's stays as it is.
    #[tokio::test]
    async fn a_credential_saved_after_a_deactivation_is_suspended() {
        use crate::users::{Actor, HostUser, NewUser, Op, UserRole, UserStore};
        let users = Arc::new(crate::users::MemoryUsers::default());
        let host = Actor::host(HostUser::parse("opsuser").unwrap());
        let person = |h: &str, roles: &[UserRole]| NewUser {
            handle: h.into(),
            display_name: h.into(),
            sign_in_name: Some(format!("{h}@contoso.com")),
            atlassian_email: Some(format!("{h}@contoso.com")),
            github_login: None,
            slack_id: None,
            roles: roles.iter().copied().collect(),
        };
        users
            .apply(
                &host,
                Op::BootstrapAdmin {
                    user: person("raj", &[UserRole::Pm]),
                    entra: None,
                },
                now(),
            )
            .await
            .unwrap();
        for h in ["ana", "ben"] {
            users
                .apply(
                    &host,
                    Op::CreateUser {
                        user: person(h, &[UserRole::Pm]),
                    },
                    now(),
                )
                .await
                .unwrap();
        }
        let store = Arc::new(MemoryCredentials::default());
        let vault = Vault::new(
            KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
            store.clone(),
        );
        let mut o = oauth();
        o.vault = vault;
        o.people = Arc::new(People::new(users.clone()));
        let credential = |user: &str| Credential {
            user: user.into(),
            provider: Provider::Atlassian,
            access_token: Secret::new("fake-access-token"),
            refresh_token: None,
            expires_at: None,
            scopes: "read:jira-work".into(),
            cloud_id: None,
            site_url: None,
            external_id: None,
            updated_at: now(),
        };
        // The checks passed for ana; then she is deactivated; then the save lands.
        users
            .apply(
                &host,
                Op::Deactivate {
                    handle: "ana".into(),
                },
                now(),
            )
            .await
            .unwrap();
        o.vault.put(&credential("ana")).await.unwrap();
        let r = o.settle_after_save("ana", Provider::Atlassian, now()).await;
        assert!(matches!(r, Err(OAuthError::WrongAccount(..))), "{r:?}");
        let sealed = store
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .unwrap();
        assert!(
            sealed.suspended_at.is_some(),
            "suspended, as the deactivation would have"
        );
        assert!(o
            .vault
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .is_none());
        // An active person's credential is untouched.
        o.vault.put(&credential("ben")).await.unwrap();
        o.settle_after_save("ben", Provider::Atlassian, now())
            .await
            .unwrap();
        assert!(o
            .vault
            .get("ben", Provider::Atlassian)
            .await
            .unwrap()
            .is_some());
    }
}
