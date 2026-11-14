//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Who the person in front of the browser is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// Behind the Spark's SSO proxy: `X-Forwarded-User` names the person
    /// (`PMOS_CONSOLE_TRUST_PROXY=1`). Only safe when nothing but the proxy can reach the
    /// console's port.
    Proxy,
    /// Development: every request is this handle (`PMOS_CONSOLE_DEV_USER`).
    Dev(String),
}

#[derive(Clone)]
pub struct Config {
    /// `PMOS_CONSOLE_BIND`, default `127.0.0.1:8760`.
    pub bind: SocketAddr,
    pub identity: Identity,
    /// `PMOS_BOTS_URL`, default `http://127.0.0.1:8750`.
    pub bots_url: String,
    /// `PMOS_BOTS_TOKEN`: the runtime's `console` client token. Required, at least 32
    /// characters.
    pub bots_token: String,
    /// `PMOS_TOOLSERVER_URL`, default `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_TOOLSERVER_TOKEN`: the tool server's service token (`PMOS_SERVICE_TOKEN`
    /// there), which the approvals list needs. Required, at least 32 characters.
    pub toolserver_token: String,
    /// `PMOS_CONTENT_ROOT`, default `content`: where `team.yaml` is.
    pub content_root: PathBuf,
    /// `PMOS_CONSOLE_AUDIT_LOG`: the tool server's JSON-lines audit file (its
    /// `PMOS_AUDIT_LOG`), mounted read-only. A task page shows its audit events from it.
    pub audit_log: Option<PathBuf>,
    /// `PMOS_CONSOLE_SLACK_URL`: the Slack workspace, such as `https://acme.slack.com`, for
    /// links to a task's thread, where its approval cards are.
    pub slack_url: Option<String>,
    /// `PMOS_CONSOLE_PUBLIC_URL`: the origin browsers use, such as
    /// `https://console.pmos.internal`. Forms must come from it. Without it, a form's
    /// `Origin` must match the request's `Host`.
    pub public_url: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("identity", &self.identity)
            .field("bots_url", &self.bots_url)
            .field("bots_token", &"<redacted>")
            .field("toolserver_url", &self.toolserver_url)
            .field("toolserver_token", &"<redacted>")
            .field("content_root", &self.content_root)
            .field("audit_log", &self.audit_log)
            .field("slack_url", &self.slack_url)
            .field("public_url", &self.public_url)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let token = |k: &str| -> Result<String, String> {
            let t = set(k).ok_or(format!("{k} is required"))?;
            if t.len() < 32 {
                return Err(format!("{k} must be at least 32 characters"));
            }
            Ok(t)
        };
        let trust_proxy = match get("PMOS_CONSOLE_TRUST_PROXY").as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_CONSOLE_TRUST_PROXY must be `1` or `0`, not `{other}`"
                ))
            }
        };
        let identity = match (trust_proxy, set("PMOS_CONSOLE_DEV_USER")) {
            (true, None) => Identity::Proxy,
            (false, Some(user)) => {
                if !pmos_contract::model::valid_handle(&user) {
                    return Err(format!(
                        "PMOS_CONSOLE_DEV_USER `{user}` is not a valid handle"
                    ));
                }
                Identity::Dev(user)
            }
            (true, Some(_)) => {
                return Err(
                    "set PMOS_CONSOLE_TRUST_PROXY=1 or PMOS_CONSOLE_DEV_USER, not both".into(),
                )
            }
            (false, None) => {
                return Err("set PMOS_CONSOLE_TRUST_PROXY=1 (behind the SSO proxy) or \
                     PMOS_CONSOLE_DEV_USER=<handle> (development)"
                    .into())
            }
        };
        let bots_token = token("PMOS_BOTS_TOKEN")?;
        let toolserver_token = token("PMOS_TOOLSERVER_TOKEN")?;
        if bots_token == toolserver_token {
            return Err("PMOS_BOTS_TOKEN and PMOS_TOOLSERVER_TOKEN must differ".into());
        }
        Ok(Self {
            bind: set("PMOS_CONSOLE_BIND")
                .unwrap_or_else(|| "127.0.0.1:8760".into())
                .parse()
                .map_err(|e| format!("PMOS_CONSOLE_BIND: {e}"))?,
            identity,
            bots_url: http_url(
                "PMOS_BOTS_URL",
                set("PMOS_BOTS_URL").unwrap_or_else(|| "http://127.0.0.1:8750".into()),
            )?,
            bots_token,
            toolserver_url: http_url(
                "PMOS_TOOLSERVER_URL",
                set("PMOS_TOOLSERVER_URL").unwrap_or_else(|| "http://127.0.0.1:8710".into()),
            )?,
            toolserver_token,
            content_root: set("PMOS_CONTENT_ROOT")
                .unwrap_or_else(|| "content".into())
                .into(),
            audit_log: set("PMOS_CONSOLE_AUDIT_LOG").map(PathBuf::from),
            slack_url: set("PMOS_CONSOLE_SLACK_URL")
                .map(|u| https_origin("PMOS_CONSOLE_SLACK_URL", u))
                .transpose()?,
            public_url: set("PMOS_CONSOLE_PUBLIC_URL")
                .map(|u| http_url("PMOS_CONSOLE_PUBLIC_URL", u))
                .transpose()?,
        })
    }
}

