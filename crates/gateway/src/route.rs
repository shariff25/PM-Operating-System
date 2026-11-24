//! Routes: a role's ordered list of labelled models (`docs/model-providers-spec.md` §4.1).
//!
//! A role in the one-hop form (`{upstream, model, label, workarounds}`) is read as a route of
//! one model built from the role's own fields ([`crate::config::Role::hops`]), and keeps every
//! behaviour it had before routes existed. A role in the route form (`{route: [a, b]}`) names
//! models from `models:` by label; the first is the **primary** (hop 1), the rest are
//! **fallbacks**, tried in order only when an earlier hop fails on the upstream's side
//! (`crate::fallback`).

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::config::{sha256_hex, UpstreamKind};

/// What a model declares it can serve (`features:`), and what a role's route `requires:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// Tool calls (`tools` in the request).
    Tools,
    /// Streamed answers (`stream: true`).
    Stream,
    /// `response_format` (JSON answers).
    Json,
    /// `POST /v1/embeddings`.
    Embeddings,
    /// The model returns `reasoning_content`.
    Reasoning,
}

impl Feature {
    pub const ALL: [Feature; 5] = [
        Feature::Tools,
        Feature::Stream,
        Feature::Json,
        Feature::Embeddings,
        Feature::Reasoning,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Stream => "stream",
            Self::Json => "json",
            Self::Embeddings => "embeddings",
            Self::Reasoning => "reasoning",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

/// One model of a route: an upstream plus a model id, with its label and declared features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// The neutral name reported in its place. Always set for a model of a route; for a
    /// one-hop role, the role's own `label:` (if any).
    pub label: Option<String>,
    /// The `upstreams:` key it is served by. Config-local: never logged, sent or reported,
    /// since an operator could type a vendor's name there (§4.9).
    pub upstream_id: String,
    /// The upstream's base URL, without its `openai+`/`anthropic+` prefix.
    pub upstream: String,
    pub kind: UpstreamKind,
    /// The model id sent upstream; never reported where the hop has a label.
    pub model: String,
    pub workarounds: Vec<String>,
    /// Claude test upstream only: `output_config.effort`.
    pub effort: Option<String>,
    /// `None` for a one-hop role, which declares no features and is never skipped for one.
    pub features: Option<Vec<Feature>>,
    /// `context-tokens:`, when declared.
    pub context_tokens: Option<u64>,
    /// The upstream's `key-file:`, when it has one (§4.11).
    pub key_file: Option<PathBuf>,
}

impl Hop {
    /// The label, or the model id for an unlabelled one-hop role.
    pub fn reported_model(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.model)
    }

    /// The SHA-256 of the model id, for a labelled hop only.
    pub fn model_sha256(&self) -> Option<String> {
        self.label.as_ref().map(|_| sha256_hex(&self.model))
    }

    /// `local`, `claude-test` or `remote-test`: never an address.
    pub fn upstream_kind_label(&self) -> &'static str {
        self.kind.label()
    }

    /// The upstream for the usage log: its kind for a labelled hop, else its host.
    pub fn logged_upstream_host(&self) -> String {
        if self.label.is_some() {
            self.upstream_kind_label().to_string()
        } else {
            reqwest::Url::parse(&self.upstream)
                .ok()
                .and_then(|u| u.host_str().map(String::from))
                .unwrap_or_default()
        }
    }

    /// Whether this hop may be sent a request that uses `f`. A one-hop role's model declares
    /// nothing and is never skipped.
    pub fn has(&self, f: Feature) -> bool {
        self.features.as_ref().is_none_or(|fs| fs.contains(&f))
    }

    /// Under `hosted-test` only (§4.9): the SHA-256 of the upstream's normalised base URL, so
    /// two test upstreams can be told apart in the usage log without naming either.
    pub fn upstream_sha256(&self) -> String {
        sha256_hex(&self.upstream.to_ascii_lowercase())
    }
}

/// Who may fall back (`clients.<…>.route`, §4.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteRule {
    /// Every hop of the role's route, in order (the default).
    #[default]
    Fallback,
    /// Hop 1 or fail.
    PrimaryOnly,
    /// May name one model of the role's route in `X-Pmos-Route-Model`; else as `Fallback`.
    Pin,
}

