//! Configuration from environment variables. Tokens never appear in `Debug` output, so a
//! logged config can't leak one.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::assertion::Assertions;

/// The runtime's callers. The name in `PMOS_BOTS_TOKENS` decides what each may do (spec §3,
/// Auth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKind {
    /// The Slack app: acts for the PM in `X-Pmos-User`.
    Slack,
    /// The web app's backend: the same per-PM abilities as Slack, but no notification feed.
    Web,
    /// Harness workers, one per bot sandbox.
    Worker,
    /// The web console: reads everything, pauses routines, cancels tasks.
    Console,
    /// The benchmark runner: eval tasks only.
    Evals,
}

impl ClientKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "slack" => Some(Self::Slack),
            "web" => Some(Self::Web),
            "worker" => Some(Self::Worker),
            "console" => Some(Self::Console),
            "evals" => Some(Self::Evals),
            _ => None,
        }
    }

    /// Clients that act for the one PM named in `X-Pmos-User`.
    pub fn is_pm(self) -> bool {
        matches!(self, Self::Slack | Self::Web)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Web => "web",
            Self::Worker => "worker",
            Self::Console => "console",
            Self::Evals => "evals",
        }
    }
}

/// One caller and its token.
#[derive(Clone)]
pub struct Client {
    pub kind: ClientKind,
    /// For a worker named `worker-<bot>`: the one bot it serves. Each bot's sandbox holds its
    /// own worker token, so a compromised sandbox can't claim another bot's tasks (and their
    /// tool-server sessions). A plain `worker` serves every bot: development only.
    pub bot: Option<String>,
    pub token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("kind", &self.kind)
            .field("bot", &self.bot)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A token this service presents to another (the tool server, the gateway).
#[derive(Clone)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `PMOS_BOTS_TOKENS`: `name=token,…` with names `slack`, `web`, `worker`, `console`, `evals`.
    pub clients: Vec<Client>,
    /// `PMOS_BOTS_DATABASE_URL`: required unless `PMOS_DEV=1` (in memory).
    pub database_url: Option<Secret>,
    /// `PMOS_BOTS_BIND`, default `127.0.0.1:8750`.
    pub bind: SocketAddr,
    /// `PMOS_CONTENT_ROOT`, default `content`: bots, roles, skills, tools and `team.yaml`.
    pub content_root: PathBuf,
    /// `PMOS_TOOLSERVER_URL`, such as `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_SERVICE_TOKEN`: the tool server's orchestrator token.
    pub service_token: Secret,
    /// `PMOS_GATEWAY_URL`: the gateway's OpenAI base, such as `http://127.0.0.1:8740/v1`.
    pub gateway_url: String,
    /// `PMOS_GATEWAY_TOKEN`: this service's gateway client token.
    pub gateway_token: Secret,
    /// `PMOS_ASSERTION_MODE` and `PMOS_ASSERTION_KEYS`: user assertions from the `web` and
    /// `console` clients (`crate::assertion`). Off unless set.
    pub assertions: Arc<Assertions>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let need = |k: &str| {
            get(k)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{k} is required"))
        };
        let clients = parse_clients(&need("PMOS_BOTS_TOKENS")?)?;
        let dev = get("PMOS_DEV").as_deref() == Some("1");
        let database_url = get("PMOS_BOTS_DATABASE_URL").filter(|u| !u.is_empty());
        if database_url.is_none() && !dev {
            return Err("PMOS_BOTS_DATABASE_URL is required (or PMOS_DEV=1 for memory)".into());
        }
        let url = |k: &str| -> Result<String, String> {
            let v = need(k)?;
            if !(v.starts_with("http://") || v.starts_with("https://")) {
                return Err(format!("{k} must be an http:// or https:// URL"));
            }
            Ok(v.trim_end_matches('/').to_string())
        };
        Ok(Self {
            clients,
            database_url: database_url.map(Secret),
            bind: get("PMOS_BOTS_BIND")
                .unwrap_or_else(|| "127.0.0.1:8750".into())
                .parse()
                .map_err(|e| format!("PMOS_BOTS_BIND: {e}"))?,
            content_root: get("PMOS_CONTENT_ROOT")
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "content".into())
                .into(),
            toolserver_url: url("PMOS_TOOLSERVER_URL")?,
            service_token: Secret(need("PMOS_SERVICE_TOKEN")?),
            gateway_url: url("PMOS_GATEWAY_URL")?,
            gateway_token: Secret(need("PMOS_GATEWAY_TOKEN")?),
            assertions: Arc::new(Assertions::from_lookup(&get)?),
        })
    }
}

