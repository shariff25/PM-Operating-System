//! Configuration: which local model serves each role, and who may call the gateway.
//!
//! The model map lives in a YAML file (`config/gateway.yaml` by default), so swapping a model
//! is a reviewed one-line change plus a benchmark run (D11). Everything else is environment.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use pmos_contract::model_source::ModelSource;
use serde::Deserialize;

pub use crate::route::{Feature, Hop, RouteRule, Routing};
use crate::scheduler::{PersonPool, Priority};
use crate::workarounds;

/// The read-only client (§4.12): it may read `GET /v1/pmos/routes`, which no other client may,
/// and the gateway refuses it on every model route (`403`).
pub const ADMIN_CLIENT: &str = "admin";

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
    /// How many calls one (believed) person may have waiting for a slot (review finding R2).
    /// `None`: twice `per-user`.
    #[serde(default)]
    pub per_user_queue: Option<usize>,
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
            per_user_queue: None,
        }
    }
}

impl Limits {
    /// The cap for a client that `clients:` doesn't give one.
    pub fn client_default(&self) -> usize {
        self.per_client.unwrap_or(self.per_user)
    }

    /// How many calls one believed person may have waiting: `per-user-queue`, else twice
    /// `per-user`.
    pub fn person_queue(&self) -> usize {
        self.per_user_queue
            .unwrap_or_else(|| self.per_user.saturating_mul(2))
    }

    fn check(&self) -> Result<(), String> {
        if self.max_in_flight == 0
            || self.per_user == 0
            || self.per_client == Some(0)
            || self.per_user_queue == Some(0)
        {
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
    /// The most calls this client may have waiting for a slot. `None`: twice its
    /// `max-in-flight` (review finding R2).
    #[serde(default)]
    max_queue: Option<usize>,
    /// Which people a trusted client may name in `X-Pmos-User`: `any` (the default), or
    /// `from-name` (a `prefix*` pattern only): just the one whose handle the `*` part of the
    /// client's name spells, as a personal bot's name does (review finding R1).
    #[serde(default)]
    user: Option<String>,
    /// How many of one person's `limits.per-user` running slots this client may hold at once
    /// (finding G3 of the review at 6bb806f). `None`: 1 for a trusted `user: any` client (a
    /// team bot, or the runtime); for a `from-name` client, which names only its own PM,
    /// `per-user`, or, with `person-pools:`, what the pools leave of it. A `user: any`
    /// client's share is at most its person pool's slots ([`Models::pools`], U1 and W1).
    #[serde(default)]
    per_user_share: Option<usize>,
    /// The person pool (`person-pools:`) this client's calls are counted in, for a trusted
    /// `user: any` client: the pool's clients together hold at most its slots of one person's
    /// `per-user` (finding W1 of the review at 52dbae2). Required for every such client when
    /// `person-pools:` is given; refused when it isn't.
    #[serde(default)]
    person_pool: Option<String>,
    /// Who may fall back (§4.10): `fallback` (default), `primary-only` or `pin`.
    #[serde(default)]
    route: Option<String>,
}

/// Which people a trusted client may name in `X-Pmos-User` (review finding R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRule {
    /// Anyone: the client verified the person itself (the runtime), or serves the whole team
    /// (a team bot such as `bot-docs`).
    Any,
    /// Only the person whose handle the client's name spells after its first `prefix`
    /// bytes, in the form a personal bot's name gives it ([`handle_in_name`]): `bot-pm-ana-k`
    /// (pattern `bot-pm-*`) may name `ana.k`, and no one else.
    FromName { prefix: usize },
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
    /// The most calls this client may have waiting for a slot.
    pub max_queue: usize,
    /// Which people it may name, when trusted.
    pub user: UserRule,
    /// The most of one believed person's `per-user` running slots this client may hold
    /// (G3, review at 6bb806f): 1 by default for a trusted `user: any` client.
    pub per_user_share: usize,
    /// For a trusted `user: any` client: its person pool, whose clients together hold at
    /// most the pool's `cap` of one person's running slots ([`Models::pools`], findings U1
    /// and W1). `None` for a `from-name` client (it names only its own PM) and an untrusted
    /// one (it names no one).
    pub pool: Option<PersonPool>,
    /// Who may fall back (`clients.<…>.route`, §4.10).
    pub route: RouteRule,
}

impl ClientPolicy {
    /// May `client` (whose policy this is) name `handle` in `X-Pmos-User`?
    ///
    /// Under `from-name`, only when the handle's spelled form, in full, is the `*` part of
    /// the client's name. A handle whose spelled form is longer than a personal bot's name can
    /// hold ([`MAX_SPELLED_HANDLE`], 29) is never named this way (review finding F2 of the
    /// review at 2544d01): its bot name is cut, so the name would also spell every other
    /// handle that starts the same way.
    pub fn may_name(&self, client: &str, handle: &str) -> bool {
        match self.user {
            UserRule::Any => true,
            UserRule::FromName { prefix } => client.get(prefix..).is_some_and(|rest| {
                let spelled = spelled_handle(handle);
                !rest.is_empty() && spelled.len() <= MAX_SPELLED_HANDLE && rest == spelled
            }),
        }
    }
}

/// The one key a call for `handle` is kept under, for the caps and the usage log's `user`,
/// whichever client makes it: the handle's spelled form ([`spelled_handle`]), as a personal
/// bot's name spells it. So `ana.k` through the runtime or a team bot and `ana-k` through
/// `bot-pm-ana-k` are one person, under one running cap (finding G1 of the review at
/// 6bb806f; before, the runtime kept `ana.k` and the PM's own bot `ana-k`, so the person had
/// `per-user` slots under each), and `ana`, `ana.` and `ana..` from `bot-pm-ana` are one
/// person, not three (the Low on canonical handles, review at 2544d01). Handles whose spelled
/// forms are the same (`ana.k`, `ana_k`) share one key: PM OS refuses to create such a handle
/// beside an existing one (`users.create`, `users import`, onboarding), and rows made before
/// that check share their caps.
pub fn person_key(handle: &str) -> String {
    spelled_handle(handle)
}

pub use pmos_contract::onboarding::{spelled_handle, MAX_SPELLED_HANDLE};

/// A handle as PM OS writes it in a personal bot's name, `pm-<this>`
/// ([`pmos_contract::onboarding::personal_bot_name`]): `.` and `_` become `-`, a run of
/// hyphens one, and it is cut so the bot name fits 32 characters. Two handles can give the
/// same form (`ana.k`, `ana_k`); they would also share one personal bot name.
pub fn handle_in_name(handle: &str) -> String {
    let bot = pmos_contract::onboarding::personal_bot_name(handle);
    bot.strip_prefix("pm-").unwrap_or(&bot).to_string()
}

/// The shortest prefix a `prefix*` client pattern may have, so a pattern can never cover every
/// client (a bare `*`) or nearly every one (`b*`). `bot-` is the shortest in use.
pub const MIN_PATTERN_PREFIX: usize = 4;

/// The longest client name: room for `bot-` before the longest personal bot name (32).
pub const MAX_CLIENT_NAME: usize = 40;

/// A client name, as in `PMOS_GATEWAY_TOKENS`: 1 to [`MAX_CLIENT_NAME`] lowercase letters,
/// digits and `-`.
pub fn valid_client_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CLIENT_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A role: the one-hop form (`upstream`, `model`, and optionally `label`, `workarounds`,
/// `effort`), or the route form (`route:` naming models of `models:` by label, with
/// `requires:` and `allow-smaller-context:`; `docs/model-providers-spec.md` §4.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RoleSpec {
    #[serde(default)]
    upstream: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    workarounds: Option<Vec<String>>,
    /// Claude test upstream only: `output_config.effort`.
    #[serde(default)]
    effort: Option<String>,
    /// A neutral name reported to clients in place of `model` ([`Role::label`]).
    #[serde(default)]
    label: Option<String>,
    /// The route form: labels of `models:`, the primary first.
    #[serde(default)]
    route: Option<Vec<String>>,
    /// Features every hop of the route must declare.
    #[serde(default)]
    requires: Option<Vec<String>>,
    /// A fallback may declare a smaller `context-tokens` than the primary.
    #[serde(default)]
    allow_smaller_context: Option<bool>,
}

/// One entry of `upstreams:`: a URL (the form every config had before routes), or
/// `{url, key-file, max-in-flight}` (§4.1, §4.7, §4.11).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum UpstreamEntry {
    Url(String),
    Full(UpstreamSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct UpstreamSpec {
    url: String,
    /// The file holding this upstream's key, read by the gateway only (§4.11).
    #[serde(default)]
    key_file: Option<PathBuf>,
    /// The most calls in flight to this upstream (default `limits.max-in-flight`).
    #[serde(default)]
    max_in_flight: Option<usize>,
}

impl UpstreamEntry {
    fn url(&self) -> &String {
        match self {
            Self::Url(u) => u,
            Self::Full(s) => &s.url,
        }
    }
    fn key_file(&self) -> Option<&PathBuf> {
        match self {
            Self::Url(_) => None,
            Self::Full(s) => s.key_file.as_ref(),
        }
    }
}

/// One model of `models:`, keyed by its label (§4.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ModelSpec {
    upstream: String,
    model: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    context_tokens: Option<u64>,
    #[serde(default)]
    workarounds: Vec<String>,
    #[serde(default)]
    effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ModelsFile {
    upstreams: BTreeMap<String, UpstreamEntry>,
    roles: BTreeMap<String, RoleSpec>,
    /// The models routes name, by label (§4.1).
    #[serde(default)]
    models: BTreeMap<String, ModelSpec>,
    /// How routed roles move between hops (§4.1).
    #[serde(default)]
    routing: Routing,
    #[serde(default)]
    local_hosts: Vec<String>,
    #[serde(default)]
    limits: Limits,
    /// Per-client trust, cap and priority, by client name or `prefix*` pattern.
    #[serde(default)]
    clients: BTreeMap<String, ClientSpec>,
    /// Person pools (finding W1 of the review at 52dbae2): pool name to how many of one
    /// person's `limits.per-user` running slots the pool's clients (`person-pool:`) may hold
    /// together. Empty: one pool, [`IMPLICIT_POOL`], for every trusted `user: any` client.
    #[serde(default)]
    person_pools: BTreeMap<String, usize>,
    /// `production` or `test` (the default): what `GET /v1/pmos/roles` reports, so a
    /// benchmark report says which kind of gateway its models came from (review finding R3).
    #[serde(default)]
    class: Option<String>,
}

/// What kind of gateway this is (review finding R3): only the deployment's own
/// gateway (`deploy/gateway.yaml`) says `production`; anything else, a config that says nothing
/// included, is `test`. A `production` gateway refuses testing-only upstreams and the stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayClass {
    Production,
    #[default]
    Test,
}

impl GatewayClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
        }
    }
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
    /// `label:`, a neutral name for the model (such as `hosted-model-a`). When set, it is what
    /// the gateway reports to clients and writes in its logs in place of `model`
    /// (`GET /v1/pmos/roles`, the `x-pmos-model` header, an answer's `model` field, the usage
    /// log), and the upstream is logged by its kind, not its address, so neither the model id
    /// nor the provider's host leaves the gateway's config. `model` is still what is sent
    /// upstream. A label names one model: give it a new label when the model changes, since
    /// benchmark reports compare roles by what the gateway reports.
    pub label: Option<String>,
    /// The `upstreams:` key, config-local: never logged or reported (§4.9).
    pub upstream_id: String,
    /// The upstream's `key-file:`, when it has one (§4.11).
    pub key_file: Option<PathBuf>,
    /// The route form's models, the primary first (`docs/model-providers-spec.md` §4.1);
    /// `None` for a role in the one-hop form, whose one model is the fields above. For a
    /// routed role those fields are its primary's.
    pub route: Option<Vec<Hop>>,
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
    /// Requests are forwarded as they are: no translation, and no field removed (checked in
    /// slice MP1: there is no `REMOTE_DROPPED_FIELDS`; only the Claude test upstream's
    /// translation drops fields, its sampling parameters). The bearer token is the upstream's
    /// own key: its `key-file:`, or `PMOS_GATEWAY_REMOTE_KEY` for a config with one remote
    /// upstream (`docs/model-providers-spec.md` §4.11).
    RemoteOpenAi,
}

