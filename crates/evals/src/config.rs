//! `evals/config.yaml`: the runner's defaults and the latency targets for `load`.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub runner: Runner,
    pub latency_targets: LatencyTargets,
    /// What a `must_decline` answer says: it passes when it contains any of these, compared
    /// as text is (see [`crate::grade::normalize_text`]).
    #[serde(default)]
    pub decline_phrases: Vec<String>,
    /// Which runs count towards a bar ([`crate::history::counts`]).
    #[serde(default)]
    pub benchmark: Benchmark,
}

/// Where the production role models are declared.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Benchmark {
    /// The production gateway config, relative to the evals directory: the `model` of each of
    /// its roles is what production uses, so only runs whose gateway reported exactly these
    /// count. Default `../deploy/gateway.yaml`.
    #[serde(default = "default_production_gateway")]
    pub production_gateway: String,
}

fn default_production_gateway() -> String {
    "../deploy/gateway.yaml".into()
}

impl Default for Benchmark {
    fn default() -> Self {
        Self {
            production_gateway: default_production_gateway(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    pub bots_url: String,
    pub requested_by: String,
    pub poll_ms: u64,
    pub task_timeout_seconds: u64,
    pub concurrency: usize,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LatencyTargets {
    pub queue_p50: Target,
    pub queue_p90: Target,
    pub total_p50: Target,
    pub total_p90: Target,
}

/// A target in seconds, and where the number comes from.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub seconds: f64,
    pub source: String,
}

impl Config {
    pub fn load(root: &Path) -> Result<Config, String> {
        let path = root.join("config.yaml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: can't read: {e}", path.display()))?;
        let config: Config =
            serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut errors = Vec::new();
        let r = &config.runner;
        if r.bots_url.trim().is_empty() {
            errors.push("runner.bots_url is empty".to_string());
        }
        if r.requested_by.trim().is_empty() {
            errors.push("runner.requested_by is empty".to_string());
        }
        if r.poll_ms == 0 {
            errors.push("runner.poll_ms must be more than 0".to_string());
        }
        if r.task_timeout_seconds == 0 {
            errors.push("runner.task_timeout_seconds must be more than 0".to_string());
        }
        if r.concurrency == 0 {
            errors.push("runner.concurrency must be at least 1".to_string());
        }
        let t = &config.latency_targets;
        for (name, target) in [
            ("queue_p50", &t.queue_p50),
            ("queue_p90", &t.queue_p90),
            ("total_p50", &t.total_p50),
            ("total_p90", &t.total_p90),
        ] {
            if !target.seconds.is_finite() || target.seconds <= 0.0 {
                errors.push(format!(
                    "latency_targets.{name}.seconds must be more than 0"
                ));
            }
            if target.source.trim().is_empty() {
                errors.push(format!(
                    "latency_targets.{name}.source is empty; say where the number comes from"
                ));
            }
        }
        if config
            .decline_phrases
            .iter()
            .any(|p| crate::grade::normalize_text(p).is_empty())
        {
            errors.push("decline_phrases has an empty entry".to_string());
        }
        if config.benchmark.production_gateway.trim().is_empty() {
            errors.push("benchmark.production_gateway is empty".to_string());
        }
        if errors.is_empty() {
            Ok(config)
        } else {
            Err(format!("{}: {}", path.display(), errors.join("; ")))
        }
    }
}
