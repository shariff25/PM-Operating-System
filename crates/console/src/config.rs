//! Configuration from environment variables.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use pmos_assertion::{KeyRing, KeyUse};

use crate::upstream::{Credentials, Signer};

/// The console's client name: the issuer of its user assertions.
pub const ISSUER: &str = "console";

/// Who the person in front of the browser is.
#[derive(Clone, PartialEq, Eq)]
pub enum Identity {
    /// Behind the host's SSO proxy: `X-Forwarded-User` names the person
    /// (`PMOS_CONSOLE_TRUST_PROXY=1`). Every request must also carry `X-Pmos-Proxy-Secret`
    /// equal to `secret` (`PMOS_CONSOLE_PROXY_SECRET`), which only the proxy holds, so a
    /// caller that reaches the console's port some other way can't name a user.
    Proxy { secret: String },
    /// Development: every request is this handle (`PMOS_CONSOLE_DEV_USER`).
    Dev(String),
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Proxy { .. } => f
                .debug_struct("Proxy")
                .field("secret", &"<redacted>")
                .finish(),
            Self::Dev(user) => f.debug_tuple("Dev").field(user).finish(),
        }
    }
}

/// The shortest `PMOS_CONSOLE_PROXY_SECRET` the console accepts.
pub const MIN_PROXY_SECRET: usize = 32;

