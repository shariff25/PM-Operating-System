//! Configuration from environment variables, checked at start-up.

use std::net::SocketAddr;
use std::path::PathBuf;

use chrono::Duration;

#[derive(Clone)]
pub struct Config {
    /// `PMOS_WEB_BIND`, default `127.0.0.1:8770`.
    pub bind: SocketAddr,
    /// `PMOS_BOTS_URL`, default `http://127.0.0.1:8750`.
    pub bots_url: String,
    /// `PMOS_WEB_BOTS_TOKEN`: the runtime's `web` client token (an entry of its
    /// `PMOS_BOTS_TOKENS`). Required, at least 32 characters.
    pub bots_token: String,
    /// `PMOS_TOOLSERVER_URL`, default `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_WEB_TOOLSERVER_TOKEN`: the tool server's service token (`PMOS_SERVICE_TOKEN`
    /// there). Listing a PM's decided items and deciding them need it. Required, at least
    /// 32 characters, and different from the runtime token.
    pub toolserver_token: String,
    /// `PMOS_CONTENT_ROOT`, default `content`: where `team.yaml` is.
    pub content_root: PathBuf,
    /// `PMOS_WEB_STATIC_DIR`: the built web app (`web/dist`). Optional; without it only
    /// `/api` and `/healthz` answer.
    pub static_dir: Option<PathBuf>,
    /// `PMOS_WEB_PUBLIC_URL`: the origin browsers use, such as `https://pmos.example.com`.
    /// Writes must come from it. Without it, `Origin` must match the request's `Host`.
    pub public_url: Option<String>,
    /// `PMOS_WEB_DEV_LOGIN=1`: development sign-in (`POST /api/auth/dev-login`), where
    /// anyone who can reach the port signs in as any member. The only provider today.
    pub dev_login: bool,
    /// `PMOS_WEB_SESSION_IDLE_MINUTES`, default 480 (8 hours).
    pub session_idle: Duration,
    /// `PMOS_WEB_SESSION_MAX_MINUTES`, default 1440 (24 hours), from sign-in.
    pub session_max: Duration,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("bots_url", &self.bots_url)
            .field("bots_token", &"<redacted>")
            .field("toolserver_url", &self.toolserver_url)
            .field("toolserver_token", &"<redacted>")
            .field("content_root", &self.content_root)
            .field("static_dir", &self.static_dir)
            .field("public_url", &self.public_url)
            .field("dev_login", &self.dev_login)
            .field("session_idle", &self.session_idle)
            .field("session_max", &self.session_max)
            .finish()
    }
}

