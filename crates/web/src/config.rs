//! Configuration from environment variables, checked at start-up.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use chrono::Duration;
use pmos_assertion::{KeyRing, KeyUse};

use crate::entra::EntraConfig;
use crate::upstream::{Credentials, Signer};

/// pmos-web's client name: the issuer of its user assertions.
pub const ISSUER: &str = "web";

#[derive(Clone)]
pub struct Config {
    /// `PMOS_WEB_BIND`, default `127.0.0.1:8770`.
    pub bind: SocketAddr,
    /// `PMOS_BOTS_URL`, default `http://127.0.0.1:8750`.
    pub bots_url: String,
    /// `PMOS_WEB_BOTS_TOKEN`: the runtime's `web` client token (an entry of its
    /// `PMOS_BOTS_TOKENS`). Required, at least 32 characters. With a `web:bots` key in
    /// `PMOS_ASSERTION_KEYS`, calls for a PM also carry a signed user assertion.
    pub bots_token: Credentials,
    /// `PMOS_TOOLSERVER_URL`, default `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_WEB_TOOLSERVER_TOKEN`: pmos-web's own tool server token (`PMOS_WEB_TOKEN`
    /// there), which reaches only the approvals routes and the read-only admin routes
    /// (`GET /internal/admin/*`). Older deployments put the service
    /// token here. Required, at least 32 characters, and different from the runtime token.
    /// With a `web:toolserver` key in `PMOS_ASSERTION_KEYS`, calls for a PM also carry a
    /// signed user assertion.
    pub toolserver_token: Credentials,
    /// `PMOS_CONTENT_ROOT`, default `content`: the deployed content tree the admin screens
    /// read.
    pub content_root: PathBuf,
    /// `PMOS_WEB_STATIC_DIR`: the built web app (`web/dist`). Optional; without it only
    /// `/api` and `/healthz` answer.
    pub static_dir: Option<PathBuf>,
    /// `PMOS_WEB_PUBLIC_URL`: the origin browsers use, such as `https://pmos.example.com`.
    /// Writes must come from it. Without it, `Origin` must match the request's `Host`.
    pub public_url: Option<String>,
    /// `PMOS_WEB_DEV_LOGIN=1`: development sign-in (`POST /api/auth/dev-login`), where
    /// anyone who can reach the port signs in as any member.
    pub dev_login: bool,
    /// Entra ID sign-in (`PMOS_WEB_ENTRA_*`, with `PMOS_WEB_PUBLIC_URL`): on when fully
    /// configured. The server starts with Entra, development sign-in, or both.
    pub entra: Option<EntraConfig>,
    /// `PMOS_WEB_PASSWORD_LOGIN=1`: email-and-password sign-in with a required
    /// authenticator code (`docs/password-sign-in.md`), the tool server deciding. Off by
    /// default; needs signed user assertions to the tool server (a `web:toolserver` key, and
    /// `PMOS_ASSERTION_MODE` `require` or unset).
    pub password_login: bool,
    /// `PMOS_WEB_TRUSTED_PROXIES`: the addresses of the reverse proxies in front of pmos-web,
    /// comma-separated. A request from one of them is counted, for password sign-in's
    /// per-address limit, by the right-most `X-Forwarded-For` address that isn't one of them;
    /// any other request by its own peer address. Empty: every request by its peer.
    pub trusted_proxies: Vec<IpAddr>,
    /// `PMOS_WEB_SESSION_IDLE_MINUTES`, default 480 (8 hours).
    pub session_idle: Duration,
    /// `PMOS_WEB_SESSION_MAX_MINUTES`, default 1440 (24 hours), from sign-in.
    pub session_max: Duration,
    /// `PMOS_WEB_INTERNAL_BIND`: a second listener for the server-to-server route
    /// (`PUT /internal/brief/{handle}`), which the browser-facing `bind` then doesn't serve.
    /// Without it, that route is on `bind`, refusing anything that looks like a browser.
    pub internal_bind: Option<SocketAddr>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("bots_url", &self.bots_url)
            .field("bots_token", &self.bots_token)
            .field("toolserver_url", &self.toolserver_url)
            .field("toolserver_token", &self.toolserver_token)
            .field("content_root", &self.content_root)
            .field("static_dir", &self.static_dir)
            .field("public_url", &self.public_url)
            .field("dev_login", &self.dev_login)
            .field("entra", &self.entra)
            .field("password_login", &self.password_login)
            .field("trusted_proxies", &self.trusted_proxies)
            .field("session_idle", &self.session_idle)
            .field("session_max", &self.session_max)
            .field("internal_bind", &self.internal_bind)
            .finish()
    }
}

