//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::assertion::Assertions;

use crate::github::{valid_login, valid_repo};
use crate::oauth::{AppCredentials, Endpoints};
use crate::vault::{KeyRing, Secret};

/// Default Atlassian scopes: Jira classic scopes, Confluence granular scopes for the v2 page
/// API and CQL search. `offline_access` is always added for refresh tokens.
pub const DEFAULT_ATLASSIAN_SCOPES: &str = "read:me read:jira-work write:jira-work read:jira-user \
     search:confluence read:page:confluence write:page:confluence read:space:confluence";

/// Default Slack user scopes: Real-time Search, thread history, channel info and members (the
/// audience check before a post), and posting as the PM.
pub const DEFAULT_SLACK_SCOPES: &str = "search:read.public search:read.private search:read.im \
     search:read.mpim channels:history groups:history im:history mpim:history channels:read \
     groups:read mpim:read im:read chat:write";

/// What `PMOS_CONNECTORS=live` needs.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// `PMOS_VAULT_KEY`: `key_id:base64key[,…]`, first key seals.
    pub vault_keys: KeyRing,
    /// `PMOS_PUBLIC_URL`: where PMs' browsers reach the tool server, for OAuth callbacks.
    pub public_url: String,
    /// `PMOS_ATLASSIAN_CLIENT_ID`, `PMOS_ATLASSIAN_CLIENT_SECRET`, `PMOS_ATLASSIAN_SCOPES`.
    pub atlassian: AppCredentials,
    /// `PMOS_ATLASSIAN_SITE`: required when a PM's grant covers several sites.
    pub atlassian_site: Option<String>,
    /// `PMOS_SLACK_CLIENT_ID`, `PMOS_SLACK_CLIENT_SECRET`, `PMOS_SLACK_SCOPES`.
    pub slack: AppCredentials,
    /// The GitHub App and the content repo, when `PMOS_GITHUB_CLIENT_ID` and
    /// `PMOS_GITHUB_CLIENT_SECRET` are set. Without them GitHub linking, `PMOS_MEMORY=github`
    /// and approving members are off.
    pub github: Option<GitHubConfig>,
    /// `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL`,
    /// `PMOS_GITHUB_AUTH_URL`, `PMOS_GITHUB_URL`: overrides for tests against mock APIs.
    /// Default to the real hosts.
    pub endpoints: Endpoints,
    /// `PMOS_DOCS_URL`: the `pmos-docs` MCP endpoint, such as `http://docs.pmos.internal:8730/mcp`.
    pub docs_url: String,
    /// `PMOS_DOCS_TOKEN`: the bearer token `pmos-docs` expects.
    pub docs_token: Secret,
}

/// GitHub (`docs/onboarding-spec.md` §1): a GitHub App used only with each PM's user token.
#[derive(Debug, Clone)]
pub struct GitHubConfig {
    /// `PMOS_GITHUB_CLIENT_ID`, `PMOS_GITHUB_CLIENT_SECRET`. The App's permissions apply, so
    /// there are no scopes.
    pub app: AppCredentials,
    /// `PMOS_GITHUB_ORG`: the org login. Memory repos and the content repo must be in it.
    pub org: String,
    /// `PMOS_CONTENT_REPO`: `org/name` of the content repo, needed to approve members.
    pub content_repo: Option<String>,
    /// `PMOS_CONTENT_BRANCH`. Default `main`.
    pub content_branch: String,
    /// `PMOS_CONTENT_DIR`: where the content root sits in that repo. Default `content`; empty
    /// for the repo root.
    pub content_dir: String,
    /// `PMOS_POLICIES_DIR`: where the sandbox policies sit in that repo, from its root.
    /// Default `policies`. A new member's two policy files are committed under it.
    pub policies_dir: String,
}

/// A folder in the content repo, from its root: `.` or empty for the root itself.
fn repo_dir(var: &str, value: &str) -> Result<String, String> {
    let dir = value.trim().trim_matches('/');
    let dir = if dir == "." { "" } else { dir };
    let ok = dir.is_empty()
        || dir.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if ok {
        Ok(dir.to_string())
    } else {
        Err(format!(
            "{var} must be a relative path in the content repo, not `{value}`"
        ))
    }
}

/// Where personal memory lives: `PMOS_MEMORY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBackend {
    /// `store`: Postgres, or memory with `PMOS_DEV=1`.
    Store,
    /// `github`: each PM's own private repo in the GitHub org.
    Github,
}