impl RouteRule {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fallback" => Some(Self::Fallback),
            "primary-only" => Some(Self::PrimaryOnly),
            "pin" => Some(Self::Pin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
            Self::PrimaryOnly => "primary-only",
            Self::Pin => "pin",
        }
    }
}

/// `routing.circuit` (§4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CircuitSettings {
    /// Failures in a row that open a circuit.
    #[serde(default = "default_failures")]
    pub failures: u32,
    /// The first cool-down; doubled on each reopening.
    #[serde(default = "default_cool_down")]
    pub cool_down_secs: u64,
    /// The longest cool-down.
    #[serde(default = "default_max_cool_down")]
    pub max_cool_down_secs: u64,
}

fn default_failures() -> u32 {
    3
}
fn default_cool_down() -> u64 {
    30
}
fn default_max_cool_down() -> u64 {
    300
}

impl Default for CircuitSettings {
    fn default() -> Self {
        Self {
            failures: default_failures(),
            cool_down_secs: default_cool_down(),
            max_cool_down_secs: default_max_cool_down(),
        }
    }
}

impl CircuitSettings {
    pub fn cool_down(&self) -> Duration {
        Duration::from_secs(self.cool_down_secs)
    }
    pub fn max_cool_down(&self) -> Duration {
        Duration::from_secs(self.max_cool_down_secs)
    }
}

/// `routing:` (§4.1): how a routed role's call moves between hops. Every value is optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Routing {
    /// Tries of one hop before the next, the first included.
    #[serde(default = "default_same_hop")]
    pub same_hop_attempts: u32,
    /// All hops of one call, from the first attempt: no attempt or wait starts after it.
    #[serde(default = "default_budget")]
    pub budget_secs: u64,
    /// From the upstream's `200` to the first content delta of a stream (§4.5).
    #[serde(default = "default_first_token")]
    pub first_token_secs: u64,
    /// A longer `Retry-After` moves to the next hop at once.
    #[serde(default = "default_short_retry_after")]
    pub short_retry_after_secs: u64,
    #[serde(default)]
    pub circuit: CircuitSettings,
    /// A hop that answered an earlier call of the same `X-Pmos-Run` goes first (§4.6).
    #[serde(default = "default_sticky")]
    pub sticky_per_run: bool,
}

fn default_same_hop() -> u32 {
    2
}
fn default_budget() -> u64 {
    30
}
fn default_first_token() -> u64 {
    60
}
fn default_short_retry_after() -> u64 {
    5
}
fn default_sticky() -> bool {
    true
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            same_hop_attempts: default_same_hop(),
            budget_secs: default_budget(),
            first_token_secs: default_first_token(),
            short_retry_after_secs: default_short_retry_after(),
            circuit: CircuitSettings::default(),
            sticky_per_run: default_sticky(),
        }
    }
}

impl Routing {
    pub fn check(&self) -> Result<(), String> {
        let bad = |what: &str, range: &str| {
            Err(format!("gateway config: routing.{what} must be {range}"))
        };
        if !(1..=10).contains(&self.same_hop_attempts) {
            return bad("same-hop-attempts", "1 to 10");
        }
        if !(1..=300).contains(&self.budget_secs) {
            return bad("budget-secs", "1 to 300");
        }
        if !(1..=600).contains(&self.first_token_secs) {
            return bad("first-token-secs", "1 to 600");
        }
        if self.short_retry_after_secs > 60 {
            return bad("short-retry-after-secs", "0 to 60");
        }
        let c = &self.circuit;
        if !(1..=100).contains(&c.failures) {
            return bad("circuit.failures", "1 to 100");
        }
        if !(1..=3600).contains(&c.cool_down_secs) {
            return bad("circuit.cool-down-secs", "1 to 3600");
        }
        if !(c.cool_down_secs..=86_400).contains(&c.max_cool_down_secs) {
            return bad(
                "circuit.max-cool-down-secs",
                "at least circuit.cool-down-secs and at most 86400",
            );
        }
        Ok(())
    }

    pub fn budget(&self) -> Duration {
        Duration::from_secs(self.budget_secs)
    }
    pub fn first_token(&self) -> Duration {
        Duration::from_secs(self.first_token_secs)
    }
    pub fn short_retry_after(&self) -> Duration {
        Duration::from_secs(self.short_retry_after_secs)
    }
}
