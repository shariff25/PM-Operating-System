//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::oauth::{AppCredentials, Endpoints};
use crate::vault::{KeyRing, Secret};

/// Default Atlassian scopes: Jira classic scopes, Confluence granular scopes for the v2 page
/// API and CQL search. `offline_access` is always added for refresh tokens.
pub const DEFAULT_ATLASSIAN_SCOPES: &str = "read:me read:jira-work write:jira-work read:jira-user \
     search:confluence read:page:confluence write:page:confluence read:space:confluence";

/// Default Slack user scopes: Real-time Search, thread history, and posting as the PM.
pub const DEFAULT_SLACK_SCOPES: &str = "search:read.public search:read.private search:read.im \
     search:read.mpim channels:history groups:history im:history mpim:history chat:write";

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
    /// `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL`: overrides for
    /// tests against mock APIs. Default to the real hosts.
    pub endpoints: Endpoints,
    /// `PMOS_DOCS_URL`: the `pmos-docs` MCP endpoint, such as `http://docs.pmos.internal:8730/mcp`.
    pub docs_url: String,
    /// `PMOS_DOCS_TOKEN`: the bearer token `pmos-docs` expects.
    pub docs_token: Secret,
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
            bot_skills,
            database_url,
            audit_log,
            allowed_hosts,
            connectors,
        })
    }
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
        endpoints: Endpoints {
            atlassian_auth: get("PMOS_ATLASSIAN_AUTH_URL").unwrap_or(defaults.atlassian_auth),
            atlassian_api: get("PMOS_ATLASSIAN_API_URL").unwrap_or(defaults.atlassian_api),
            slack: get("PMOS_SLACK_URL").unwrap_or(defaults.slack),
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
    }
}
