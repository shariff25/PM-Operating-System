//! Configuration: which local model serves each role, and who may call the gateway.
//!
//! The model map lives in a YAML file (`config/gateway.yaml` by default), so swapping a model
//! is a reviewed one-line change plus a benchmark run (D11). Everything else is environment.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::Deserialize;

use crate::workarounds;

/// How many requests run at once, how many any one person may have running, and how many
/// may wait before the gateway answers "busy".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Limits {
    #[serde(default = "default_in_flight")]
    pub max_in_flight: usize,
    #[serde(default = "default_per_user")]
    pub per_user: usize,
    #[serde(default = "default_queue")]
    pub max_queue: usize,
}

fn default_in_flight() -> usize {
    4
}
fn default_per_user() -> usize {
    2
}
fn default_queue() -> usize {
    64
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_in_flight: default_in_flight(),
            per_user: default_per_user(),
            max_queue: default_queue(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RoleSpec {
    upstream: String,
    model: String,
    #[serde(default)]
    workarounds: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ModelsFile {
    upstreams: BTreeMap<String, String>,
    roles: BTreeMap<String, RoleSpec>,
    #[serde(default)]
    local_hosts: Vec<String>,
    #[serde(default)]
    limits: Limits,
}

/// One model role, such as `default`, which callers address as `pmos/default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    /// The model server's OpenAI-compatible base, such as `http://127.0.0.1:8001/v1`.
    pub upstream: String,
    /// The model name that server expects (vLLM's `--served-model-name`).
    pub model: String,
    /// Workaround ids switched on for this role (see `workarounds.rs`).
    pub workarounds: Vec<String>,
}

impl Role {
    pub fn alias(&self) -> String {
        format!("pmos/{}", self.name)
    }

    /// The upstream's host, for the usage log.
    pub fn upstream_host(&self) -> String {
        reqwest::Url::parse(&self.upstream)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct Models {
    pub roles: BTreeMap<String, Role>,
    pub limits: Limits,
}

impl Models {
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let file: ModelsFile =
            serde_norway::from_str(text).map_err(|e| format!("gateway config: {e}"))?;
        if file.roles.is_empty() {
            return Err("gateway config: no roles".into());
        }
        if !file.roles.contains_key("default") {
            return Err("gateway config: a `default` role is required".into());
        }
        let limits = file.limits;
        if limits.max_in_flight == 0 || limits.per_user == 0 {
            return Err("gateway config: limits must be at least 1".into());
        }
        let mut roles = BTreeMap::new();
        for (name, spec) in file.roles {
            if !valid_role_name(&name) {
                return Err(format!(
                    "gateway config: role `{name}` must be lowercase letters, digits and `-`"
                ));
            }
            let base = file.upstreams.get(&spec.upstream).ok_or_else(|| {
                format!(
                    "gateway config: role `{name}` names upstream `{}`, which isn't listed",
                    spec.upstream
                )
            })?;
            let url = reqwest::Url::parse(base)
                .map_err(|e| format!("gateway config: upstream `{}`: {e}", spec.upstream))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(format!(
                    "gateway config: upstream `{}` must be http or https",
                    spec.upstream
                ));
            }
            let host = url.host_str().unwrap_or_default();
            if !is_local_host(host, &file.local_hosts) {
                return Err(format!(
                    "gateway config: upstream `{}` ({host}) is not on this machine or its private network; \
                     PM OS sends prompts only to local models",
                    spec.upstream
                ));
            }
            if spec.model.trim().is_empty() {
                return Err(format!("gateway config: role `{name}` has no model"));
            }
            for w in &spec.workarounds {
                if !workarounds::known(w) {
                    return Err(format!(
                        "gateway config: role `{name}` enables unknown workaround `{w}`; known: {}",
                        workarounds::REGISTRY
                            .iter()
                            .map(|w| w.id)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            roles.insert(
                name.clone(),
                Role {
                    name,
                    upstream: base.trim_end_matches('/').to_string(),
                    model: spec.model,
                    workarounds: spec.workarounds,
                },
            );
        }
        Ok(Self { roles, limits })
    }

    /// The role a request's `model` names: `pmos/<role>`. A concrete model name is refused,
    /// so callers never depend on which model is behind a role.
    pub fn role(&self, alias: &str) -> Option<&Role> {
        alias
            .strip_prefix("pmos/")
            .and_then(|name| self.roles.get(name))
    }

    pub fn aliases(&self) -> Vec<String> {
        self.roles.values().map(Role::alias).collect()
    }
}

fn valid_role_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A host that can't be on the public internet: loopback or private addresses, a name with
/// no dot (a compose service such as `vllm`), `localhost`, `*.pmos.internal`, or one the
/// platform owner listed. Anything else is refused, so prompts never leave the Spark (ADM-3).
pub fn is_local_host(host: &str, extra: &[String]) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => {
                v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local
            }
        };
    }
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || !host.contains('.')
        || host.ends_with(".pmos.internal")
        || extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
}

/// One caller and its token, such as the bot runtime or the benchmark runner.
#[derive(Clone)]
pub struct Client {
    pub name: String,
    pub token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("name", &self.name)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `PMOS_GATEWAY_BIND`, default `127.0.0.1:8740`.
    pub bind: SocketAddr,
    /// `PMOS_GATEWAY_CONFIG`, default `config/gateway.yaml`.
    pub models_path: PathBuf,
    /// `PMOS_GATEWAY_TOKENS`: `name=token,name=token`, each token at least 32 characters.
    pub clients: Vec<Client>,
    /// `PMOS_GATEWAY_USAGE_LOG`: also append usage events to this JSON-lines file.
    pub usage_log: Option<PathBuf>,
    /// `PMOS_GATEWAY_STUB=1`: answer with a deterministic stub model, no model server needed
    /// (CI, local development).
    pub stub: bool,
    /// `PMOS_GATEWAY_WORKAROUNDS=off`: switch every workaround off, for the benchmark run
    /// that decides whether they can be deleted (D11).
    pub workarounds_on: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let clients =
            parse_clients(&get("PMOS_GATEWAY_TOKENS").ok_or("PMOS_GATEWAY_TOKENS is required")?)?;
        let bind = get("PMOS_GATEWAY_BIND")
            .unwrap_or_else(|| "127.0.0.1:8740".into())
            .parse()
            .map_err(|e| format!("PMOS_GATEWAY_BIND: {e}"))?;
        let workarounds_on = match get("PMOS_GATEWAY_WORKAROUNDS").as_deref() {
            None | Some("on") => true,
            Some("off") => false,
            Some(other) => {
                return Err(format!(
                    "PMOS_GATEWAY_WORKAROUNDS must be `on` or `off`, not `{other}`"
                ))
            }
        };
        Ok(Self {
            bind,
            models_path: get("PMOS_GATEWAY_CONFIG")
                .unwrap_or_else(|| "config/gateway.yaml".into())
                .into(),
            clients,
            usage_log: get("PMOS_GATEWAY_USAGE_LOG")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            stub: get("PMOS_GATEWAY_STUB").as_deref() == Some("1"),
            workarounds_on,
        })
    }
}

fn parse_clients(spec: &str) -> Result<Vec<Client>, String> {
    let mut clients: Vec<Client> = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (name, token) = entry
            .split_once('=')
            .ok_or("PMOS_GATEWAY_TOKENS entries are `name=token`")?;
        if !valid_role_name(name) {
            return Err(format!(
                "PMOS_GATEWAY_TOKENS: client name `{name}` must be lowercase letters, digits and `-`"
            ));
        }
        if token.len() < 32 {
            return Err(format!(
                "PMOS_GATEWAY_TOKENS: the token for `{name}` must be at least 32 characters"
            ));
        }
        if clients.iter().any(|c| c.name == name || c.token == token) {
            return Err(format!(
                "PMOS_GATEWAY_TOKENS: `{name}` repeats a client name or token"
            ));
        }
        clients.push(Client {
            name: name.into(),
            token: token.into(),
        });
    }
    if clients.is_empty() {
        return Err("PMOS_GATEWAY_TOKENS lists no clients".into());
    }
    Ok(clients)
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = "upstreams:\n  qwen: http://127.0.0.1:8001/v1/\n  nemo: http://vllm-nemotron:8000/v1\n\
roles:\n  default: {upstream: qwen, model: qwen3.6-35b-a3b, workarounds: [strip-think]}\n  extraction: {upstream: nemo, model: nemotron-3-nano}\n";

    #[test]
    fn reads_roles_and_resolves_aliases() {
        let m = Models::from_yaml(YAML).unwrap();
        let r = m.role("pmos/default").unwrap();
        assert_eq!(r.upstream, "http://127.0.0.1:8001/v1");
        assert_eq!(r.model, "qwen3.6-35b-a3b");
        assert_eq!(
            m.role("pmos/extraction").unwrap().upstream_host(),
            "vllm-nemotron"
        );
        assert!(
            m.role("qwen3.6-35b-a3b").is_none(),
            "concrete names are refused"
        );
        assert!(m.role("pmos/nope").is_none());
        assert_eq!(m.aliases(), vec!["pmos/default", "pmos/extraction"]);
        assert_eq!(m.limits, Limits::default());
    }

    #[test]
    fn refuses_anything_off_the_spark() {
        for host in [
            "https://api.openai.com/v1",
            "http://8.8.8.8/v1",
            "http://models.example.com/v1",
        ] {
            let yaml = format!(
                "upstreams: {{x: '{host}'}}\nroles: {{default: {{upstream: x, model: m}}}}\n"
            );
            let err = Models::from_yaml(&yaml).unwrap_err();
            assert!(err.contains("only to local models"), "{host}: {err}");
        }
        for host in [
            "http://localhost:8000/v1",
            "http://10.0.0.5:8000/v1",
            "http://192.168.1.9/v1",
            "http://vllm:8000/v1",
            "http://gpu.pmos.internal/v1",
            "http://[::1]:8000/v1",
        ] {
            let yaml = format!(
                "upstreams: {{x: '{host}'}}\nroles: {{default: {{upstream: x, model: m}}}}\n"
            );
            assert!(Models::from_yaml(&yaml).is_ok(), "{host}");
        }
        // A platform owner can list a host explicitly.
        let yaml = "upstreams: {x: 'http://spark.corp.example/v1'}\nlocal-hosts: [spark.corp.example]\nroles: {default: {upstream: x, model: m}}\n";
        assert!(Models::from_yaml(yaml).is_ok());
    }

    #[test]
    fn refuses_bad_config() {
        let cases = [
            ("upstreams: {x: 'http://vllm/v1'}\nroles: {extraction: {upstream: x, model: m}}\n", "default"),
            ("upstreams: {x: 'http://vllm/v1'}\nroles: {default: {upstream: y, model: m}}\n", "isn't listed"),
            ("upstreams: {x: 'http://vllm/v1'}\nroles: {default: {upstream: x, model: m, workarounds: [magic]}}\n", "unknown workaround"),
            ("upstreams: {x: 'http://vllm/v1'}\nroles: {default: {upstream: x, model: m}, Extract: {upstream: x, model: m}}\n", "lowercase"),
            ("upstreams: {x: 'http://vllm/v1'}\nroles: {default: {upstream: x, model: m}}\nlimits: {max-in-flight: 0}\n", "at least 1"),
            ("upstreams: {x: 'ftp://vllm/v1'}\nroles: {default: {upstream: x, model: m}}\n", "http or https"),
        ];
        for (yaml, want) in cases {
            let err = Models::from_yaml(yaml).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
    }

    #[test]
    fn clients_need_long_distinct_tokens_and_never_print() {
        let t1 = "a".repeat(32);
        let t2 = "b".repeat(32);
        let ok = parse_clients(&format!("runtime={t1}, evals={t2}")).unwrap();
        assert_eq!(ok.len(), 2);
        assert!(!format!("{ok:?}").contains(&t1));
        assert!(parse_clients("runtime=short").is_err());
        assert!(parse_clients(&format!("runtime={t1},evals={t1}")).is_err());
        assert!(parse_clients(&format!("Run={t1}")).is_err());
        assert!(parse_clients("").is_err());
        let cfg = Config::from_lookup(|k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("runtime={t1}")),
            "PMOS_GATEWAY_WORKAROUNDS" => Some("off".into()),
            _ => None,
        })
        .unwrap();
        assert!(!cfg.workarounds_on);
        assert!(!cfg.stub);
        assert_eq!(cfg.bind.port(), 8740);
    }
}