#[derive(Debug, Clone)]
pub enum Connectors {
    Fake,
    Live(Box<LiveConfig>),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `PMOS_CONTENT_ROOT`: the content repo checkout. Default `content`.
    pub content_root: PathBuf,
    /// `PMOS_BIND`: listen address. Default `127.0.0.1:8710`.
    pub bind: SocketAddr,
    /// `PMOS_SERVICE_TOKEN`: required; the orchestrator's token for `/internal`. At least 32
    /// characters.
    pub service_token: Secret,
    /// `PMOS_BOT_TOKEN`: the Slack bot's own token, at least 32 characters and different from
    /// the service token. Optional; without it the bot can't connect.
    pub bot_token: Option<Secret>,
    /// `PMOS_WEB_TOKEN`: `pmos-web`'s own token, for the approvals routes only. Optional; at
    /// least 32 characters and different from every other token.
    pub web_token: Option<Secret>,
    /// `PMOS_CONSOLE_TS_TOKEN`: the web console's own token, at least 32 characters and
    /// different from every other token. Optional. It may only read approvals
    /// (`GET /internal/approvals` and `GET /internal/approvals/{id}`).
    pub console_token: Option<Secret>,
    /// `PMOS_ASSERTION_MODE` and `PMOS_ASSERTION_KEYS`: user assertions from `pmos-web` and
    /// the console on the approvals routes (`crate::assertion`). Off unless set.
    pub assertions: Arc<Assertions>,
    /// `PMOS_BOT_SKILLS`: space- or comma-separated skills the bot may start sessions for.
    /// Default `docs`.
    pub bot_skills: Vec<String>,
    /// `PMOS_DATABASE_URL`: Postgres for every store. Required unless `PMOS_DEV=1`, which
    /// keeps everything in memory.
    pub database_url: Option<String>,
    /// `PMOS_AUDIT_LOG`: also write audit events to this JSON-lines file, for shipping
    /// off-box. Optional.
    pub audit_log: Option<PathBuf>,
    /// `PMOS_ALLOWED_HOSTS`: comma-separated `Host` values for `/mcp`.
    /// Default `toolserver.pmos.internal,localhost,127.0.0.1`.
    pub allowed_hosts: Vec<String>,
    /// `PMOS_CONNECTORS`: `live` (real Atlassian and Slack, each PM's own token) or `fake`
    /// (fixtures, for dev and tests). Required, so the choice is always deliberate.
    pub connectors: Connectors,
    /// With `PMOS_CONNECTORS=fake`: `PMOS_DOCS_URL` and `PMOS_DOCS_TOKEN` both set point the
    /// docs tools at a real `pmos-docs` (the public support docs) while Jira, Confluence and
    /// Slack stay fixtures. For testing a bot on real docs without any PM's tokens.
    pub fake_mode_docs: Option<(String, Secret)>,
    /// `PMOS_MEMORY`: `store` (default) or `github`, which needs live connectors and GitHub.
    pub memory: MemoryBackend,
    /// `PMOS_CONTENT_SYNC_SECS`: how often the content checkout is pulled; `0` turns it off.
    /// Default 300 with live connectors, 0 with fake ones (a dev checkout isn't pulled).
    pub content_sync_secs: u64,
    /// `PMOS_LEDGER_EXPIRE_DAYS`: a draft still pending after this many days is reported as
    /// `expired` in the feedback ledger. Default 7.
    pub ledger_expire_days: i64,
    /// `PMOS_SLACK_DRAFT_TTL_HOURS`: an undecided Slack draft expires after this many hours,
    /// and a decided one loses its text this long after its decision. 1 to 24; default 24.
    pub slack_draft_ttl_hours: i64,
    /// `PMOS_AUDIT_RETENTION_DAYS`: audit events older than this many days are deleted once
    /// a day (`docs/audit-retention.md`). From 90 (the floor, and the default) to 3650.
    pub audit_retention_days: u32,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let service_token = get("PMOS_SERVICE_TOKEN").ok_or("PMOS_SERVICE_TOKEN is required")?;
        if service_token.len() < 32 {
            return Err("PMOS_SERVICE_TOKEN must be at least 32 characters".into());
        }
        let bot_token = get("PMOS_BOT_TOKEN").filter(|t| !t.is_empty());
        if let Some(t) = &bot_token {
            if t.len() < 32 {
                return Err("PMOS_BOT_TOKEN must be at least 32 characters".into());
            }
            if t == &service_token {
                return Err("PMOS_BOT_TOKEN must differ from PMOS_SERVICE_TOKEN".into());
            }
        }
        let mut seen: Vec<String> = std::iter::once(service_token.clone())
            .chain(bot_token.clone())
            .collect();
        let mut person_token = |k: &str| -> Result<Option<String>, String> {
            let Some(t) = get(k).filter(|t| !t.is_empty()) else {
                return Ok(None);
            };
            if t.len() < 32 {
                return Err(format!("{k} must be at least 32 characters"));
            }
            if seen.contains(&t) {
                return Err(format!(
                    "{k} must differ from PMOS_SERVICE_TOKEN, PMOS_BOT_TOKEN and the other \
                     client tokens"
                ));
            }
            seen.push(t.clone());
            Ok(Some(t))
        };
        let web_token = person_token("PMOS_WEB_TOKEN")?;
        let console_token = person_token("PMOS_CONSOLE_TS_TOKEN")?;
        // A client that acts for a person makes `require` the default, and `off` or `verify`
        // refuses to start without the development override (W2).
        let person_clients: Vec<(&str, &str)> = [
            web_token.as_ref().map(|_| ("web", "PMOS_WEB_TOKEN")),
            console_token
                .as_ref()
                .map(|_| ("console", "PMOS_CONSOLE_TS_TOKEN")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let assertions = Arc::new(Assertions::from_lookup(&get, &person_clients)?);
        let bot_skills: Vec<String> = get("PMOS_BOT_SKILLS")
            .unwrap_or_else(|| "docs".into())
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let dev = get("PMOS_DEV").as_deref() == Some("1");
        if dev && get("PMOS_CONNECTORS").as_deref() == Some("live") {
            return Err(
                "PMOS_DEV=1 keeps tokens and the audit log in memory; it can't be used with PMOS_CONNECTORS=live"
                    .into(),
            );
        }
        let audit_log = get("PMOS_AUDIT_LOG").map(PathBuf::from);
        let database_url = get("PMOS_DATABASE_URL").filter(|u| !u.is_empty());
        if database_url.is_none() && !dev {
            return Err(
                "PMOS_DATABASE_URL is required (or set PMOS_DEV=1 to keep everything in memory)"
                    .into(),
            );
        }
        let connectors = match get("PMOS_CONNECTORS").as_deref() {
            Some("fake") => Connectors::Fake,
            Some("live") => Connectors::Live(Box::new(live_config(&get)?)),
            Some(other) => {
                return Err(format!(
                    "PMOS_CONNECTORS must be `live` or `fake`, not `{other}`"
                ))
            }
            None => return Err("PMOS_CONNECTORS is required (`live` or `fake`)".into()),
        };
        let fake_mode_docs = match (&connectors, get("PMOS_DOCS_URL"), get("PMOS_DOCS_TOKEN")) {
            (Connectors::Fake, Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                Some((url, Secret::new(token)))
            }
            _ => None,
        };
        let memory = match get("PMOS_MEMORY").as_deref() {
            None | Some("") | Some("store") => MemoryBackend::Store,
            Some("github") => MemoryBackend::Github,
            Some(other) => {
                return Err(format!(
                    "PMOS_MEMORY must be `store` or `github`, not `{other}`"
                ))
            }
        };
        let github_on = matches!(&connectors, Connectors::Live(live) if live.github.is_some());
        if memory == MemoryBackend::Github && !github_on {
            return Err("PMOS_MEMORY=github needs PMOS_CONNECTORS=live and GitHub \
                        (PMOS_GITHUB_CLIENT_ID, PMOS_GITHUB_CLIENT_SECRET, PMOS_GITHUB_ORG)"
                .into());
        }
        let content_sync_secs = match get("PMOS_CONTENT_SYNC_SECS").filter(|s| !s.is_empty()) {
            Some(s) => s.trim().parse().map_err(|_| {
                format!("PMOS_CONTENT_SYNC_SECS must be a number of seconds, not `{s}`")
            })?,
            None if matches!(connectors, Connectors::Live(_)) => 300,
            None => 0,
        };
        let ledger_expire_days = match get("PMOS_LEDGER_EXPIRE_DAYS").filter(|s| !s.is_empty()) {
            Some(s) => s
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|d| *d >= 1)
                .ok_or_else(|| {
                    format!("PMOS_LEDGER_EXPIRE_DAYS must be a whole number of days (1 or more), not `{s}`")
                })?,
            None => crate::ledger::DEFAULT_EXPIRE_DAYS,
        };
        let max_ttl = crate::service::DEFAULT_SLACK_DRAFT_TTL_HOURS;
        let slack_draft_ttl_hours = match get("PMOS_SLACK_DRAFT_TTL_HOURS").filter(|s| !s.is_empty()) {
            Some(s) => s
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|h| (1..=max_ttl).contains(h))
                .ok_or_else(|| {
                    format!("PMOS_SLACK_DRAFT_TTL_HOURS must be a whole number of hours from 1 to {max_ttl} (no Slack text is kept longer), not `{s}`")
                })?,
            None => max_ttl,
        };
        let (min_days, max_days) = (
            crate::audit::MIN_RETENTION_DAYS,
            crate::audit::MAX_RETENTION_DAYS,
        );
        let audit_retention_days = match get("PMOS_AUDIT_RETENTION_DAYS").filter(|s| !s.is_empty()) {
            Some(s) => match s.trim().parse::<u32>() {
                Ok(d) if d < min_days => {
                    return Err(format!(
                        "PMOS_AUDIT_RETENTION_DAYS={d} is shorter than the {min_days}-day minimum: \
                         audit events are kept at least {min_days} days (docs/audit-retention.md). \
                         Set {min_days} or more, or leave it out for {min_days}."
                    ))
                }
                Ok(d) if d <= max_days => d,
                _ => {
                    return Err(format!(
                        "PMOS_AUDIT_RETENTION_DAYS must be a whole number of days from {min_days} to {max_days}, not `{s}`"
                    ))
                }
            },
            None => min_days,
        };
        let bind = get("PMOS_BIND")
            .unwrap_or_else(|| "127.0.0.1:8710".into())
            .parse()
            .map_err(|e| format!("PMOS_BIND: {e}"))?;
        let allowed_hosts = get("PMOS_ALLOWED_HOSTS")
            .unwrap_or_else(|| "toolserver.pmos.internal,localhost,127.0.0.1".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Self {
            content_root: get("PMOS_CONTENT_ROOT")
                .unwrap_or_else(|| "content".into())
                .into(),
            bind,
            service_token: Secret::new(service_token),
            bot_token: bot_token.map(Secret::new),
            web_token: web_token.map(Secret::new),
            console_token: console_token.map(Secret::new),
            assertions,
            bot_skills,
            database_url,
            audit_log,
            allowed_hosts,
            connectors,
            fake_mode_docs,
            memory,
            content_sync_secs,
            ledger_expire_days,
            slack_draft_ttl_hours,
            audit_retention_days,
        })
    }
}