impl UpstreamKind {
    /// A testing-only upstream: prompts sent to it leave this machine.
    pub fn is_cloud_test(self) -> bool {
        matches!(self, Self::ClaudeTest | Self::RemoteOpenAi)
    }

    /// The neutral name for the kind, never an address: `local`, `claude-test` or
    /// `remote-test`.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "local",
            Self::ClaudeTest => "claude-test",
            Self::RemoteOpenAi => "remote-test",
        }
    }
}

impl Role {
    pub fn alias(&self) -> String {
        format!("pmos/{}", self.name)
    }

    /// The model as the gateway reports it to clients and in its logs: the label when one is
    /// set, else the model id.
    pub fn reported_model(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.model)
    }

    /// For a labelled role only: the lowercase hex SHA-256 of its real model id (`model`), so
    /// a benchmark report and the usage log can tell when the model behind an unchanged label
    /// was swapped, without naming it. `None` for a role without a label, which reports its
    /// model id itself. A hash of a short id from a known list can be matched by trying the
    /// list; the reviewer judged that acceptable (`docs/gateway.md`, "Hiding the model behind
    /// a label").
    pub fn model_sha256(&self) -> Option<String> {
        self.label.as_ref().map(|_| sha256_hex(&self.model))
    }

    /// The neutral name for the upstream's kind, never its address: `local`, `claude-test` or
    /// `remote-test`.
    pub fn upstream_kind_label(&self) -> &'static str {
        self.kind.label()
    }

    /// Whether the role is in the route form (§4.1). A one-hop role keeps every behaviour it
    /// had before routes: its retries are `PMOS_GATEWAY_RETRY_*`, it has no circuit, and its
    /// errors are returned as before.
    pub fn routed(&self) -> bool {
        self.route.is_some()
    }

    /// The role's models in order, the primary first: the route form's, or for a one-hop role
    /// the one model its own fields name (read afresh, so a test that points a role's
    /// `upstream` at a mock after parsing is followed).
    pub fn hops(&self) -> Vec<Hop> {
        match &self.route {
            Some(r) => r.clone(),
            None => vec![Hop {
                label: self.label.clone(),
                upstream_id: self.upstream_id.clone(),
                upstream: self.upstream.clone(),
                kind: self.kind,
                model: self.model.clone(),
                workarounds: self.workarounds.clone(),
                effort: self.effort.clone(),
                features: None,
                context_tokens: None,
                key_file: self.key_file.clone(),
            }],
        }
    }

    /// Whether any of the role's models switches on workaround `id`.
    pub fn uses_workaround(&self, id: &str) -> bool {
        self.hops()
            .iter()
            .any(|h| h.workarounds.iter().any(|w| w == id))
    }

    /// The upstream as the gateway's logs name it: its base URL, or, for a labelled role, its
    /// kind ([`Role::upstream_kind_label`]), so a log doesn't name the provider's host.
    pub fn logged_upstream(&self) -> String {
        if self.label.is_some() {
            self.upstream_kind_label().to_string()
        } else {
            self.upstream.clone()
        }
    }

    /// The upstream's host for the usage log, or, for a labelled role, its kind.
    pub fn logged_upstream_host(&self) -> String {
        if self.label.is_some() {
            self.upstream_kind_label().to_string()
        } else {
            self.upstream_host()
        }
    }

    /// The upstream's host.
    pub fn upstream_host(&self) -> String {
        reqwest::Url::parse(&self.upstream)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default()
    }
}