/// Why the server won't start without a provider.
pub const NO_PROVIDER: &str = "no sign-in provider: Entra ID is not built yet, so pmos-web \
     starts only with PMOS_WEB_DEV_LOGIN=1 (development sign-in; never expose it)";

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
        let dev_login = match get("PMOS_WEB_DEV_LOGIN").as_deref().map(str::trim) {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_WEB_DEV_LOGIN must be `1` or `0`, not `{other}`"
                ))
            }
        };
        if !dev_login {
            return Err(NO_PROVIDER.into());
        }
        let bots_token = token("PMOS_WEB_BOTS_TOKEN")?;
        let toolserver_token = token("PMOS_WEB_TOOLSERVER_TOKEN")?;
        if bots_token == toolserver_token {
            return Err("PMOS_WEB_BOTS_TOKEN and PMOS_WEB_TOOLSERVER_TOKEN must differ".into());
        }
        let minutes = |k: &str, default: i64| -> Result<Duration, String> {
            match set(k) {
                None => Ok(Duration::minutes(default)),
                Some(v) => match v.trim().parse::<i64>() {
                    Ok(n) if (1..=60 * 24 * 30).contains(&n) => Ok(Duration::minutes(n)),
                    _ => Err(format!("{k} must be a whole number of minutes, 1 to 43200")),
                },
            }
        };
        let session_idle = minutes("PMOS_WEB_SESSION_IDLE_MINUTES", 8 * 60)?;
        let session_max = minutes("PMOS_WEB_SESSION_MAX_MINUTES", 24 * 60)?;
        if session_idle > session_max {
            return Err(
                "PMOS_WEB_SESSION_IDLE_MINUTES can't be longer than PMOS_WEB_SESSION_MAX_MINUTES"
                    .into(),
            );
        }
        Ok(Self {
            bind: set("PMOS_WEB_BIND")
                .unwrap_or_else(|| "127.0.0.1:8770".into())
                .parse()
                .map_err(|e| format!("PMOS_WEB_BIND: {e}"))?,
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
            static_dir: set("PMOS_WEB_STATIC_DIR").map(PathBuf::from),
            public_url: set("PMOS_WEB_PUBLIC_URL")
                .map(|u| origin("PMOS_WEB_PUBLIC_URL", u))
                .transpose()?,
            dev_login,
            session_idle,
            session_max,
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

/// An origin: scheme, host and port only.
fn origin(key: &str, value: String) -> Result<String, String> {
    let value = http_url(key, value)?;
    let url = reqwest::Url::parse(&value).map_err(|e| format!("{key}: {e}"))?;
    if url.path() != "/" || url.query().is_some() {
        return Err(format!(
            "{key} must be an origin such as https://pmos.example.com, with no path"
        ));
    }
    Ok(url.origin().ascii_serialization())
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

    const BOTS: &str = "web-client-token-0123456789abcdef0123";
    const TOOLS: &str = "service-token-0123456789abcdef012345";

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PMOS_WEB_DEV_LOGIN", "1"),
            ("PMOS_WEB_BOTS_TOKEN", BOTS),
            ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
        ]
    }

    #[test]
    fn reads_defaults_and_redacts_tokens() {
        let cfg = Config::from_lookup(lookup(&base())).unwrap();
        assert_eq!(cfg.bind.to_string(), "127.0.0.1:8770");
        assert_eq!(cfg.bots_url, "http://127.0.0.1:8750");
        assert_eq!(cfg.toolserver_url, "http://127.0.0.1:8710");
        assert_eq!(cfg.content_root, PathBuf::from("content"));
        assert_eq!(cfg.session_idle, Duration::hours(8));
        assert_eq!(cfg.session_max, Duration::hours(24));
        assert!(cfg.dev_login && cfg.static_dir.is_none() && cfg.public_url.is_none());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(BOTS) && !debug.contains(TOOLS));
    }

    #[test]
    fn refuses_to_start_without_a_provider() {
        let mut pairs = base();
        pairs.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
        assert!(err.contains("no sign-in provider"), "{err}");
        pairs.push(("PMOS_WEB_DEV_LOGIN", "0"));
        assert!(Config::from_lookup(lookup(&pairs)).is_err());
        let mut yes = base();
        yes[0] = ("PMOS_WEB_DEV_LOGIN", "yes");
        assert!(Config::from_lookup(lookup(&yes))
            .unwrap_err()
            .contains("`1` or `0`"));
    }

    #[test]
    fn refuses_weak_tokens_bad_urls_and_bad_durations() {
        let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
                ],
                "PMOS_WEB_BOTS_TOKEN is required",
            ),
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_BOTS_TOKEN", "short"),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
                ],
                "at least 32",
            ),
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_BOTS_TOKEN", BOTS),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", BOTS),
                ],
                "must differ",
            ),
            (
                [base(), vec![("PMOS_BOTS_URL", "ftp://bots")]].concat(),
                "http or https",
            ),
            (
                [
                    base(),
                    vec![("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com/app")],
                ]
                .concat(),
                "no path",
            ),
            (
                [base(), vec![("PMOS_WEB_SESSION_IDLE_MINUTES", "0")]].concat(),
                "whole number",
            ),
            (
                [
                    base(),
                    vec![
                        ("PMOS_WEB_SESSION_IDLE_MINUTES", "600"),
                        ("PMOS_WEB_SESSION_MAX_MINUTES", "60"),
                    ],
                ]
                .concat(),
                "can't be longer",
            ),
        ];
        for (pairs, want) in cases {
            let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
            assert!(!err.contains(BOTS) && !err.contains(TOOLS));
        }
    }

    #[test]
    fn normalizes_urls() {
        let cfg = Config::from_lookup(lookup(
            &[
                base(),
                vec![
                    ("PMOS_BOTS_URL", "http://bots.pmos.internal:8750/"),
                    ("PMOS_WEB_PUBLIC_URL", "https://PMOS.example.com/"),
                ],
            ]
            .concat(),
        ))
        .unwrap();
        assert_eq!(cfg.bots_url, "http://bots.pmos.internal:8750");
        assert_eq!(cfg.public_url.as_deref(), Some("https://pmos.example.com"));
    }
}