/// The GitHub settings, given the App's id and secret.
fn github_config(
    get: &impl Fn(&str) -> Option<String>,
    client_id: String,
    client_secret: String,
) -> Result<GitHubConfig, String> {
    let set = |k: &str| {
        get(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let org = set("PMOS_GITHUB_ORG").ok_or("PMOS_GITHUB_ORG is required with the GitHub App")?;
    if !valid_login(&org) {
        return Err(format!(
            "PMOS_GITHUB_ORG: `{org}` is not a GitHub org login"
        ));
    }
    let content_repo = set("PMOS_CONTENT_REPO");
    if let Some(repo) = &content_repo {
        if !valid_repo(repo) {
            return Err(format!(
                "PMOS_CONTENT_REPO must be `org/name`, not `{repo}`"
            ));
        }
        let owner = repo.split('/').next().unwrap_or_default();
        if !owner.eq_ignore_ascii_case(&org) {
            return Err(format!(
                "PMOS_CONTENT_REPO must be in PMOS_GITHUB_ORG (`{org}`), not `{owner}`"
            ));
        }
    }
    let content_branch = set("PMOS_CONTENT_BRANCH").unwrap_or_else(|| "main".into());
    let branch_ok = content_branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !content_branch.contains("..")
        && !content_branch.starts_with('/')
        && !content_branch.ends_with('/');
    if !branch_ok {
        return Err(format!(
            "PMOS_CONTENT_BRANCH: `{content_branch}` is not a branch name"
        ));
    }
    let content_dir = repo_dir(
        "PMOS_CONTENT_DIR",
        &get("PMOS_CONTENT_DIR").unwrap_or_else(|| "content".into()),
    )?;
    let policies_dir = repo_dir(
        "PMOS_POLICIES_DIR",
        &get("PMOS_POLICIES_DIR").unwrap_or_else(|| "policies".into()),
    )?;
    Ok(GitHubConfig {
        app: AppCredentials {
            client_id,
            client_secret: Secret::new(client_secret),
            scopes: Vec::new(),
        },
        org,
        content_repo,
        content_branch,
        content_dir,
        policies_dir,
    })
}

fn live_config(get: &impl Fn(&str) -> Option<String>) -> Result<LiveConfig, String> {
    let need = |k: &str| {
        get(k)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{k} is required with PMOS_CONNECTORS=live"))
    };
    let scopes = |k: &str, default: &str| -> Vec<String> {
        get(k)
            .unwrap_or_else(|| default.to_string())
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    let vault_keys =
        KeyRing::from_spec(&need("PMOS_VAULT_KEY")?).map_err(|e| format!("PMOS_VAULT_KEY: {e}"))?;
    let public_url = need("PMOS_PUBLIC_URL")?;
    if !public_url.starts_with("https://") && !public_url.starts_with("http://localhost") {
        return Err("PMOS_PUBLIC_URL must be an https:// URL".into());
    }
    let defaults = Endpoints::default();
    let github_id = get("PMOS_GITHUB_CLIENT_ID").filter(|v| !v.trim().is_empty());
    let github_secret = get("PMOS_GITHUB_CLIENT_SECRET").filter(|v| !v.trim().is_empty());
    let github = match (github_id, github_secret) {
        (Some(id), Some(secret)) => Some(github_config(get, id, secret)?),
        (None, None) => {
            for k in ["PMOS_GITHUB_ORG", "PMOS_CONTENT_REPO"] {
                if get(k).is_some_and(|v| !v.trim().is_empty()) {
                    return Err(format!(
                        "{k} needs the GitHub App: set PMOS_GITHUB_CLIENT_ID and PMOS_GITHUB_CLIENT_SECRET"
                    ));
                }
            }
            None
        }
        _ => {
            return Err(
                "PMOS_GITHUB_CLIENT_ID and PMOS_GITHUB_CLIENT_SECRET must be set together".into(),
            )
        }
    };
    Ok(LiveConfig {
        vault_keys,
        public_url,
        atlassian: AppCredentials {
            client_id: need("PMOS_ATLASSIAN_CLIENT_ID")?,
            client_secret: Secret::new(need("PMOS_ATLASSIAN_CLIENT_SECRET")?),
            scopes: scopes("PMOS_ATLASSIAN_SCOPES", DEFAULT_ATLASSIAN_SCOPES),
        },
        atlassian_site: get("PMOS_ATLASSIAN_SITE").filter(|s| !s.is_empty()),
        slack: AppCredentials {
            client_id: need("PMOS_SLACK_CLIENT_ID")?,
            client_secret: Secret::new(need("PMOS_SLACK_CLIENT_SECRET")?),
            scopes: scopes("PMOS_SLACK_SCOPES", DEFAULT_SLACK_SCOPES),
        },
        github,
        endpoints: Endpoints {
            atlassian_auth: get("PMOS_ATLASSIAN_AUTH_URL").unwrap_or(defaults.atlassian_auth),
            atlassian_api: get("PMOS_ATLASSIAN_API_URL").unwrap_or(defaults.atlassian_api),
            slack: get("PMOS_SLACK_URL").unwrap_or(defaults.slack),
            github_auth: get("PMOS_GITHUB_AUTH_URL").unwrap_or(defaults.github_auth),
            github_api: get("PMOS_GITHUB_URL").unwrap_or(defaults.github_api),
        },
        docs_url: need("PMOS_DOCS_URL")?,
        docs_token: Secret::new(need("PMOS_DOCS_TOKEN")?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn dev_mode_and_live_connectors_dont_mix_and_tokens_never_print() {
        let err = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "live"),
        ])
        .unwrap_err();
        assert!(err.contains("PMOS_DEV=1"), "{err}");

        let ok = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_BOT_TOKEN", "bot-token-0123456789abcdef-0123456"),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ])
        .unwrap();
        let printed = format!("{ok:?}");
        assert!(!printed.contains(TOKEN), "{printed}");
        assert!(!printed.contains("bot-token"), "{printed}");
        assert_eq!(ok.bot_skills, vec!["docs"]);
    }

    #[test]
    fn person_tokens_and_assertions() {
        let web = "web-token-0123456789abcdef-01234567";
        let console = "console-token-0123456789abcdef-0123";
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:toolserver:k1:{key},console:toolserver:c1:{key}");
        let base = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        let with = |extra: &[(&str, &str)]| {
            let mut pairs = base.to_vec();
            pairs.extend_from_slice(extra);
            cfg(&pairs)
        };
        let ok = with(&[
            ("PMOS_WEB_TOKEN", web),
            ("PMOS_CONSOLE_TS_TOKEN", console),
            ("PMOS_ASSERTION_MODE", "require"),
            ("PMOS_ASSERTION_KEYS", &keys),
        ])
        .unwrap();
        assert_eq!(ok.assertions.mode(), crate::assertion::Mode::Require);
        let printed = format!("{ok:?}");
        for secret in [web, console, key] {
            assert!(!printed.contains(secret), "{printed}");
        }
        // Off by default; on without keys refuses to start.
        assert_eq!(
            with(&[]).unwrap().assertions.mode(),
            crate::assertion::Mode::Off
        );
        let err = with(&[("PMOS_ASSERTION_MODE", "verify")]).unwrap_err();
        assert!(err.contains("PMOS_ASSERTION_KEYS"), "{err}");
        // With a person client, `require` is the default (W2) ...
        for token in [("PMOS_WEB_TOKEN", web), ("PMOS_CONSOLE_TS_TOKEN", console)] {
            let ok = with(&[token, ("PMOS_ASSERTION_KEYS", &keys)]).unwrap();
            assert_eq!(ok.assertions.mode(), crate::assertion::Mode::Require);
            assert!(ok.assertions.insecure_warning().is_none());
            // ... it needs keys ...
            let err = with(&[token]).unwrap_err();
            assert!(err.contains("PMOS_ASSERTION_KEYS"), "{err}");
            // ... and `off` or `verify` refuses to start, naming the token and the fix.
            for mode in ["off", "verify"] {
                let err = with(&[
                    token,
                    ("PMOS_ASSERTION_MODE", mode),
                    ("PMOS_ASSERTION_KEYS", &keys),
                ])
                .unwrap_err();
                assert!(
                    err.contains(token.0) && err.contains("PMOS_ASSERTION_INSECURE_OFF"),
                    "{err}"
                );
            }
        }
        // Only the development override lets it start insecurely, with a warning to log.
        let dev = with(&[
            ("PMOS_WEB_TOKEN", web),
            ("PMOS_ASSERTION_MODE", "off"),
            ("PMOS_ASSERTION_INSECURE_OFF", "1"),
        ])
        .unwrap();
        assert_eq!(dev.assertions.mode(), crate::assertion::Mode::Off);
        assert!(dev
            .assertions
            .insecure_warning()
            .unwrap()
            .contains("INSECURE"));
        // ... and never with live connectors (R3).
        let err = with(&[
            ("PMOS_WEB_TOKEN", web),
            ("PMOS_ASSERTION_MODE", "off"),
            ("PMOS_ASSERTION_INSECURE_OFF", "1"),
            ("PMOS_CONNECTORS", "live"),
        ])
        .unwrap_err();
        assert!(
            err.contains("PMOS_ASSERTION_INSECURE_OFF") && err.contains("fake-data development"),
            "{err}"
        );
        // Tokens are long and distinct.
        assert!(with(&[("PMOS_WEB_TOKEN", "short")])
            .unwrap_err()
            .contains("32"));
        assert!(with(&[("PMOS_WEB_TOKEN", TOKEN)])
            .unwrap_err()
            .contains("differ"));
        assert!(
            with(&[("PMOS_WEB_TOKEN", web), ("PMOS_CONSOLE_TS_TOKEN", web)])
                .unwrap_err()
                .contains("differ")
        );
    }

    #[test]
    fn the_console_token_is_optional_long_and_its_own() {
        let base = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_BOT_TOKEN", "bot-token-0123456789abcdef-0123456"),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        assert!(cfg(&base).unwrap().console_token.is_none());
        let keys = "console:toolserver:c1:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let with = |token: &'static str| {
            let mut vars = base.to_vec();
            vars.push(("PMOS_CONSOLE_TS_TOKEN", token));
            vars.push(("PMOS_ASSERTION_KEYS", keys));
            cfg(&vars)
        };
        let ok = with("console-token-0123456789abcdef-012").unwrap();
        assert_eq!(
            ok.console_token.as_ref().map(Secret::expose),
            Some("console-token-0123456789abcdef-012")
        );
        assert!(!format!("{ok:?}").contains("console-token"));
        assert!(with("short").unwrap_err().contains("at least 32"));
        assert!(with(TOKEN).unwrap_err().contains("must differ"));
        assert!(with("bot-token-0123456789abcdef-0123456")
            .unwrap_err()
            .contains("must differ"));
    }

    #[test]
    fn requires_token_audit_and_connectors() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[
            ("PMOS_SERVICE_TOKEN", "short"),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake")
        ])
        .is_err());
        assert!(
            cfg(&[("PMOS_SERVICE_TOKEN", TOKEN), ("PMOS_CONNECTORS", "fake")])
                .unwrap_err()
                .contains("PMOS_DATABASE_URL")
        );
        assert!(cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "atlassian")
        ])
        .is_err());
        let live_missing = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DATABASE_URL", "postgresql://pmos@db/pmos"),
            ("PMOS_CONNECTORS", "live"),
        ])
        .unwrap_err();
        assert!(live_missing.contains("PMOS_VAULT_KEY"), "{live_missing}");
        let ok = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ])
        .unwrap();
        assert_eq!(ok.bind.port(), 8710);
        assert!(ok
            .allowed_hosts
            .contains(&"toolserver.pmos.internal".to_string()));
        assert!(matches!(ok.connectors, Connectors::Fake));
        assert!(ok.fake_mode_docs.is_none());
        let docs = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
            ("PMOS_DOCS_URL", "http://127.0.0.1:8730/mcp"),
            ("PMOS_DOCS_TOKEN", "docs-token"),
        ])
        .unwrap();
        let (url, _) = docs.fake_mode_docs.expect("fake mode can use real docs");
        assert_eq!(url, "http://127.0.0.1:8730/mcp");
    }

    #[test]
    fn live_needs_everything_and_defaults_scopes() {
        let key = KeyRing::generate_spec("k1");
        let base = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DATABASE_URL", "postgresql://pmos@db/pmos"),
            ("PMOS_CONNECTORS", "live"),
            ("PMOS_VAULT_KEY", key.as_str()),
            ("PMOS_PUBLIC_URL", "https://pmos.example.com"),
            ("PMOS_ATLASSIAN_CLIENT_ID", "atl"),
            ("PMOS_ATLASSIAN_CLIENT_SECRET", "atl-secret"),
            ("PMOS_SLACK_CLIENT_ID", "slk"),
            ("PMOS_SLACK_CLIENT_SECRET", "slk-secret"),
            ("PMOS_DOCS_URL", "http://docs.pmos.internal:8730/mcp"),
            ("PMOS_DOCS_TOKEN", "docs-token"),
        ];
        let ok = cfg(&base).unwrap();
        let Connectors::Live(live) = ok.connectors else {
            panic!("expected live");
        };
        assert!(live
            .atlassian
            .scopes
            .contains(&"write:page:confluence".to_string()));
        assert!(live.slack.scopes.contains(&"chat:write".to_string()));
        // The audience check reads channel info and members as the PM.
        for scope in ["channels:read", "groups:read", "mpim:read", "im:read"] {
            assert!(live.slack.scopes.contains(&scope.to_string()), "{scope}");
        }
        assert_eq!(live.endpoints.atlassian_api, "https://api.atlassian.com");
        assert!(
            !format!("{live:?}").contains("atl-secret"),
            "secrets never print"
        );

        for skip in [
            "PMOS_VAULT_KEY",
            "PMOS_PUBLIC_URL",
            "PMOS_SLACK_CLIENT_SECRET",
            "PMOS_DOCS_URL",
        ] {
            let partial: Vec<_> = base.iter().copied().filter(|(k, _)| *k != skip).collect();
            assert!(cfg(&partial).unwrap_err().contains(skip), "{skip}");
        }
        let mut http_url = base.to_vec();
        http_url.retain(|(k, _)| *k != "PMOS_PUBLIC_URL");
        http_url.push(("PMOS_PUBLIC_URL", "http://pmos.example.com"));
        assert!(cfg(&http_url).unwrap_err().contains("https://"));

        // GitHub is off without the App, and memory stays in the store.
        assert!(live.github.is_none());
        assert_eq!(ok.memory, MemoryBackend::Store);
        assert_eq!(ok.content_sync_secs, 300);
    }

    fn live_base(key: &str) -> Vec<(&str, &str)> {
        vec![
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DATABASE_URL", "postgresql://pmos@db/pmos"),
            ("PMOS_CONNECTORS", "live"),
            ("PMOS_VAULT_KEY", key),
            ("PMOS_PUBLIC_URL", "https://pmos.example.com"),
            ("PMOS_ATLASSIAN_CLIENT_ID", "atl"),
            ("PMOS_ATLASSIAN_CLIENT_SECRET", "atl-secret"),
            ("PMOS_SLACK_CLIENT_ID", "slk"),
            ("PMOS_SLACK_CLIENT_SECRET", "slk-secret"),
            ("PMOS_DOCS_URL", "http://docs.pmos.internal:8730/mcp"),
            ("PMOS_DOCS_TOKEN", "docs-token"),
        ]
    }

    fn with<'a>(
        base: &[(&'a str, &'a str)],
        extra: &[(&'a str, &'a str)],
    ) -> Vec<(&'a str, &'a str)> {
        let mut v: Vec<_> = base
            .iter()
            .copied()
            .filter(|(k, _)| !extra.iter().any(|(e, _)| e == k))
            .collect();
        v.extend_from_slice(extra);
        v
    }

    #[test]
    fn github_settings_are_checked_at_start_up() {
        let key = KeyRing::generate_spec("k1");
        let base = live_base(&key);
        let app = [
            ("PMOS_GITHUB_CLIENT_ID", "Iv1.gh"),
            ("PMOS_GITHUB_CLIENT_SECRET", "gh-secret"),
            ("PMOS_GITHUB_ORG", "example-org"),
        ];
        let ok = cfg(&with(&base, &app)).unwrap();
        let Connectors::Live(live) = &ok.connectors else {
            panic!("expected live");
        };
        let gh = live.github.as_ref().unwrap();
        assert_eq!(gh.org, "example-org");
        assert_eq!(gh.content_branch, "main");
        assert_eq!(gh.content_dir, "content");
        assert_eq!(gh.policies_dir, "policies");
        assert!(gh.content_repo.is_none());
        assert_eq!(live.endpoints.github_api, "https://api.github.com");
        assert_eq!(live.endpoints.github_auth, "https://github.com");
        assert!(
            !format!("{ok:?}").contains("gh-secret"),
            "secrets never print"
        );

        let full = cfg(&with(
            &with(&base, &app),
            &[
                ("PMOS_CONTENT_REPO", "Example-Org/pm-os"),
                ("PMOS_CONTENT_DIR", "./"),
                ("PMOS_MEMORY", "github"),
                ("PMOS_CONTENT_SYNC_SECS", "0"),
                ("PMOS_GITHUB_URL", "http://127.0.0.1:9"),
            ],
        ))
        .unwrap();
        assert_eq!(full.memory, MemoryBackend::Github);
        assert_eq!(full.content_sync_secs, 0);
        let Connectors::Live(live) = &full.connectors else {
            panic!("expected live");
        };
        let gh = live.github.as_ref().unwrap();
        assert_eq!(gh.content_repo.as_deref(), Some("Example-Org/pm-os"));
        assert_eq!(gh.content_dir, "", "`./` is the repo root");
        assert_eq!(live.endpoints.github_api, "http://127.0.0.1:9");

        let refused = |extra: &[(&str, &str)], needle: &str| {
            let err = cfg(&with(&base, extra)).unwrap_err();
            assert!(err.contains(needle), "{extra:?}: {err}");
        };
        refused(&[("PMOS_MEMORY", "github")], "PMOS_MEMORY=github needs");
        refused(&[("PMOS_MEMORY", "s3")], "PMOS_MEMORY must be");
        refused(&[("PMOS_GITHUB_CLIENT_ID", "x")], "set together");
        refused(
            &[("PMOS_GITHUB_ORG", "example-org")],
            "needs the GitHub App",
        );
        refused(
            &[("PMOS_CONTENT_REPO", "example-org/pm-os")],
            "needs the GitHub App",
        );
        refused(&app[..2], "PMOS_GITHUB_ORG is required");
        let app_with = |extra: (&'static str, &'static str)| {
            let mut v = app.to_vec();
            v.push(extra);
            v
        };
        refused(
            &app_with(("PMOS_CONTENT_REPO", "pm-os")),
            "must be `org/name`",
        );
        refused(
            &app_with(("PMOS_CONTENT_REPO", "other/pm-os")),
            "must be in PMOS_GITHUB_ORG",
        );
        refused(&app_with(("PMOS_CONTENT_DIR", "../x")), "PMOS_CONTENT_DIR");
        refused(
            &app_with(("PMOS_POLICIES_DIR", "/abs/../x")),
            "PMOS_POLICIES_DIR",
        );
        refused(
            &app_with(("PMOS_CONTENT_BRANCH", "a b")),
            "PMOS_CONTENT_BRANCH",
        );
        refused(
            &app_with(("PMOS_GITHUB_ORG", "-bad")),
            "not a GitHub org login",
        );
        refused(
            &app_with(("PMOS_CONTENT_SYNC_SECS", "soon")),
            "PMOS_CONTENT_SYNC_SECS",
        );

        // Fake connectors can't keep memory in GitHub, and don't pull by default.
        let fake = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        assert_eq!(cfg(&fake).unwrap().content_sync_secs, 0);
        assert!(cfg(&with(&fake, &[("PMOS_MEMORY", "github")]))
            .unwrap_err()
            .contains("PMOS_MEMORY=github needs"));
    }

    #[test]
    fn ledger_expiry_defaults_to_a_week() {
        let fake = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        assert_eq!(cfg(&fake).unwrap().ledger_expire_days, 7);
        assert_eq!(
            cfg(&with(&fake, &[("PMOS_LEDGER_EXPIRE_DAYS", "14")]))
                .unwrap()
                .ledger_expire_days,
            14
        );
        for bad in ["0", "-1", "soon", "1.5"] {
            assert!(cfg(&with(&fake, &[("PMOS_LEDGER_EXPIRE_DAYS", bad)]))
                .unwrap_err()
                .contains("PMOS_LEDGER_EXPIRE_DAYS"));
        }
    }

    #[test]
    fn slack_drafts_expire_within_a_day() {
        let fake = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        assert_eq!(cfg(&fake).unwrap().slack_draft_ttl_hours, 24);
        assert_eq!(
            cfg(&with(&fake, &[("PMOS_SLACK_DRAFT_TTL_HOURS", "4")]))
                .unwrap()
                .slack_draft_ttl_hours,
            4
        );
        // Never longer than a day: no Slack text is kept past 24 hours.
        for bad in ["0", "25", "48", "soon", "1.5"] {
            assert!(cfg(&with(&fake, &[("PMOS_SLACK_DRAFT_TTL_HOURS", bad)]))
                .unwrap_err()
                .contains("PMOS_SLACK_DRAFT_TTL_HOURS"));
        }
    }

    #[test]
    fn audit_retention_is_90_days_or_longer() {
        let fake = [
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        assert_eq!(cfg(&fake).unwrap().audit_retention_days, 90);
        for (set, want) in [("90", 90), ("365", 365), (" 120 ", 120), ("3650", 3650)] {
            assert_eq!(
                cfg(&with(&fake, &[("PMOS_AUDIT_RETENTION_DAYS", set)]))
                    .unwrap()
                    .audit_retention_days,
                want
            );
        }
        // Below the floor: refused at start-up, with what to do.
        for short in ["0", "30", "89"] {
            let err = cfg(&with(&fake, &[("PMOS_AUDIT_RETENTION_DAYS", short)])).unwrap_err();
            assert!(err.contains("shorter than the 90-day minimum"), "{err}");
        }
        for bad in ["-1", "ninety", "90.5", "3651"] {
            let err = cfg(&with(&fake, &[("PMOS_AUDIT_RETENTION_DAYS", bad)])).unwrap_err();
            assert!(err.contains("from 90 to 3650"), "{err}");
        }
    }
}