/// An `http` or `https` base URL, without a trailing slash.
fn http_url(key: &str, value: String) -> Result<String, String> {
    let url = reqwest::Url::parse(&value).map_err(|e| format!("{key}: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("{key} must be an http or https URL"));
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn https_origin(key: &str, value: String) -> Result<String, String> {
    let value = http_url(key, value)?;
    if !value.starts_with("https://") {
        return Err(format!("{key} must be an https URL"));
    }
    Ok(value)
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
        }
    }

    const BOTS: &str = "console-token-0123456789abcdef0123";
    const TOOLS: &str = "service-token-0123456789abcdef0123";

    #[test]
    fn reads_defaults_and_redacts_tokens() {
        let cfg = Config::from_lookup(lookup(&[
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_DEV_USER", "ana"),
        ]))
        .unwrap();
        assert_eq!(cfg.bind.to_string(), "127.0.0.1:8760");
        assert_eq!(cfg.identity, Identity::Dev("ana".into()));
        assert_eq!(cfg.bots_url, "http://127.0.0.1:8750");
        assert_eq!(cfg.toolserver_url, "http://127.0.0.1:8710");
        assert_eq!(cfg.content_root, PathBuf::from("content"));
        assert!(cfg.audit_log.is_none() && cfg.slack_url.is_none());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(BOTS) && !debug.contains(TOOLS));
    }

    #[test]
    fn needs_exactly_one_way_to_know_the_user() {
        let base = [("PMOS_BOTS_TOKEN", BOTS), ("PMOS_TOOLSERVER_TOKEN", TOOLS)];
        let err = Config::from_lookup(lookup(&base)).unwrap_err();
        assert!(err.contains("PMOS_CONSOLE_TRUST_PROXY"), "{err}");
        let mut both = base.to_vec();
        both.extend([
            ("PMOS_CONSOLE_TRUST_PROXY", "1"),
            ("PMOS_CONSOLE_DEV_USER", "ana"),
        ]);
        assert!(Config::from_lookup(lookup(&both))
            .unwrap_err()
            .contains("not both"));
        let mut proxy = base.to_vec();
        proxy.push(("PMOS_CONSOLE_TRUST_PROXY", "1"));
        assert_eq!(
            Config::from_lookup(lookup(&proxy)).unwrap().identity,
            Identity::Proxy
        );
        let mut yes = base.to_vec();
        yes.push(("PMOS_CONSOLE_TRUST_PROXY", "yes"));
        assert!(Config::from_lookup(lookup(&yes)).is_err());
        let mut bad = base.to_vec();
        bad.push(("PMOS_CONSOLE_DEV_USER", "Ana Smith"));
        assert!(Config::from_lookup(lookup(&bad)).is_err());
    }

    #[test]
    fn refuses_weak_tokens_and_bad_urls() {
        let dev = ("PMOS_CONSOLE_DEV_USER", "ana");
        for (pairs, want) in [
            (
                vec![dev, ("PMOS_TOOLSERVER_TOKEN", TOOLS)],
                "PMOS_BOTS_TOKEN is required",
            ),
            (
                vec![
                    dev,
                    ("PMOS_BOTS_TOKEN", "short"),
                    ("PMOS_TOOLSERVER_TOKEN", TOOLS),
                ],
                "at least 32",
            ),
            (
                vec![
                    dev,
                    ("PMOS_BOTS_TOKEN", BOTS),
                    ("PMOS_TOOLSERVER_TOKEN", BOTS),
                ],
                "must differ",
            ),
            (
                vec![
                    dev,
                    ("PMOS_BOTS_TOKEN", BOTS),
                    ("PMOS_TOOLSERVER_TOKEN", TOOLS),
                    ("PMOS_BOTS_URL", "ftp://bots"),
                ],
                "http or https",
            ),
            (
                vec![
                    dev,
                    ("PMOS_BOTS_TOKEN", BOTS),
                    ("PMOS_TOOLSERVER_TOKEN", TOOLS),
                    ("PMOS_CONSOLE_SLACK_URL", "http://acme.slack.com"),
                ],
                "https",
            ),
        ] {
            let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
    }

    #[test]
    fn trims_trailing_slashes() {
        let cfg = Config::from_lookup(lookup(&[
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_DEV_USER", "ana"),
            ("PMOS_BOTS_URL", "http://bots.pmos.internal:8750/"),
            ("PMOS_CONSOLE_SLACK_URL", "https://acme.slack.com/"),
        ]))
        .unwrap();
        assert_eq!(cfg.bots_url, "http://bots.pmos.internal:8750");
        assert_eq!(cfg.slack_url.as_deref(), Some("https://acme.slack.com"));
    }
}