/// Why development sign-in won't start beside Entra ID without the override.
pub const DEV_WITH_ENTRA: &str = "PMOS_WEB_DEV_LOGIN=1 with Entra ID sign-in configured: \
     development sign-in bypasses Entra entirely. Unset PMOS_WEB_DEV_LOGIN, or set \
     PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1 on a development machine that nobody else can reach";

/// Why development sign-in won't start on a reachable address.
pub const DEV_NOT_LOOPBACK: &str = "PMOS_WEB_DEV_LOGIN=1 needs PMOS_WEB_BIND on a loopback \
     address (127.0.0.1 or ::1): with development sign-in anyone who reaches the port signs \
     in as any member";

/// Development sign-in only on a loopback address, the listener's and the internal one's.
pub fn check_dev_bind(dev_login: bool, bind: SocketAddr) -> Result<(), String> {
    if dev_login && !bind.ip().is_loopback() {
        return Err(DEV_NOT_LOOPBACK.into());
    }
    Ok(())
}

/// Why the server won't start without a provider.
pub const NO_PROVIDER: &str = "no sign-in provider: set PMOS_WEB_ENTRA_TENANT_ID, \
     PMOS_WEB_ENTRA_CLIENT_ID, PMOS_WEB_ENTRA_CLIENT_SECRET and PMOS_WEB_PUBLIC_URL for Entra \
     ID sign-in, PMOS_WEB_PASSWORD_LOGIN=1 for password sign-in, or PMOS_WEB_DEV_LOGIN=1 for \
     development sign-in (never expose it)";

/// Why password sign-in won't start without signed user assertions.
pub const PASSWORD_NEEDS_REQUIRE: &str = "PMOS_WEB_PASSWORD_LOGIN=1 needs signed user \
     assertions to the tool server in `require` mode: a web:toolserver key in \
     PMOS_ASSERTION_KEYS, and PMOS_ASSERTION_MODE unset or `require` \
     (docs/password-sign-in.md)";

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
        let password_login = match get("PMOS_WEB_PASSWORD_LOGIN").as_deref().map(str::trim) {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_WEB_PASSWORD_LOGIN must be `1` or `0`, not `{other}`"
                ))
            }
        };
        let public_url = set("PMOS_WEB_PUBLIC_URL")
            .map(|u| origin("PMOS_WEB_PUBLIC_URL", u))
            .transpose()?;
        let entra = EntraConfig::from_lookup(set, public_url.as_deref())?;
        if !dev_login && entra.is_none() && !password_login {
            return Err(NO_PROVIDER.into());
        }
        let trusted_proxies = match set("PMOS_WEB_TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse::<IpAddr>()
                        .map_err(|_| format!("PMOS_WEB_TRUSTED_PROXIES: `{s}` isn't an IP address"))
                })
                .collect::<Result<_, _>>()?,
        };
        let dev_with_entra = match get("PMOS_WEB_DEV_LOGIN_WITH_ENTRA")
            .as_deref()
            .map(str::trim)
        {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_WEB_DEV_LOGIN_WITH_ENTRA must be `1` or `0`, not `{other}`"
                ))
            }
        };
        if dev_login && entra.is_some() && !dev_with_entra {
            return Err(DEV_WITH_ENTRA.into());
        }
        let bots_token = token("PMOS_WEB_BOTS_TOKEN")?;
        let toolserver_token = token("PMOS_WEB_TOOLSERVER_TOKEN")?;
        if bots_token == toolserver_token {
            return Err("PMOS_WEB_BOTS_TOKEN and PMOS_WEB_TOOLSERVER_TOKEN must differ".into());
        }
        let content_root: PathBuf = set("PMOS_CONTENT_ROOT")
            .unwrap_or_else(|| "content".into())
            .into();
        let mut keys = match set("PMOS_ASSERTION_KEYS") {
            None => BTreeMap::new(),
            Some(spec) => parse_assertion_keys(&spec, "pmos-web", |iss, aud| {
                iss == ISSUER && (aud == "bots" || aud == "toolserver")
            })?,
        };
        let mut credentials = |token: String, audience: &str| {
            let c = Credentials::new(token);
            match keys.remove(&(ISSUER.to_string(), audience.to_string())) {
                Some(ring) => c.with_signer(Signer::new(ISSUER, audience, ring)),
                None => c,
            }
        };
        let bots_token = credentials(bots_token, "bots");
        let toolserver_token = credentials(toolserver_token, "toolserver");
        if password_login {
            let mode_ok = matches!(
                set("PMOS_ASSERTION_MODE").as_deref().map(str::trim),
                None | Some("require")
            );
            if !toolserver_token.signs() || !mode_ok {
                return Err(PASSWORD_NEEDS_REQUIRE.into());
            }
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
        let bind: SocketAddr = set("PMOS_WEB_BIND")
            .unwrap_or_else(|| "127.0.0.1:8770".into())
            .parse()
            .map_err(|e| format!("PMOS_WEB_BIND: {e}"))?;
        check_dev_bind(dev_login, bind)?;
        let internal_bind: Option<SocketAddr> = set("PMOS_WEB_INTERNAL_BIND")
            .map(|v| v.parse())
            .transpose()
            .map_err(|e| format!("PMOS_WEB_INTERNAL_BIND: {e}"))?;
        if internal_bind.is_some_and(|i| i == bind) {
            return Err("PMOS_WEB_INTERNAL_BIND must differ from PMOS_WEB_BIND".into());
        }
        Ok(Self {
            bind,
            internal_bind,
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
            content_root,
            static_dir: set("PMOS_WEB_STATIC_DIR").map(PathBuf::from),
            public_url,
            dev_login,
            entra,
            password_login,
            trusted_proxies,
            session_idle,
            session_max,
        })
    }
}

