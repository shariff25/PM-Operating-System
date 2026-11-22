//! Configuration: which local model serves each role, and who may call the gateway.
//!
//! The model map lives in a YAML file (`config/gateway.yaml` by default), so swapping a model
//! is a reviewed one-line change plus a benchmark run (D11). Everything else is environment.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::Deserialize;

use crate::scheduler::Priority;
use crate::workarounds;

/// How many requests run at once, how many any one person may have running, how many may
/// wait before the gateway answers "busy", and how long and how large one call may be.
///
/// The settings added for review findings N3 and N4 (`per-client`, `queue-wait-secs`,
/// `stream-max-secs`, `max-answer-bytes`) have safe defaults, so a config that doesn't
/// mention them still bounds every client, every wait, every stream and every answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Limits {
    #[serde(default = "default_in_flight")]
    pub max_in_flight: usize,
    #[serde(default = "default_per_user")]
    pub per_user: usize,
    #[serde(default = "default_queue")]
    pub max_queue: usize,
    /// How many calls one client (one gateway token) may have running, for a client that
    /// `clients:` doesn't give its own `max-in-flight`. `None`: `per-user`.
    #[serde(default)]
    pub per_client: Option<usize>,
    /// How long a call may wait for a slot before it gets `503` (with `Retry-After`).
    #[serde(default = "default_queue_wait")]
    pub queue_wait_secs: u64,
    /// The most a streamed answer may take in all, from its first byte to its last.
    #[serde(default = "default_stream_max")]
    pub stream_max_secs: u64,
    /// The largest answer taken from a model server, streamed or not.
    #[serde(default = "default_max_answer")]
    pub max_answer_bytes: usize,
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
fn default_queue_wait() -> u64 {
    300
}
fn default_stream_max() -> u64 {
    900
}
fn default_max_answer() -> usize {
    16 * 1024 * 1024
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_in_flight: default_in_flight(),
            per_user: default_per_user(),
            max_queue: default_queue(),
            per_client: None,
            queue_wait_secs: default_queue_wait(),
            stream_max_secs: default_stream_max(),
            max_answer_bytes: default_max_answer(),
        }
    }
}

impl Limits {
    /// The cap for a client that `clients:` doesn't give one.
    pub fn client_default(&self) -> usize {
        self.per_client.unwrap_or(self.per_user)
    }

    fn check(&self) -> Result<(), String> {
        if self.max_in_flight == 0 || self.per_user == 0 || self.per_client == Some(0) {
            return Err("gateway config: limits must be at least 1".into());
        }
        if !(1..=3600).contains(&self.queue_wait_secs) {
            return Err("gateway config: limits.queue-wait-secs must be 1 to 3600".into());
        }
        if !(1..=7200).contains(&self.stream_max_secs) {
            return Err("gateway config: limits.stream-max-secs must be 1 to 7200".into());
        }
        if !(1024..=256 * 1024 * 1024).contains(&self.max_answer_bytes) {
            return Err("gateway config: limits.max-answer-bytes must be 1024 to 268435456".into());
        }
        Ok(())
    }
}

/// What `clients:` says about one client (a gateway token's name, or a `prefix*` pattern).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ClientSpec {
    /// Believe the client's `X-Pmos-User` and `X-Pmos-Priority`.
    #[serde(default)]
    trusted: bool,
    /// The most calls this client may have running at once.
    #[serde(default)]
    max_in_flight: Option<usize>,
    /// The priority of its calls: always, for a client that isn't trusted; when it sends no
    /// `X-Pmos-Priority`, for one that is.
    #[serde(default)]
    priority: Option<String>,
}

/// How the gateway treats one client (review finding N3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPolicy {
    /// `X-Pmos-User` and `X-Pmos-Priority` are believed. Otherwise the client's calls are
    /// keyed by its name (`client:<name>`) and get `priority`, whatever they declare.
    pub trusted: bool,
    /// The most calls this client may have running at once.
    pub max_in_flight: usize,
    /// For an untrusted client, every call's priority; for a trusted one, the priority of a
    /// call without `X-Pmos-Priority`.
    pub priority: Priority,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RoleSpec {
    upstream: String,
    model: String,
    #[serde(default)]
    workarounds: Vec<String>,
    /// Claude test upstream only: `output_config.effort`.
    #[serde(default)]
    effort: Option<String>,
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
    /// Per-client trust, cap and priority, by client name or `prefix*` pattern.
    #[serde(default)]
    clients: BTreeMap<String, ClientSpec>,
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
    /// The API the upstream speaks.
    pub kind: UpstreamKind,
    /// Claude test upstream only: the effort level sent with each request.
    pub effort: Option<String>,
}