#[derive(Clone)]
pub struct Config {
    /// `PMOS_CONSOLE_BIND`, default `127.0.0.1:8760`.
    pub bind: SocketAddr,
    pub identity: Identity,
    /// `PMOS_BOTS_URL`, default `http://127.0.0.1:8750`.
    pub bots_url: String,
    /// `PMOS_BOTS_TOKEN`: the runtime's `console` client token. Required, at least 32
    /// characters. With a `console:bots` key in `PMOS_ASSERTION_KEYS`, calls that name a
    /// person also carry a signed user assertion.
    pub bots_token: Credentials,
    /// `PMOS_TOOLSERVER_URL`, default `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_TOOLSERVER_TOKEN`: the tool server's read-only `console` client token
    /// (`PMOS_CONSOLE_TS_TOKEN` there), which may only read approvals and look users up
    /// (`resolve`). Required, at least 32 characters. With a `console:toolserver` key in
    /// `PMOS_ASSERTION_KEYS`, calls that name a person also carry a signed user assertion.
    pub toolserver_token: Credentials,
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
            .field("bots_token", &self.bots_token)
            .field("toolserver_url", &self.toolserver_url)
            .field("toolserver_token", &self.toolserver_token)
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
            (true, None) => {
                let secret = set("PMOS_CONSOLE_PROXY_SECRET").ok_or(
                    "PMOS_CONSOLE_TRUST_PROXY=1 needs PMOS_CONSOLE_PROXY_SECRET: the shared \
                     secret (at least 32 characters) the sign-in proxy sends in \
                     X-Pmos-Proxy-Secret on every request. Run deploy/setup.sh to generate it, \
                     and set the proxy to send it (docs/console.md, Sign-in)",
                )?;
                if secret.len() < MIN_PROXY_SECRET {
                    return Err(format!(
                        "PMOS_CONSOLE_PROXY_SECRET must be at least {MIN_PROXY_SECRET} characters"
                    ));
                }
                Identity::Proxy { secret }
            }
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
        if let Identity::Proxy { secret } = &identity {
            if *secret == bots_token || *secret == toolserver_token {
                return Err(
                    "PMOS_CONSOLE_PROXY_SECRET must differ from PMOS_BOTS_TOKEN and \
                     PMOS_TOOLSERVER_TOKEN"
                        .into(),
                );
            }
        }
        let mut keys = match set("PMOS_ASSERTION_KEYS") {
            None => BTreeMap::new(),
            Some(spec) => parse_assertion_keys(&spec, "the console", |iss, aud| {
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
        let bind: SocketAddr = set("PMOS_CONSOLE_BIND")
            .unwrap_or_else(|| "127.0.0.1:8760".into())
            .parse()
            .map_err(|e| format!("PMOS_CONSOLE_BIND: {e}"))?;
        if matches!(identity, Identity::Dev(_)) && !bind.ip().is_loopback() {
            return Err(format!(
                "PMOS_CONSOLE_DEV_USER makes every request that person, so it needs a loopback \
                 PMOS_CONSOLE_BIND (such as 127.0.0.1:8760), not {bind}"
            ));
        }
        Ok(Self {
            bind,
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
    const SECRET: &str = "proxy-secret-0123456789abcdef01234";

    #[test]
    fn assertion_keys_attach_a_signer_per_upstream() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let base = [
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_DEV_USER", "ana"),
        ];
        let with = |keys: &str| {
            let mut pairs = base.to_vec();
            pairs.push(("PMOS_ASSERTION_KEYS", keys));
            Config::from_lookup(lookup(&pairs))
        };
        let cfg = with(&format!(
            "console:bots:c1:{key},console:toolserver:c2:{key}"
        ))
        .unwrap();
        assert!(cfg.bots_token.signs() && cfg.toolserver_token.signs());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(key) && !debug.contains(BOTS) && !debug.contains(TOOLS));
        let plain = Config::from_lookup(lookup(&base)).unwrap();
        assert!(!plain.bots_token.signs() && !plain.toolserver_token.signs());
        // The web app's pair is not the console's to hold; a bad key never shows.
        for (spec, want) in [
            (format!("web:bots:k1:{key}"), "not a pair"),
            ("console:bots:k1:not base64!".to_string(), "base64"),
        ] {
            let err = with(&spec).unwrap_err();
            assert!(err.contains(want) && !err.contains(key), "{err}");
        }
    }

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
        let err = Config::from_lookup(lookup(&proxy)).unwrap_err();
        assert!(
            err.contains("PMOS_CONSOLE_PROXY_SECRET"),
            "trusting the proxy needs its secret: {err}"
        );
        let mut short = proxy.clone();
        short.push(("PMOS_CONSOLE_PROXY_SECRET", "too-short"));
        assert!(Config::from_lookup(lookup(&short))
            .unwrap_err()
            .contains("at least 32"));
        proxy.push(("PMOS_CONSOLE_PROXY_SECRET", SECRET));
        let cfg = Config::from_lookup(lookup(&proxy)).unwrap();
        assert_eq!(
            cfg.identity,
            Identity::Proxy {
                secret: SECRET.into()
            }
        );
        assert!(
            !format!("{cfg:?}").contains(SECRET),
            "the proxy secret never prints"
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

    #[test]
    fn proxy_trust_needs_a_strong_secret() {
        let base = [
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_TRUST_PROXY", "1"),
        ];
        // No secret at all, or a blank one: refuse to start, naming the variable.
        let err = Config::from_lookup(lookup(&base)).unwrap_err();
        assert!(err.contains("PMOS_CONSOLE_PROXY_SECRET"), "{err}");
        let mut blank = base.to_vec();
        blank.push(("PMOS_CONSOLE_PROXY_SECRET", "  "));
        let err = Config::from_lookup(lookup(&blank)).unwrap_err();
        assert!(err.contains("PMOS_CONSOLE_PROXY_SECRET"), "{err}");
        // Too short.
        let mut short = base.to_vec();
        short.push((
            "PMOS_CONSOLE_PROXY_SECRET",
            "0123456789abcdef0123456789abcde",
        ));
        let err = Config::from_lookup(lookup(&short)).unwrap_err();
        assert!(err.contains("at least 32"), "{err}");
        // Reusing a token.
        let mut reused = base.to_vec();
        reused.push(("PMOS_CONSOLE_PROXY_SECRET", TOOLS));
        let err = Config::from_lookup(lookup(&reused)).unwrap_err();
        assert!(err.contains("must differ"), "{err}");
        // A good one; the Debug output never shows it.
        let mut good = base.to_vec();
        good.push(("PMOS_CONSOLE_PROXY_SECRET", SECRET));
        let cfg = Config::from_lookup(lookup(&good)).unwrap();
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(SECRET), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn the_proxy_secret_is_compared_whole() {
        use axum::http::{HeaderMap, HeaderValue};
        let identity = Identity::Proxy {
            secret: SECRET.into(),
        };
        let with = |values: &[&str]| {
            let mut h = HeaderMap::new();
            for v in values {
                h.append(crate::auth::PROXY_SECRET, HeaderValue::from_str(v).unwrap());
            }
            crate::auth::from_proxy(&identity, &h)
        };
        assert!(with(&[SECRET]));
        assert!(!with(&[]));
        assert!(!with(&[""]));
        assert!(!with(&[&SECRET[..SECRET.len() - 1]]));
        assert!(!with(&[&format!("{SECRET}x")]));
        assert!(!with(&[&SECRET.to_uppercase()]));
        // Exactly one: a client's own copy next to the proxy's is refused, whichever comes
        // first.
        assert!(!with(&[SECRET, SECRET]));
        assert!(!with(&["guess", SECRET]));
    }

    #[test]
    fn the_dev_user_needs_a_loopback_bind() {
        let base = [
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_DEV_USER", "ana"),
        ];
        for bind in ["0.0.0.0:8760", "10.0.0.2:8760", "[::]:8760"] {
            let mut pairs = base.to_vec();
            pairs.push(("PMOS_CONSOLE_BIND", bind));
            let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(err.contains("loopback"), "{bind}: {err}");
        }
        for bind in ["127.0.0.1:8760", "[::1]:8760"] {
            let mut pairs = base.to_vec();
            pairs.push(("PMOS_CONSOLE_BIND", bind));
            assert!(Config::from_lookup(lookup(&pairs)).is_ok(), "{bind}");
        }
        // Behind the proxy, any bind is allowed: the secret guards it.
        let pairs = [
            ("PMOS_BOTS_TOKEN", BOTS),
            ("PMOS_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_CONSOLE_TRUST_PROXY", "1"),
            ("PMOS_CONSOLE_PROXY_SECRET", SECRET),
            ("PMOS_CONSOLE_BIND", "0.0.0.0:8760"),
        ];
        assert!(Config::from_lookup(lookup(&pairs)).is_ok());
    }
}