/// `PMOS_ASSERTION_KEYS`: the keys this service signs user assertions with
/// (`docs/user-assertion.md`; `pmos_assertion::parse_key_list`, shared by every service that
/// signs or checks). Only pairs `wanted` accepts may appear, so a service never holds a key it
/// has no use for. Errors name the entry and the pair, never a key.
pub fn parse_assertion_keys(
    spec: &str,
    service: &str,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<(String, String), KeyRing>, String> {
    pmos_assertion::parse_key_list(spec, service, KeyUse::Signs, wanted)
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
    fn starts_with_entra_dev_or_both_but_never_half_configured() {
        const SECRET: &str = "entra-client-secret-value";
        let entra = vec![
            (
                "PMOS_WEB_ENTRA_TENANT_ID",
                "11111111-2222-3333-4444-555555555555",
            ),
            (
                "PMOS_WEB_ENTRA_CLIENT_ID",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            ),
            ("PMOS_WEB_ENTRA_CLIENT_SECRET", SECRET),
            ("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com"),
        ];
        let mut pairs = base();
        pairs.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        let only_entra =
            Config::from_lookup(lookup(&[pairs.clone(), entra.clone()].concat())).unwrap();
        assert!(only_entra.entra.is_some() && !only_entra.dev_login);
        assert!(!format!("{only_entra:?}").contains(SECRET));
        // Both only with the explicit development override.
        let err = Config::from_lookup(lookup(&[base(), entra.clone()].concat())).unwrap_err();
        assert!(err.contains("PMOS_WEB_DEV_LOGIN_WITH_ENTRA"), "{err}");
        assert!(!err.contains(SECRET));
        let both = Config::from_lookup(lookup(
            &[
                base(),
                entra.clone(),
                vec![("PMOS_WEB_DEV_LOGIN_WITH_ENTRA", "1")],
            ]
            .concat(),
        ))
        .unwrap();
        assert!(both.entra.is_some() && both.dev_login);
        // Half-configured refuses even with development sign-in on.
        for drop in 0..entra.len() {
            let mut half = entra.clone();
            half.remove(drop);
            let err = Config::from_lookup(lookup(&[base(), half].concat())).unwrap_err();
            assert!(
                err.contains("half-configured") || err.contains("PMOS_WEB_PUBLIC_URL is required"),
                "{err}"
            );
            assert!(!err.contains(SECRET), "{err}");
        }
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
    fn assertion_keys_attach_a_signer_per_upstream() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:bots:k1:{key}");
        let cfg = Config::from_lookup(lookup(
            &[base(), vec![("PMOS_ASSERTION_KEYS", keys.as_str())]].concat(),
        ))
        .unwrap();
        assert!(cfg.bots_token.signs() && !cfg.toolserver_token.signs());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(key) && !debug.contains(BOTS), "{debug}");
        assert!(debug.contains("k1"), "{debug}");
        // Without keys nothing is signed.
        let plain = Config::from_lookup(lookup(&base())).unwrap();
        assert!(!plain.bots_token.signs() && !plain.toolserver_token.signs());
        // Another service's pair, or a short key, refuses to start without showing the key.
        let short = "AQEBAQEBAQEBAQEBAQEBAQ==";
        for (spec, want) in [
            (format!("console:bots:k1:{key}"), "not a pair"),
            (format!("web:bots:k1:{short}"), "32 to 64 bytes"),
            (format!("web:bots:{key}"), "expected issuer:audience"),
        ] {
            let err = Config::from_lookup(lookup(
                &[base(), vec![("PMOS_ASSERTION_KEYS", spec.as_str())]].concat(),
            ))
            .unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(!err.contains(key) && !err.contains(short), "{err}");
        }
    }

    #[test]
    fn development_sign_in_only_on_a_loopback_bind() {
        for bind in ["0.0.0.0:8770", "10.0.0.2:8770", "[::]:8770"] {
            let err =
                Config::from_lookup(lookup(&[base(), vec![("PMOS_WEB_BIND", bind)]].concat()))
                    .unwrap_err();
            assert!(err.contains("loopback"), "{bind}: {err}");
        }
        for bind in ["127.0.0.1:8770", "[::1]:8770"] {
            assert!(
                Config::from_lookup(lookup(&[base(), vec![("PMOS_WEB_BIND", bind)]].concat()))
                    .is_ok()
            );
        }
        // The internal listener: its own address.
        let cfg = Config::from_lookup(lookup(
            &[base(), vec![("PMOS_WEB_INTERNAL_BIND", "127.0.0.1:8771")]].concat(),
        ))
        .unwrap();
        assert_eq!(cfg.internal_bind.unwrap().to_string(), "127.0.0.1:8771");
        assert!(Config::from_lookup(lookup(
            &[base(), vec![("PMOS_WEB_INTERNAL_BIND", "127.0.0.1:8770")]].concat()
        ))
        .unwrap_err()
        .contains("differ"));
    }

    #[test]
    fn password_sign_in_needs_signed_assertions_in_require() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:toolserver:k1:{key}");
        let mut alone = base();
        alone.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        alone.push(("PMOS_WEB_PASSWORD_LOGIN", "1"));
        // Without a key for the tool server: refused.
        let err = Config::from_lookup(lookup(&alone)).unwrap_err();
        assert!(err.contains("PMOS_WEB_PASSWORD_LOGIN"), "{err}");
        // With one: password sign-in alone is a provider.
        let with_key = [alone.clone(), vec![("PMOS_ASSERTION_KEYS", keys.as_str())]].concat();
        let cfg = Config::from_lookup(lookup(&with_key)).unwrap();
        assert!(cfg.password_login && !cfg.dev_login && cfg.entra.is_none());
        // A mode other than require refuses; require is fine.
        for mode in ["off", "verify"] {
            let pairs = [with_key.clone(), vec![("PMOS_ASSERTION_MODE", mode)]].concat();
            assert!(Config::from_lookup(lookup(&pairs))
                .unwrap_err()
                .contains("require"));
        }
        let pairs = [with_key.clone(), vec![("PMOS_ASSERTION_MODE", "require")]].concat();
        assert!(Config::from_lookup(lookup(&pairs)).is_ok());
        // Beside development sign-in, too; and off by default.
        let both = [
            base(),
            vec![
                ("PMOS_WEB_PASSWORD_LOGIN", "1"),
                ("PMOS_ASSERTION_KEYS", keys.as_str()),
            ],
        ]
        .concat();
        assert!(Config::from_lookup(lookup(&both)).unwrap().password_login);
        assert!(!Config::from_lookup(lookup(&base())).unwrap().password_login);
        let bad = [base(), vec![("PMOS_WEB_PASSWORD_LOGIN", "yes")]].concat();
        assert!(Config::from_lookup(lookup(&bad))
            .unwrap_err()
            .contains("`1` or `0`"));
        // Trusted proxies are addresses.
        let proxies = [base(), vec![("PMOS_WEB_TRUSTED_PROXIES", "10.0.0.1, ::1")]].concat();
        assert_eq!(
            Config::from_lookup(lookup(&proxies))
                .unwrap()
                .trusted_proxies
                .len(),
            2
        );
        let proxies = [base(), vec![("PMOS_WEB_TRUSTED_PROXIES", "proxy.local")]].concat();
        assert!(Config::from_lookup(lookup(&proxies)).is_err());
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