/// What an upstream speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamKind {
    /// An OpenAI-compatible local model server (vLLM). The only kind outside testing.
    OpenAi,
    /// **Testing only:** the Claude Messages API, written `anthropic+https://…` and allowed
    /// only with `PMOS_GATEWAY_CLOUD_TEST=1` (`anthropic.rs`).
    ClaudeTest,
    /// **Testing only:** an OpenAI-compatible API on the public internet, written
    /// `openai+https://<host>/<base path>` and allowed only with `PMOS_GATEWAY_CLOUD_TEST=1`.
    /// Requests are forwarded as they are (no translation), with `PMOS_GATEWAY_REMOTE_KEY` as
    /// the bearer token and the fields in `app::REMOTE_DROPPED_FIELDS` removed.
    RemoteOpenAi,
}

impl UpstreamKind {
    /// A testing-only upstream: prompts sent to it leave this machine.
    pub fn is_cloud_test(self) -> bool {
        matches!(self, Self::ClaudeTest | Self::RemoteOpenAi)
    }
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
    /// `clients:`, resolved: an exact client name, or a pattern ending in `*`.
    pub clients: BTreeMap<String, ClientPolicy>,
}

impl Models {
    /// The config, refusing any upstream that isn't a local model server.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        Self::parse(text, false)
    }

    /// The config, also accepting `anthropic+https://` (Claude) and `openai+https://` (a
    /// remote OpenAI-compatible API) upstreams when `cloud_test` is set. Testing only:
    /// prompts to those leave the machine.
    pub fn parse(text: &str, cloud_test: bool) -> Result<Self, String> {
        let file: ModelsFile =
            serde_norway::from_str(text).map_err(|e| format!("gateway config: {e}"))?;
        if file.roles.is_empty() {
            return Err("gateway config: no roles".into());
        }
        if !file.roles.contains_key("default") {
            return Err("gateway config: a `default` role is required".into());
        }
        let limits = file.limits;
        limits.check()?;
        let mut clients = BTreeMap::new();
        for (name, spec) in file.clients {
            let ok = match name.strip_suffix('*') {
                Some(prefix) => prefix.is_empty() || valid_role_name(prefix),
                None => valid_role_name(&name),
            };
            if !ok {
                return Err(format!(
                    "gateway config: client `{name}` must be a client name (lowercase letters, \
                     digits and `-`), optionally ending in `*`"
                ));
            }
            if spec.max_in_flight == Some(0) {
                return Err(format!(
                    "gateway config: client `{name}`: max-in-flight must be at least 1"
                ));
            }
            let priority = match spec.priority.as_deref() {
                None if spec.trusted => Priority::Interactive,
                None => Priority::Personal,
                Some(p) => Priority::parse(p).ok_or_else(|| {
                    format!(
                        "gateway config: client `{name}`: priority must be interactive, \
                         scheduled or personal, not `{p}`"
                    )
                })?,
            };
            clients.insert(
                name,
                ClientPolicy {
                    trusted: spec.trusted,
                    max_in_flight: spec.max_in_flight.unwrap_or(limits.client_default()),
                    priority,
                },
            );
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
            let (kind, base) = if let Some(rest) = base.strip_prefix("anthropic+") {
                (UpstreamKind::ClaudeTest, rest)
            } else if let Some(rest) = base.strip_prefix("openai+") {
                (UpstreamKind::RemoteOpenAi, rest)
            } else {
                (UpstreamKind::OpenAi, base.as_str())
            };
            let url = reqwest::Url::parse(base)
                .map_err(|e| format!("gateway config: upstream `{}`: {e}", spec.upstream))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(format!(
                    "gateway config: upstream `{}` must be http or https",
                    spec.upstream
                ));
            }
            let host = url.host_str().unwrap_or_default();
            if kind.is_cloud_test() {
                if !cloud_test {
                    return Err(match kind {
                        UpstreamKind::ClaudeTest => format!(
                            "gateway config: upstream `{}` is the Claude test upstream; it needs \
                             PMOS_GATEWAY_CLOUD_TEST=1 (testing only: prompts leave this machine)",
                            spec.upstream
                        ),
                        _ => format!(
                            "gateway config: upstream `{}` is a remote test upstream (an \
                             OpenAI-compatible API on the internet), so prompts would leave this \
                             machine; it is for testing only and needs PMOS_GATEWAY_CLOUD_TEST=1",
                            spec.upstream
                        ),
                    });
                }
                // Plain http only to a local mock (the tests); a real remote API is https.
                if url.scheme() != "https" && !is_local_host(host, &file.local_hosts) {
                    return Err(format!(
                        "gateway config: upstream `{}` must use https",
                        spec.upstream
                    ));
                }
            } else if !is_local_host(host, &file.local_hosts) {
                return Err(format!(
                    "gateway config: upstream `{}` ({host}) is not on this machine or its private network; \
                     PM OS sends prompts only to local models",
                    spec.upstream
                ));
            }
            if spec.model.trim().is_empty() {
                return Err(format!("gateway config: role `{name}` has no model"));
            }
            if let Some(e) = &spec.effort {
                if kind != UpstreamKind::ClaudeTest {
                    return Err(format!(
                        "gateway config: role `{name}` sets `effort`, which only the Claude test upstream takes"
                    ));
                }
                if !crate::anthropic::EFFORTS.contains(&e.as_str()) {
                    return Err(format!(
                        "gateway config: role `{name}` effort `{e}` must be one of {}",
                        crate::anthropic::EFFORTS.join(", ")
                    ));
                }
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
                    kind,
                    effort: spec.effort,
                },
            );
        }
        Ok(Self {
            roles,
            limits,
            clients,
        })
    }

    /// How `client` is treated: its exact entry in `clients:`, else the longest `prefix*`
    /// pattern that matches it, else the default: not trusted (its `X-Pmos-User` and
    /// `X-Pmos-Priority` are ignored), `personal`, and `limits.per-client` in flight.
    pub fn client_policy(&self, client: &str) -> ClientPolicy {
        if let Some(p) = self.clients.get(client) {
            return *p;
        }
        self.clients
            .iter()
            .filter_map(|(k, p)| {
                let prefix = k.strip_suffix('*')?;
                client.starts_with(prefix).then_some((prefix.len(), p))
            })
            .max_by_key(|(n, _)| *n)
            .map(|(_, p)| *p)
            .unwrap_or(ClientPolicy {
                trusted: false,
                max_in_flight: self.limits.client_default(),
                priority: Priority::Personal,
            })
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

#[derive(Clone)]
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
    /// `PMOS_GATEWAY_CLOUD_TEST=1`: **testing only**, accept the Claude test upstream.
    pub cloud_test: bool,
    /// `PMOS_GATEWAY_ANTHROPIC_KEY`: the Claude API key for the test upstream. Required when
    /// a role uses it; empty otherwise.
    pub anthropic_key: String,
    /// `PMOS_GATEWAY_REMOTE_KEY`: the bearer key for remote OpenAI-compatible test upstreams
    /// (`openai+https://…`). Required when a role uses one; empty otherwise.
    pub remote_key: String,
    /// `PMOS_GATEWAY_RETRY_*`: how a `429`/`503` from the upstream is retried (`retry.rs`).
    pub retry: crate::retry::RetryPolicy,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = |k: &str| if k.is_empty() { "" } else { "<redacted>" };
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("models_path", &self.models_path)
            .field("clients", &self.clients)
            .field("usage_log", &self.usage_log)
            .field("stub", &self.stub)
            .field("workarounds_on", &self.workarounds_on)
            .field("cloud_test", &self.cloud_test)
            .field("anthropic_key", &redacted(&self.anthropic_key))
            .field("remote_key", &redacted(&self.remote_key))
            .field("retry", &self.retry)
            .finish()
    }
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
            cloud_test: get("PMOS_GATEWAY_CLOUD_TEST").as_deref() == Some("1"),
            anthropic_key: get("PMOS_GATEWAY_ANTHROPIC_KEY").unwrap_or_default(),
            remote_key: get("PMOS_GATEWAY_REMOTE_KEY").unwrap_or_default(),
            retry: crate::retry::RetryPolicy::from_lookup(&get)?,
        })
    }

    /// Refuses to start when a role uses a testing-only upstream whose key is missing.
    pub fn check_keys(&self, models: &Models) -> Result<(), String> {
        let uses = |kind| models.roles.values().any(|r| r.kind == kind);
        if uses(UpstreamKind::ClaudeTest) && self.anthropic_key.trim().is_empty() {
            return Err(
                "a role uses the Claude test upstream; set PMOS_GATEWAY_ANTHROPIC_KEY to a Claude API key"
                    .into(),
            );
        }
        if uses(UpstreamKind::RemoteOpenAi) && self.remote_key.trim().is_empty() {
            return Err(
                "a role uses a remote test upstream (openai+https://…); set PMOS_GATEWAY_REMOTE_KEY \
                 to that API's key"
                    .into(),
            );
        }
        Ok(())
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
roles:\n  default: {upstream: qwen, model: qwen3.6-35b-a3b, workarounds: [strip-think]}\n  reasoning: {upstream: qwen, model: qwen3.6-35b-a3b, workarounds: [strip-think]}\n  extraction: {upstream: nemo, model: nemotron-3-nano}\n";

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
        assert_eq!(
            m.aliases(),
            vec!["pmos/default", "pmos/extraction", "pmos/reasoning"]
        );
        assert_eq!(
            m.role("pmos/reasoning").unwrap().model,
            m.role("pmos/default").unwrap().model
        );
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