/// `name=token,…`. Each name is a known client and appears once; tokens are at least 32
/// characters and distinct, so no token is ambiguous about who is calling.
pub fn parse_clients(spec: &str) -> Result<Vec<Client>, String> {
    let mut clients: Vec<Client> = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (name, token) = entry
            .split_once('=')
            .ok_or("PMOS_BOTS_TOKENS entries are `name=token`")?;
        let name = name.trim();
        let token = token.trim();
        let (kind, bot) = match name.strip_prefix("worker-") {
            Some(bot) if pmos_contract::bots::valid_name(bot) => {
                (ClientKind::Worker, Some(bot.to_string()))
            }
            _ => (
                ClientKind::parse(name).ok_or_else(|| {
                    format!(
                        "PMOS_BOTS_TOKENS: unknown client `{name}`; the clients are slack, web, worker-<bot>, worker, console and evals"
                    )
                })?,
                None,
            ),
        };
        if token.len() < 32 {
            return Err(format!(
                "PMOS_BOTS_TOKENS: the token for `{name}` must be at least 32 characters"
            ));
        }
        if clients
            .iter()
            .any(|c| (c.kind == kind && c.bot == bot) || c.token == token)
        {
            return Err(format!(
                "PMOS_BOTS_TOKENS: `{name}` repeats a client name or token"
            ));
        }
        clients.push(Client {
            kind,
            bot,
            token: token.into(),
        });
    }
    if clients.is_empty() {
        return Err("PMOS_BOTS_TOKENS lists no clients".into());
    }
    Ok(clients)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<_, _> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    fn base(tokens: &str) -> Vec<(&'static str, String)> {
        vec![
            ("PMOS_BOTS_TOKENS", tokens.to_string()),
            ("PMOS_TOOLSERVER_URL", "http://127.0.0.1:8710/".into()),
            (
                "PMOS_SERVICE_TOKEN",
                "service-token-0123456789abcdef0123".into(),
            ),
            ("PMOS_GATEWAY_URL", "http://127.0.0.1:8740/v1".into()),
            (
                "PMOS_GATEWAY_TOKEN",
                "gateway-token-0123456789abcdef0123".into(),
            ),
        ]
    }

    #[test]
    fn per_bot_workers_serve_one_bot() {
        let t = |c: char| c.to_string().repeat(32);
        let clients = parse_clients(&format!(
            "worker-docs={},worker-pm-ana={},worker={}",
            t('a'),
            t('b'),
            t('c')
        ))
        .unwrap();
        assert_eq!(clients[0].bot.as_deref(), Some("docs"));
        assert_eq!(clients[1].bot.as_deref(), Some("pm-ana"));
        assert_eq!(clients[2].bot, None);
        assert!(clients.iter().all(|c| c.kind == ClientKind::Worker));
        // The same bot twice, or a bad bot name, is refused.
        assert!(parse_clients(&format!("worker-docs={},worker-docs={}", t('a'), t('b'))).is_err());
        assert!(parse_clients(&format!("worker-Docs={}", t('a'))).is_err());
    }

    #[test]
    fn defaults_and_required() {
        let t = "a".repeat(32);
        let tokens = format!("slack={t}");
        let owned = base(&tokens);
        let mut pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let err = cfg(&pairs).unwrap_err();
        assert!(err.contains("PMOS_BOTS_DATABASE_URL"), "{err}");
        pairs.push(("PMOS_DEV", "1"));
        let ok = cfg(&pairs).unwrap();
        assert_eq!(ok.bind.port(), 8750);
        assert_eq!(ok.toolserver_url, "http://127.0.0.1:8710");
        assert_eq!(ok.content_root, PathBuf::from("content"));
        assert!(ok.database_url.is_none());
        let without_gateway: Vec<_> = pairs
            .iter()
            .copied()
            .filter(|(k, _)| *k != "PMOS_GATEWAY_URL")
            .collect();
        assert!(cfg(&without_gateway)
            .unwrap_err()
            .contains("PMOS_GATEWAY_URL"));
    }

    #[test]
    fn secrets_never_print() {
        let t = "s".repeat(40);
        let tokens = format!("worker={t}");
        let owned = base(&tokens);
        let mut pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        pairs.push((
            "PMOS_BOTS_DATABASE_URL",
            "postgresql://pmos:hunter2@db/pmos",
        ));
        let c = cfg(&pairs).unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains(&t));
        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("service-token"));
        assert!(!shown.contains("gateway-token"));
    }

    #[test]
    fn clients_are_known_long_and_distinct() {
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let ok = parse_clients(&format!("slack={a}, worker={b}")).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[1].kind, ClientKind::Worker);
        assert!(parse_clients("slack=short").unwrap_err().contains("32"));
        assert!(parse_clients(&format!("slack={a},worker={a}")).is_err());
        assert!(parse_clients(&format!("slack={a},slack={b}")).is_err());
        assert!(parse_clients(&format!("admin={a}"))
            .unwrap_err()
            .contains("unknown client"));
        assert!(parse_clients("").is_err());
        assert!(parse_clients(&format!("slack{a}")).is_err());
    }
}