/// The one person pool of a config without `person-pools:`: every trusted `user: any` client
/// is in it, and its slots are `per-user`, less one when a `from-name` client is listed (U1).
pub const IMPLICIT_POOL: &str = "user-any";

/// A person pool's name and its slots of each person's `per-user`.
pub type NamedPool = (String, usize);

#[derive(Debug, Clone)]
pub struct Models {
    pub roles: BTreeMap<String, Role>,
    pub limits: Limits,
    /// `clients:`, resolved: an exact client name, or a pattern ending in `*`.
    pub clients: BTreeMap<String, ClientPolicy>,
    /// `class:` (default `test`).
    pub class: GatewayClass,
    /// The person pools, by [`PersonPool::id`]: each one's name and slots (`person-pools:`,
    /// or the one [`IMPLICIT_POOL`]).
    pub pools: Vec<NamedPool>,
    /// With `person-pools:`, what the pools leave of `per-user`: the most a `from-name`
    /// client (a PM's own bot) may hold of its PM's slots, and its default share. `None`
    /// without `person-pools:`, where that is `per-user`.
    pub own_bot_slots: Option<usize>,
    /// `routing:` (§4.1), for routed roles.
    pub routing: Routing,
    /// `upstreams.<u>.max-in-flight`, by `upstreams:` key (§4.7). An upstream not listed here
    /// is bounded by `limits.max-in-flight` alone.
    pub upstream_caps: BTreeMap<String, usize>,
    /// The `upstreams:` keys some role uses, with their kind (for [`Config::check_keys`]).
    pub upstream_kinds: BTreeMap<String, UpstreamKind>,
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
        let class = match file.class.as_deref() {
            None | Some("test") => GatewayClass::Test,
            Some("production") => GatewayClass::Production,
            Some(other) => {
                return Err(format!(
                    "gateway config: class must be `production` or `test`, not `{other}`"
                ))
            }
        };
        if class == GatewayClass::Production && !file.local_hosts.is_empty() {
            return Err(
                "gateway config: `local-hosts` is for a test gateway; a `class: production` \
                 gateway's model servers are named `<name>.pmos.internal`"
                    .into(),
            );
        }
        let (pools, own_bot_slots) = person_pools(&file, &limits)?;
        let explicit_pools = own_bot_slots.is_some();
        let mut pool_used = vec![false; pools.len()];
        let mut clients = BTreeMap::new();
        for (name, spec) in file.clients {
            let pattern = name.strip_suffix('*');
            if pattern.is_some_and(|p| p.len() < MIN_PATTERN_PREFIX) {
                return Err(format!(
                    "gateway config: client pattern `{name}` must have at least \
                     {MIN_PATTERN_PREFIX} characters before its `*`, so it can't cover every client"
                ));
            }
            let ok = match pattern {
                Some(prefix) => valid_client_name(prefix),
                None => valid_client_name(&name),
            };
            if !ok {
                return Err(format!(
                    "gateway config: client `{name}` must be a client name (lowercase letters, \
                     digits and `-`, at most {MAX_CLIENT_NAME}), optionally ending in `*`"
                ));
            }
            if spec.max_in_flight == Some(0) || spec.max_queue == Some(0) {
                return Err(format!(
                    "gateway config: client `{name}`: max-in-flight and max-queue must be at least 1"
                ));
            }
            let user = match spec.user.as_deref() {
                None | Some("any") => UserRule::Any,
                Some("from-name") => match pattern {
                    Some(prefix) if spec.trusted => UserRule::FromName {
                        prefix: prefix.len(),
                    },
                    Some(_) => {
                        return Err(format!(
                            "gateway config: client `{name}`: `user: from-name` needs \
                             `trusted: true` (an untrusted client names no one)"
                        ))
                    }
                    None => {
                        return Err(format!(
                            "gateway config: client `{name}`: `user: from-name` is for a pattern \
                             ending in `*`, whose `*` part spells the handle"
                        ))
                    }
                },
                Some(other) => {
                    return Err(format!(
                        "gateway config: client `{name}`: user must be `any` or `from-name`, not \
                         `{other}`"
                    ))
                }
            };
            // W1 (review at 52dbae2): a client that may name anyone is counted in a person
            // pool, whose clients together hold at most the pool's slots of one person's.
            let shared = spec.trusted && user == UserRule::Any;
            let pool_id = match (&spec.person_pool, shared) {
                (Some(p), false) => {
                    return Err(format!(
                        "gateway config: client `{name}`: person-pool `{p}` is for a trusted \
                         client that may name anyone (`trusted: true`, `user: any`)"
                    ))
                }
                (Some(p), true) => Some(
                    pools
                        .iter()
                        .position(|(n, _)| n == p)
                        .filter(|_| explicit_pools)
                        .ok_or_else(|| {
                            format!(
                                "gateway config: client `{name}`: person-pool `{p}` isn't \
                                 listed in `person-pools:`"
                            )
                        })?,
                ),
                (None, true) if explicit_pools => {
                    return Err(format!(
                        "gateway config: client `{name}` may name anyone, so with \
                         `person-pools:` it must say which pool it is in (`person-pool:`)"
                    ))
                }
                (None, true) => Some(0),
                (None, false) => None,
            };
            let pool = pool_id.map(|id| {
                pool_used[id] = true;
                PersonPool {
                    id,
                    cap: pools[id].1,
                }
            });
            // G3 (review at 6bb806f): a client that may name anyone holds at most this many of
            // one person's running slots, 1 unless it says otherwise, and never more than its
            // pool's (U1, W1). With `person-pools:`, a PM's own bot holds at most what the
            // pools leave, so each pool's slots are really kept for its clients.
            let own = matches!(user, UserRule::FromName { .. });
            let most = match (pool, own_bot_slots) {
                (Some(p), _) => p.cap,
                (None, Some(left)) if own => left,
                _ => limits.per_user,
            };
            let per_user_share = match spec.per_user_share {
                Some(n) if !spec.trusted => {
                    return Err(format!(
                        "gateway config: client `{name}`: per-user-share ({n}) needs \
                         `trusted: true` (an untrusted client names no one)"
                    ))
                }
                Some(n) if (n == 0 || n > most) && explicit_pools && pool.is_some() => {
                    return Err(format!(
                        "gateway config: client `{name}`: per-user-share must be 1 to {most}, \
                         the slots of its person pool `{}`, not {n}",
                        pools[pool_id.unwrap_or(0)].0
                    ))
                }
                Some(n) if (n == 0 || n > most) && explicit_pools && own => {
                    return Err(format!(
                        "gateway config: client `{name}`: per-user-share must be 1 to {most}, \
                         what the person pools leave of limits.per-user ({}), not {n}",
                        limits.per_user
                    ))
                }
                Some(n) if (n == 0 || n > most) && most < limits.per_user => {
                    return Err(format!(
                        "gateway config: client `{name}`: per-user-share must be 1 to {most}, \
                         not {n}: limits.per-user ({}) less the one slot of each person's kept \
                         for their own bot (a `user: from-name` client is listed)",
                        limits.per_user
                    ))
                }
                Some(n) if n == 0 || n > most => {
                    return Err(format!(
                        "gateway config: client `{name}`: per-user-share must be 1 to \
                         limits.per-user ({}), not {n}",
                        limits.per_user
                    ))
                }
                Some(n) => n,
                None if shared => 1,
                None if own => most,
                None => limits.per_user,
            };
            let route = match spec.route.as_deref() {
                None => RouteRule::Fallback,
                Some(r) => RouteRule::parse(r).ok_or_else(|| {
                    format!(
                        "gateway config: client `{name}`: route must be fallback, primary-only or \
                         pin, not `{r}`"
                    )
                })?,
            };
            if name == ADMIN_CLIENT && spec.route.is_some() {
                return Err(format!(
                    "gateway config: client `{ADMIN_CLIENT}` is read-only and makes no model calls, \
                     so it takes no `route`"
                ));
            }
            let max_in_flight = spec.max_in_flight.unwrap_or(limits.client_default());
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
                    max_in_flight,
                    priority,
                    max_queue: spec
                        .max_queue
                        .unwrap_or_else(|| max_in_flight.saturating_mul(2)),
                    user,
                    per_user_share,
                    pool,
                    route,
                },
            );
        }
        if explicit_pools {
            if let Some(((name, _), _)) = pools.iter().zip(&pool_used).find(|(_, used)| !**used) {
                return Err(format!(
                    "gateway config: person pool `{name}` has no client (`person-pool: {name}`)"
                ));
            }
        }
        let routing = file.routing;
        routing.check()?;
        let mut upstream_caps = BTreeMap::new();
        for (id, entry) in &file.upstreams {
            let UpstreamEntry::Full(u) = entry else {
                continue;
            };
            // An upstream's key is config-local (§4.9): these refusals don't name it.
            if u.max_in_flight == Some(0) {
                return Err(
                    "gateway config: an upstream's `max-in-flight` must be at least 1".into(),
                );
            }
            if let Some(cap) = u.max_in_flight {
                upstream_caps.insert(id.clone(), cap);
            }
            if u.key_file.as_ref().is_some_and(|k| !k.is_absolute()) {
                return Err(
                    "gateway config: an upstream's `key-file` must be an absolute path".into(),
                );
            }
        }
        let ctx = UpstreamCtx {
            local_hosts: &file.local_hosts,
            cloud_test,
            class,
        };
        let mut models_by_label: BTreeMap<String, Hop> = BTreeMap::new();
        for (label, m) in &file.models {
            let hop = model_hop(label, m, (&file.roles, &file.upstreams), &ctx)?;
            models_by_label.insert(label.clone(), hop);
        }
        let mut upstream_kinds = BTreeMap::new();
        let mut roles = BTreeMap::new();
        for (name, spec) in file.roles {
            if !valid_role_name(&name) {
                return Err(format!(
                    "gateway config: role `{name}` must be lowercase letters, digits and `-`"
                ));
            }
            if let Some(route) = &spec.route {
                let role = routed_role(&name, route, &spec, &models_by_label)?;
                for hop in role.route.iter().flatten() {
                    upstream_kinds.insert(hop.upstream_id.clone(), hop.kind);
                }
                roles.insert(name, role);
                continue;
            }
            if spec.requires.is_some() || spec.allow_smaller_context.is_some() {
                return Err(format!(
                    "gateway config: role `{name}`: `requires` and `allow-smaller-context` are for \
                     a role with a `route:`"
                ));
            }
            let (Some(upstream), Some(model)) = (spec.upstream.clone(), spec.model.clone()) else {
                return Err(format!(
                    "gateway config: role `{name}` needs `upstream` and `model`, or a `route:` of \
                     models"
                ));
            };
            let workarounds_on = spec.workarounds.clone().unwrap_or_default();
            let entry = file.upstreams.get(&upstream).ok_or_else(|| {
                format!(
                    "gateway config: role `{name}` names upstream `{}`, which isn't listed",
                    upstream
                )
            })?;
            let base = entry.url();
            let (kind, base) = split_kind(base);
            let url = reqwest::Url::parse(base)
                .map_err(|e| format!("gateway config: upstream `{}`: {e}", upstream))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(format!(
                    "gateway config: upstream `{}` must be http or https",
                    upstream
                ));
            }
            let host = url.host_str().unwrap_or_default();
            if kind.is_cloud_test() {
                if !cloud_test {
                    return Err(match kind {
                        UpstreamKind::ClaudeTest => format!(
                            "gateway config: upstream `{}` is the Claude test upstream; it needs \
                             PMOS_GATEWAY_CLOUD_TEST=1 (testing only: prompts leave this machine)",
                            upstream
                        ),
                        _ => format!(
                            "gateway config: upstream `{}` is a remote test upstream (an \
                             OpenAI-compatible API on the internet), so prompts would leave this \
                             machine; it is for testing only and needs PMOS_GATEWAY_CLOUD_TEST=1",
                            upstream
                        ),
                    });
                }
                // Plain http only to a local mock (the tests); a real remote API is https.
                if url.scheme() != "https" && !is_local_host(host, &file.local_hosts) {
                    return Err(format!(
                        "gateway config: upstream `{}` must use https",
                        upstream
                    ));
                }
            } else if !is_local_host(host, &file.local_hosts) {
                return Err(format!(
                    "gateway config: upstream `{}` ({host}) is not on this machine or its private network; \
                     PM OS sends prompts only to local models",
                    upstream
                ));
            }
            if model.trim().is_empty() {
                return Err(format!("gateway config: role `{name}` has no model"));
            }
            if let Some(l) = &spec.label {
                if !valid_label(l) {
                    return Err(format!(
                        "gateway config: role `{name}` label must be 1 to 64 letters, digits, \
                         `.`, `_` and `-`"
                    ));
                }
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
            for w in &workarounds_on {
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
            if kind.is_cloud_test() && class == GatewayClass::Production {
                return Err(format!(
                    "gateway config: role `{name}` uses a testing-only upstream, which a \
                     `class: production` gateway never serves"
                ));
            }
            // F3 (review at 2544d01): a production gateway's model servers are named only
            // `<name>.pmos.internal`, never by address, a bare name or a `local-hosts` entry,
            // all of which `is_local_host` accepts for a test gateway.
            if class == GatewayClass::Production && !is_production_host(host) {
                return Err(format!(
                    "gateway config: upstream `{}` ({host}): a `class: production` gateway's \
                     model servers must be named `<name>.pmos.internal`",
                    upstream
                ));
            }
            // Slice H4 (spec §3.6, "Which gateway may serve `local`"): only the two model
            // server names, each answered only by its own compiled address ([`pinned_address`],
            // checked again at every connection by `resolve::PinnedResolver`).
            if class == GatewayClass::Production && pinned_address(host).is_none() {
                return Err(format!(
                    "gateway config: upstream `{}` ({host}): a `class: production` gateway's \
                     model servers are {}, each pinned to its own address; another model server \
                     is a reviewed code change",
                    upstream,
                    PINNED_MODEL_SERVERS
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(" and ")
                ));
            }
            if kind == UpstreamKind::OpenAi && entry.key_file().is_some() {
                return Err(format!(
                    "gateway config: role `{name}`: its upstream is a local model server, which \
                     takes no `key-file`"
                ));
            }
            upstream_kinds.insert(upstream.clone(), kind);
            roles.insert(
                name.clone(),
                Role {
                    name,
                    upstream: base.trim_end_matches('/').to_string(),
                    model,
                    workarounds: workarounds_on,
                    kind,
                    effort: spec.effort,
                    label: spec.label,
                    upstream_id: upstream,
                    key_file: entry.key_file().cloned(),
                    route: None,
                },
            );
        }
        // One label, one model: a one-hop role's label may not also be a routed model's.
        for r in roles.values().filter(|r| !r.routed()) {
            if let Some(l) = r
                .label
                .as_ref()
                .filter(|l| models_by_label.contains_key(*l))
            {
                return Err(format!(
                    "gateway config: role `{}` has label `{l}`, which is also a model's label \
                     under `models:`; a label names one model",
                    r.name
                ));
            }
        }
        Ok(Self {
            roles,
            limits,
            clients,
            class,
            pools,
            own_bot_slots,
            routing,
            upstream_caps,
            upstream_kinds,
        })
    }

    /// How `client` is treated: its exact entry in `clients:`, else the longest `prefix*`
    /// pattern that matches it, else the default: not trusted (its `X-Pmos-User` and
    /// `X-Pmos-Priority` are ignored), `personal`, and `limits.per-client` in flight.
    ///
    /// A name that looks like a `from-name` pattern's but doesn't match it (`bot-pm` or
    /// `bot-pmana` beside `bot-pm-*`; [`Models::lookalike_of`]) gets the default too, never a
    /// shorter pattern such as `bot-*`, which may name anyone (the Low on `bot-pm` names,
    /// review at 2544d01). The gateway warns at start-up about a token of such a name.
    pub fn client_policy(&self, client: &str) -> ClientPolicy {
        let default = ClientPolicy {
            trusted: false,
            max_in_flight: self.limits.client_default(),
            priority: Priority::Personal,
            max_queue: self.limits.client_default().saturating_mul(2),
            user: UserRule::Any,
            // Untrusted: it names no one, so no person's slots are counted for it.
            per_user_share: self.limits.per_user,
            pool: None,
            route: RouteRule::Fallback,
        };
        if let Some(p) = self.clients.get(client) {
            return *p;
        }
        if self.lookalike_of(client).is_some() {
            return default;
        }
        self.clients
            .iter()
            .filter_map(|(k, p)| {
                let prefix = k.strip_suffix('*')?;
                client.starts_with(prefix).then_some((prefix.len(), p))
            })
            .max_by_key(|(n, _)| *n)
            .map(|(_, p)| *p)
            .unwrap_or(default)
    }

    /// The `from-name` pattern that `client` looks like but doesn't match, if any: one whose
    /// prefix, without its trailing hyphens, starts `client`'s name, which then doesn't go on
    /// with the full prefix (`bot-pm`, `bot-pmana` for `bot-pm-*`). An exact entry in
    /// `clients:` is never a lookalike.
    pub fn lookalike_of(&self, client: &str) -> Option<&str> {
        if self.clients.contains_key(client) {
            return None;
        }
        self.clients.iter().find_map(|(k, p)| {
            let prefix = k.strip_suffix('*')?;
            if !matches!(p.user, UserRule::FromName { .. }) {
                return None;
            }
            let stem = prefix.trim_end_matches('-');
            (stem.len() < prefix.len() && client.starts_with(stem) && !client.starts_with(prefix))
                .then_some(k.as_str())
        })
    }

    /// What to warn about at start-up: each client token whose name looks like a `from-name`
    /// pattern's but doesn't match it ([`Models::lookalike_of`]). Such a client is served, as
    /// untrusted, rather than stopping the gateway, since a team bot may be named so
    /// (`pmo` gives `bot-pmo`).
    pub fn lookalike_warnings(&self, clients: &[Client]) -> Vec<String> {
        clients
            .iter()
            .filter_map(|c| {
                let pattern = self.lookalike_of(&c.name)?;
                Some(format!(
                    "client `{}` looks like `{pattern}` but doesn't match it: it is not trusted \
                     (it names no one and gets `personal`), and never falls under a shorter \
                     pattern; a PM's own bot is `{pattern}` with the handle after it",
                    c.name
                ))
            })
            .collect()
    }

    /// A person pool's name, by its [`PersonPool::id`].
    pub fn pool_name(&self, id: usize) -> &str {
        self.pools.get(id).map_or("", |(n, _)| n.as_str())
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

/// The person pools (finding W1 of the review at 52dbae2), and, with `person-pools:`, what
/// they leave of `per-user` for a PM's own bot.
///
/// Without `person-pools:`, one pool, [`IMPLICIT_POOL`], holds every trusted `user: any`
/// client, as U1 had it: `per-user - 1` of each person's slots when a `from-name` client is
/// listed (the last is kept for that person's own bot), else `per-user`. With it, each pool
/// has the slots it names; together they must leave at least one slot for a PM's own bot
/// when a `from-name` client is listed, and may never hold more than `per-user`.
fn person_pools(
    file: &ModelsFile,
    limits: &Limits,
) -> Result<(Vec<NamedPool>, Option<usize>), String> {
    let own_bot = file
        .clients
        .values()
        .any(|c| c.user.as_deref() == Some("from-name"));
    let reserve = usize::from(own_bot);
    if file.person_pools.is_empty() {
        if own_bot && limits.per_user < 2 {
            return Err(
                "gateway config: a `user: from-name` client keeps one of each person's running \
                 slots for that person's own bot, so limits.per-user must be at least 2"
                    .into(),
            );
        }
        return Ok((
            vec![(IMPLICIT_POOL.to_string(), limits.per_user - reserve)],
            None,
        ));
    }
    let mut total = 0usize;
    for (name, slots) in &file.person_pools {
        if !valid_client_name(name) {
            return Err(format!(
                "gateway config: person pool `{name}` must be lowercase letters, digits and `-`"
            ));
        }
        if *slots == 0 {
            return Err(format!(
                "gateway config: person pool `{name}` must have at least 1 slot"
            ));
        }
        total = total.saturating_add(*slots);
    }
    if total.saturating_add(reserve) > limits.per_user {
        return Err(format!(
            "gateway config: the person pools hold {total} of each person's running slots{}, \
             more than limits.per-user ({})",
            if own_bot {
                " and one more is kept for their own bot (a `user: from-name` client is listed)"
            } else {
                ""
            },
            limits.per_user
        ));
    }
    Ok((
        file.person_pools
            .iter()
            .map(|(n, s)| (n.clone(), *s))
            .collect(),
        Some(limits.per_user - total),
    ))
}

/// An upstream URL's kind, from its prefix, and the URL without it.
fn split_kind(base: &str) -> (UpstreamKind, &str) {
    if let Some(rest) = base.strip_prefix("anthropic+") {
        (UpstreamKind::ClaudeTest, rest)
    } else if let Some(rest) = base.strip_prefix("openai+") {
        (UpstreamKind::RemoteOpenAi, rest)
    } else {
        (UpstreamKind::OpenAi, base)
    }
}

/// What the start-up checks of a routed model's upstream need.
struct UpstreamCtx<'a> {
    local_hosts: &'a [String],
    cloud_test: bool,
    class: GatewayClass,
}

/// One model of `models:`, checked as a one-hop role's upstream is (MP-3: every hop passes
/// the source's start-up rules, and one that fails stops the gateway). The refusals name the
/// model's label and the setting, never the upstream's key, URL, host or the model id (§4.12).
fn model_hop(
    label: &str,
    m: &ModelSpec,
    (roles, upstreams): (
        &BTreeMap<String, RoleSpec>,
        &BTreeMap<String, UpstreamEntry>,
    ),
    ctx: &UpstreamCtx<'_>,
) -> Result<Hop, String> {
    let fail = |why: &str| Err(format!("gateway config: models.{label}: {why}"));
    if !valid_label(label) {
        return Err(format!(
            "gateway config: model label `{label}` must be 1 to 64 letters, digits, `.`, `_` and `-`"
        ));
    }
    if roles.contains_key(label) {
        return fail("a model's label must not be a role's name");
    }
    let Some(entry) = upstreams.get(&m.upstream) else {
        return fail("its `upstream` isn't listed under `upstreams:`");
    };
    let (kind, base) = split_kind(entry.url());
    let Ok(url) = reqwest::Url::parse(base) else {
        return fail("its upstream's URL doesn't parse");
    };
    if !matches!(url.scheme(), "http" | "https") {
        return fail("its upstream must be http or https");
    }
    let host = url.host_str().unwrap_or_default();
    if kind.is_cloud_test() {
        if !ctx.cloud_test {
            return fail(
                "its upstream is a testing-only one (prompts would leave this machine); it needs \
                 PMOS_GATEWAY_CLOUD_TEST=1",
            );
        }
        if url.scheme() != "https" && !is_local_host(host, ctx.local_hosts) {
            return fail("its upstream must use https");
        }
    } else if !is_local_host(host, ctx.local_hosts) {
        return fail(
            "its upstream is not on this machine or its private network; PM OS sends prompts \
             only to local models",
        );
    }
    if ctx.class == GatewayClass::Production {
        if kind.is_cloud_test() {
            return fail(
                "its upstream is testing-only, which a `class: production` gateway never serves",
            );
        }
        if !is_production_host(host) || pinned_address(host).is_none() {
            return fail(&format!(
                "a `class: production` gateway's model servers are {}, each pinned to its own \
                 address; another model server is a reviewed code change",
                PINNED_MODEL_SERVERS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(" and ")
            ));
        }
    }
    if kind == UpstreamKind::OpenAi && entry.key_file().is_some() {
        return fail("its upstream is a local model server, which takes no `key-file`");
    }
    if m.model.trim().is_empty() {
        return fail("it has no `model` id");
    }
    let mut features = Vec::new();
    for f in &m.features {
        let Some(f) = Feature::parse(f) else {
            return fail(&format!(
                "unknown feature `{f}`; known: {}",
                Feature::ALL.map(Feature::as_str).join(", ")
            ));
        };
        if !features.contains(&f) {
            features.push(f);
        }
    }
    features.sort();
    if kind == UpstreamKind::ClaudeTest && features.contains(&Feature::Embeddings) {
        return fail("the Claude test upstream has no embeddings");
    }
    if m.context_tokens == Some(0) {
        return fail("`context-tokens` must be at least 1");
    }
    if let Some(e) = &m.effort {
        if kind != UpstreamKind::ClaudeTest {
            return fail("`effort` is taken only by the Claude test upstream");
        }
        if !crate::anthropic::EFFORTS.contains(&e.as_str()) {
            return fail(&format!(
                "effort `{e}` must be one of {}",
                crate::anthropic::EFFORTS.join(", ")
            ));
        }
    }
    for w in &m.workarounds {
        if !workarounds::known(w) {
            return fail(&format!(
                "unknown workaround `{w}`; known: {}",
                workarounds::REGISTRY
                    .iter()
                    .map(|w| w.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(Hop {
        label: Some(label.to_string()),
        upstream_id: m.upstream.clone(),
        upstream: base.trim_end_matches('/').to_string(),
        kind,
        model: m.model.clone(),
        workarounds: m.workarounds.clone(),
        effort: m.effort.clone(),
        features: Some(features),
        context_tokens: m.context_tokens,
        key_file: entry.key_file().cloned(),
    })
}

/// A role in the route form (§4.1, §4.8, §4.12): every label names a model of `models:`, none
/// twice; every hop has every feature in `requires:`; and no fallback declares a smaller
/// `context-tokens` than the primary (nor leaves it out when the primary declares one) unless
/// the role says `allow-smaller-context: true`. The role's own fields are its primary's.
fn routed_role(
    name: &str,
    route: &[String],
    spec: &RoleSpec,
    models: &BTreeMap<String, Hop>,
) -> Result<Role, String> {
    if spec.upstream.is_some()
        || spec.model.is_some()
        || spec.label.is_some()
        || spec.workarounds.is_some()
        || spec.effort.is_some()
    {
        return Err(format!(
            "gateway config: role `{name}`: a role with a `route:` names models by label; \
             `upstream`, `model`, `label`, `workarounds` and `effort` belong to each model under \
             `models:`"
        ));
    }
    if route.is_empty() {
        return Err(format!(
            "gateway config: role `{name}` has an empty `route:`"
        ));
    }
    let mut hops: Vec<Hop> = Vec::new();
    for l in route {
        let Some(hop) = models.get(l) else {
            return Err(format!(
                "gateway config: role `{name}`: route names `{l}`, which isn't a model under \
                 `models:`"
            ));
        };
        if hops.iter().any(|h| h.label == hop.label) {
            return Err(format!(
                "gateway config: role `{name}`: route names `{l}` twice"
            ));
        }
        hops.push(hop.clone());
    }
    let mut requires = Vec::new();
    for f in spec.requires.iter().flatten() {
        let Some(f) = Feature::parse(f) else {
            return Err(format!(
                "gateway config: role `{name}`: unknown required feature `{f}`; known: {}",
                Feature::ALL.map(Feature::as_str).join(", ")
            ));
        };
        requires.push(f);
    }
    for hop in &hops {
        if let Some(f) = requires.iter().find(|f| !hop.has(**f)) {
            return Err(format!(
                "gateway config: role `{name}`: model `{}` lacks `{}`, which the role requires",
                hop.reported_model(),
                f.as_str()
            ));
        }
    }
    if !spec.allow_smaller_context.unwrap_or(false) {
        if let Some(primary) = hops[0].context_tokens {
            if let Some(h) = hops[1..]
                .iter()
                .find(|h| h.context_tokens.is_none_or(|c| c < primary))
            {
                return Err(format!(
                    "gateway config: role `{name}`: fallback `{}` declares a smaller \
                     `context-tokens` than the primary (or none); set \
                     `allow-smaller-context: true` on the role to allow it",
                    h.reported_model()
                ));
            }
        }
    }
    let p = &hops[0];
    Ok(Role {
        name: name.to_string(),
        upstream: p.upstream.clone(),
        model: p.model.clone(),
        workarounds: p.workarounds.clone(),
        kind: p.kind,
        effort: p.effort.clone(),
        label: p.label.clone(),
        upstream_id: p.upstream_id.clone(),
        key_file: p.key_file.clone(),
        route: Some(hops),
    })
}

/// Lowercase hex SHA-256 of `text`.
pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A role's `label:`: 1 to 64 ASCII letters, digits, `.`, `_` and `-`.
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
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
/// platform owner listed. Anything else is refused. Only a `class: test` gateway (`hosted-test`,
/// fake data) takes these; a `class: production` one takes only [`is_production_host`] names.
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

/// A `class: production` gateway's model server host (F3, review at 2544d01): a DNS name
/// `<label>.<…>.pmos.internal`, each label lowercase letters, digits and inner hyphens. On
/// PM OS's host these are the vLLM services' network aliases in `deploy/compose.yaml`.
/// `.internal` is reserved for private use, so such a name never resolves on the internet.
pub fn is_production_host(host: &str) -> bool {
    let Some(name) = host.strip_suffix(".pmos.internal") else {
        return false;
    };
    !name.is_empty()
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// The model servers a `class: production` gateway may name, each with the one address it
/// accepts for it (slice H4, `docs/openshell-any-host-spec.md` §3.6): the addresses
/// `deploy/compose.yaml` pins for the two vLLM services (slice H2), beside the policy crate's
/// service addresses. A unit test checks they equal the compose file's literals. They are
/// constants, never settings: `PMOS_NETWORK_SUBNET` and `PMOS_NETWORK_IP_RANGE` don't reach the
/// gateway, so an edit to `deploy/.env` can't widen what it treats as its own model server.
pub const PINNED_MODEL_SERVERS: [(&str, Ipv4Addr); 2] = [
    ("vllm-default.pmos.internal", Ipv4Addr::new(10, 201, 77, 13)),
    (
        "vllm-extraction.pmos.internal",
        Ipv4Addr::new(10, 201, 77, 14),
    ),
];

/// The address compiled in for a production model server's name, compared exactly (a name in
/// another case, or with a trailing dot, is not one of them).
pub fn pinned_address(host: &str) -> Option<Ipv4Addr> {
    PINNED_MODEL_SERVERS
        .iter()
        .find(|(name, _)| *name == host)
        .map(|(_, addr)| *addr)
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
    /// `PMOS_MODEL_SOURCE`: where this gateway's prompts go (`local` or `hosted-test`), required
    /// with no default ([`Config::check_source`]).
    pub model_source: ModelSource,
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
            .field("model_source", &self.model_source)
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
            model_source: ModelSource::from_lookup(&get)?,
        })
    }

    /// The source every answer reports (`x-pmos-model-source`, `/readyz`, the roles answer):
    /// `stub` with the stub model on, which sends no prompt anywhere; else `PMOS_MODEL_SOURCE`.
    pub fn served_source(&self) -> &'static str {
        served_source(self.model_source, self.stub)
    }

    /// Refuses to start when the config doesn't fit `PMOS_MODEL_SOURCE` (slice H4, spec §3.6,
    /// "Where each refusal lives"), checked on the config's own `class` (before the stub's
    /// [`Config::served_class`]):
    ///
    /// - `local`: a `class: production` config only (its model servers are the two pinned
    ///   names), and never `PMOS_GATEWAY_CLOUD_TEST=1`. The stub may be on (a first install).
    /// - `hosted-test`: a `class: test` config only, with `PMOS_GATEWAY_CLOUD_TEST=1` unless the
    ///   stub is on, and a `label:` on every role, so no model id reaches a client, a report or
    ///   a log (r7, X1).
    pub fn check_source(&self, models: &Models) -> Result<(), String> {
        match self.model_source {
            ModelSource::Local => {
                if models.class != GatewayClass::Production {
                    return Err(
                        "PMOS_MODEL_SOURCE=local needs a `class: production` gateway config \
                         (deploy/gateway.yaml), whose model servers are on this host; a `class: \
                         test` config is for hosted-test"
                            .into(),
                    );
                }
                if self.cloud_test {
                    return Err(
                        "PMOS_MODEL_SOURCE=local can't be used with PMOS_GATEWAY_CLOUD_TEST=1, \
                         which lets prompts leave this host"
                            .into(),
                    );
                }
                // MP-3, MP-4: every hop of every route is one of the two pinned model servers
                // (the class's own start-up rules already refuse anything else; this is the
                // source's own check, so no fallback can leave the host).
                for r in models.roles.values() {
                    for h in r.hops() {
                        let host = reqwest::Url::parse(&h.upstream)
                            .ok()
                            .and_then(|u| u.host_str().map(String::from))
                            .unwrap_or_default();
                        if h.kind != UpstreamKind::OpenAi || pinned_address(&host).is_none() {
                            return Err(format!(
                                "PMOS_MODEL_SOURCE=local: role `{}` has a model that isn't one of \
                                 the two pinned model servers on this host",
                                r.name
                            ));
                        }
                    }
                }
            }
            ModelSource::HostedTest => {
                if models.class != GatewayClass::Test {
                    return Err(
                        "PMOS_MODEL_SOURCE=hosted-test needs a `class: test` gateway config \
                         (deploy/gateway.hosted-test.yaml): its endpoint is unapproved, so \
                         nothing it serves counts as production"
                            .into(),
                    );
                }
                if !self.cloud_test && !self.stub {
                    return Err(
                        "PMOS_MODEL_SOURCE=hosted-test needs PMOS_GATEWAY_CLOUD_TEST=1 (testing \
                         only: prompts leave this host, so the stack takes fake data only)"
                            .into(),
                    );
                }
                if let Some(r) = models
                    .roles
                    .values()
                    .find(|r| r.label.is_none() || r.hops().iter().any(|h| h.label.is_none()))
                {
                    return Err(format!(
                        "PMOS_MODEL_SOURCE=hosted-test: role `{}` has no `label:`; every role \
                         needs one, so the gateway reports and logs a label, never a model id",
                        r.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// The class this gateway serves as: the config's, except that with the stub model on it
    /// is always `test`, so a stub run never counts as production. A first install runs the
    /// deployment's own `class: production` config with `PMOS_GATEWAY_STUB=1` until the model
    /// servers are up (`deploy/env.example`), so the gateway starts then, with a warning
    /// (`Some`, the text to log), rather than refusing as it did at 2544d01. The router applies
    /// the same rule ([`crate::router`]).
    pub fn served_class(&self, models: &Models) -> (GatewayClass, Option<&'static str>) {
        if self.stub && models.class == GatewayClass::Production {
            return (
                GatewayClass::Test,
                Some(
                    "PMOS_GATEWAY_STUB=1 with a `class: production` config: serving as class \
                     `test` (stub on); no eval run against this gateway counts",
                ),
            );
        }
        (models.class, None)
    }

    /// Refuses to start when a role uses a testing-only upstream whose key is missing, and
    /// keeps one key per upstream (`docs/model-providers-spec.md` §4.11): an upstream with a
    /// `key-file:` reads its key from that file only; `PMOS_GATEWAY_REMOTE_KEY` serves a config
    /// with one remote test upstream and no `key-file:`, and is refused beside two or more, or
    /// beside a `key-file:`, so no key can reach an upstream it wasn't given for.
    pub fn check_keys(&self, models: &Models) -> Result<(), String> {
        let hops: Vec<Hop> = models.roles.values().flat_map(Role::hops).collect();
        if hops
            .iter()
            .any(|h| h.kind == UpstreamKind::ClaudeTest && h.key_file.is_none())
            && self.anthropic_key.trim().is_empty()
        {
            return Err(
                "a role uses the Claude test upstream; set PMOS_GATEWAY_ANTHROPIC_KEY to a Claude API key"
                    .into(),
            );
        }
        let mut remote: Vec<(&str, bool)> = hops
            .iter()
            .filter(|h| h.kind == UpstreamKind::RemoteOpenAi)
            .map(|h| (h.upstream_id.as_str(), h.key_file.is_some()))
            .collect();
        remote.sort();
        remote.dedup();
        let env_key = !self.remote_key.trim().is_empty();
        if remote.len() >= 2 {
            if env_key {
                return Err(
                    "PMOS_GATEWAY_REMOTE_KEY is for a config with one remote test upstream; with \
                     two or more, each upstream's key is in its own `key-file:`"
                        .into(),
                );
            }
            if remote.iter().any(|(_, file)| !file) {
                return Err(
                    "with two or more remote test upstreams, each needs its own `key-file:`".into(),
                );
            }
        }
        if env_key && remote.iter().any(|(_, file)| *file) {
            return Err(
                "PMOS_GATEWAY_REMOTE_KEY is set, but the remote test upstream has a `key-file:`; \
                 give its key one way only"
                    .into(),
            );
        }
        if remote.iter().any(|(_, file)| !file) && !env_key {
            return Err(
                "a role uses a remote test upstream (openai+https://…); set PMOS_GATEWAY_REMOTE_KEY \
                 to that API's key"
                    .into(),
            );
        }
        Ok(())
    }
}

/// `pmos-gateway --check-config [path]` (§4.12): the config at `path` (else
/// `PMOS_GATEWAY_CONFIG`) put through every start-up check the gateway makes against this
/// environment (`PMOS_MODEL_SOURCE`, `PMOS_GATEWAY_CLOUD_TEST`, the keys), without serving.
/// `PMOS_GATEWAY_TOKENS` may be left out. `Ok`: a summary by label only, never a URL, host,
/// upstream key or model id.
pub fn check_config(
    get: impl Fn(&str) -> Option<String>,
    path: Option<PathBuf>,
) -> Result<String, String> {
    let placeholder = format!("check-config={}", "x".repeat(32));
    let config = Config::from_lookup(|k| match k {
        "PMOS_GATEWAY_TOKENS" => get(k).or_else(|| Some(placeholder.clone())),
        _ => get(k),
    })?;
    let path = path.unwrap_or_else(|| config.models_path.clone());
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: can't read the gateway config: {e}", path.display()))?;
    let models = Models::parse(&text, config.cloud_test)?;
    config.check_keys(&models)?;
    config.check_source(&models)?;
    let mut out = vec![format!(
        "ok: class {}, model source {}",
        models.class.as_str(),
        config.served_source()
    )];
    for r in models.roles.values() {
        let hops = r.hops();
        let labels: Vec<&str> = hops
            .iter()
            .map(|h| h.label.as_deref().unwrap_or("(unlabelled)"))
            .collect();
        out.push(format!(
            "role {}: {} {}",
            r.name,
            if r.routed() { "route" } else { "one model" },
            labels.join(", ")
        ));
    }
    Ok(out.join("\n"))
}

/// What a gateway reports as its source: `stub` with the stub on, else `source`.
pub fn served_source(source: ModelSource, stub: bool) -> &'static str {
    if stub {
        pmos_contract::model_source::STUB
    } else {
        source.as_str()
    }
}

fn parse_clients(spec: &str) -> Result<Vec<Client>, String> {
    let mut clients: Vec<Client> = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (name, token) = entry
            .split_once('=')
            .ok_or("PMOS_GATEWAY_TOKENS entries are `name=token`")?;
        if !valid_client_name(name) {
            return Err(format!(
                "PMOS_GATEWAY_TOKENS: client name `{name}` must be 1 to {MAX_CLIENT_NAME} \
                 lowercase letters, digits and `-`"
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
    fn a_label_is_reported_in_place_of_the_model() {
        let yaml = "upstreams: {x: 'http://vllm/v1'}\nroles:\n  default: {upstream: x, model: real-id, label: hosted-model-a}\n  extraction: {upstream: x, model: other-id}\n";
        let m = Models::from_yaml(yaml).unwrap();
        let labelled = m.role("pmos/default").unwrap();
        assert_eq!(
            labelled.model, "real-id",
            "the real id is still what goes upstream"
        );
        assert_eq!(labelled.reported_model(), "hosted-model-a");
        assert_eq!(labelled.logged_upstream(), "local");
        assert_eq!(labelled.logged_upstream_host(), "local");
        let plain = m.role("pmos/extraction").unwrap();
        assert_eq!(plain.label, None);
        assert_eq!(plain.reported_model(), "other-id");
        assert_eq!(plain.logged_upstream(), "http://vllm/v1");
        assert_eq!(plain.logged_upstream_host(), "vllm");
        let long = "x".repeat(65);
        for bad in ["''", "'has space'", "'a/b'", long.as_str()] {
            let yaml = format!(
                "upstreams: {{x: 'http://vllm/v1'}}\nroles: {{default: {{upstream: x, model: m, label: {bad}}}}}\n"
            );
            let err = Models::from_yaml(&yaml).unwrap_err();
            assert!(err.contains("label"), "{bad}: {err}");
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
            "PMOS_MODEL_SOURCE" => Some("local".into()),
            _ => None,
        })
        .unwrap();
        assert!(!cfg.workarounds_on);
        assert!(!cfg.stub);
        assert_eq!(cfg.bind.port(), 8740);
    }
}
