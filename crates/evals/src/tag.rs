//! Failure tags (D10): every failed task gets exactly one bottleneck.
//!
//! The rules run in the order of [`RULES`]; the first that matches decides the tag, and
//! `model` is what is left. "Error" means the task's `error` field, lowercased; "trace" is the
//! task's trace when the runner could read it.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{status, TraceEntry};
use crate::grade::{extract_urls, normalize_url, Grade};
use crate::outcome::Outcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tag {
    Model,
    Tool,
    Policy,
    Memory,
    Harness,
}

impl Tag {
    pub const ALL: [Tag; 5] = [
        Tag::Model,
        Tag::Tool,
        Tag::Policy,
        Tag::Memory,
        Tag::Harness,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tag::Model => "model",
            Tag::Tool => "tool",
            Tag::Policy => "policy",
            Tag::Memory => "memory",
            Tag::Harness => "harness",
        }
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tag a failure got, the rule that gave it, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tagged {
    pub tag: Tag,
    pub rule: String,
    pub reason: String,
}

/// The rules in the order they run: (rule id, tag, what it matches). The documentation in
/// `docs/evals.md` is this table.
pub const RULES: &[(&str, Tag, &str)] = &[
    ("runtime-unreachable", Tag::Harness, "a call to the runtime failed, so the task was never created or couldn't be followed"),
    ("policy-denial", Tag::Policy, "the trace has a `policy_denial` entry"),
    ("policy-error", Tag::Policy, "the task failed with an error naming a policy refusal: `policy`, `denied`, `forbidden`, `not permitted` or `403`"),
    ("never-claimed", Tag::Harness, "timed out while still `queued`: no worker picked it up"),
    ("cancelled", Tag::Harness, "the task was cancelled; nothing in a benchmark cancels tasks"),
    ("interrupted", Tag::Harness, "the task failed with an error naming the runtime or worker: `interrupted`, `restart`, `lease`, `heartbeat`, `worker`, `sandbox` or `gateway` (the model endpoint itself unreachable)"),
    ("silent-timeout", Tag::Harness, "timed out while `working` with no events: the worker never reported progress"),
    ("done-without-answer", Tag::Harness, "the task ended `done` with an empty result"),
    ("memory-error", Tag::Memory, "the task failed with an error naming memory or the context window: `memory`, `context length`, `context window` or `maximum context`"),
    ("memory-tool-error", Tag::Memory, "the trace has a failed result from a `memory.*` tool"),
    ("tool-error", Tag::Tool, "the trace has a failed tool result (`isError`, `is_error` or `error` set, or a `status` of 400 or more)"),
    ("tool-failure", Tag::Tool, "the task failed with an error naming a tool: `tool`, `mcp`, `connector` or `unavailable`"),
    ("page-not-retrieved", Tag::Tool, "a page to cite is missing, the bot called tools, and no tool result held that page: retrieval didn't surface it"),
    ("model", Tag::Model, "anything else: the bot had what it needed and answered wrong, timed out while working, failed with any other error, or drafted the wrong number of approvals"),
];

const POLICY_WORDS: &[&str] = &["policy", "denied", "forbidden", "not permitted", "403"];
const HARNESS_WORDS: &[&str] = &[
    "interrupted",
    "restart",
    "lease",
    "heartbeat",
    "worker",
    "sandbox",
    "gateway",
];
const MEMORY_WORDS: &[&str] = &[
    "memory",
    "context length",
    "context window",
    "maximum context",
];
const TOOL_WORDS: &[&str] = &["tool", "mcp", "connector", "unavailable"];

fn rule(id: &str) -> (Tag, String) {
    RULES
        .iter()
        .find(|(r, _, _)| *r == id)
        .map(|(r, t, _)| (*t, (*r).to_string()))
        .expect("every rule id used is in RULES")
}

fn tagged(id: &str, reason: impl Into<String>) -> Tagged {
    let (tag, rule) = rule(id);
    Tagged {
        tag,
        rule,
        reason: reason.into(),
    }
}

fn mentions<'a>(text: &str, words: &[&'a str]) -> Option<&'a str> {
    words.iter().copied().find(|w| text.contains(w))
}

/// The tool a trace entry names, from `tool` or `name` in its body.
fn tool_name(body: &Value) -> Option<&str> {
    body.get("tool")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
}

fn is_error_result(body: &Value) -> bool {
    let flag = |k: &str| body.get(k).and_then(Value::as_bool) == Some(true);
    let error = body.get("error").is_some_and(|e| !e.is_null());
    let status = body
        .get("status")
        .and_then(Value::as_u64)
        .is_some_and(|s| s >= 400);
    flag("isError") || flag("is_error") || error || status
}

fn of_kind<'a>(trace: &'a [TraceEntry], kind: &'a str) -> impl Iterator<Item = &'a TraceEntry> {
    trace.iter().filter(move |e| e.kind == kind)
}

/// Tag one failure. Call it only for a task that failed grading.
pub fn tag_failure(outcome: &Outcome, grade: &Grade) -> Tagged {
    if let Some(e) = &outcome.runtime_error {
        return tagged("runtime-unreachable", e.clone());
    }
    let error = outcome.error.as_deref().unwrap_or("").to_lowercase();
    let failed = outcome.status_is(status::FAILED);
    let trace = outcome.trace.as_deref();

    if let Some(e) = trace.and_then(|t| of_kind(t, "policy_denial").next()) {
        return tagged(
            "policy-denial",
            format!("policy denial in the trace: {}", short(&e.body)),
        );
    }
    if failed {
        if let Some(w) = mentions(&error, POLICY_WORDS) {
            return tagged("policy-error", format!("error mentions `{w}`: {error}"));
        }
    }
    if outcome.timed_out && outcome.status_is(status::QUEUED) {
        return tagged(
            "never-claimed",
            "still queued when the runner stopped waiting",
        );
    }
    if outcome.status_is(status::CANCELLED) {
        return tagged("cancelled", "the task was cancelled");
    }
    if failed {
        if let Some(w) = mentions(&error, HARNESS_WORDS) {
            return tagged("interrupted", format!("error mentions `{w}`: {error}"));
        }
    }
    if outcome.timed_out && outcome.status_is(status::WORKING) && outcome.events.is_empty() {
        return tagged(
            "silent-timeout",
            "working, with no events, when the runner stopped waiting",
        );
    }
    if outcome.status_is(status::DONE) && outcome.result_text().trim().is_empty() {
        return tagged("done-without-answer", "done with an empty result");
    }
    if failed {
        if let Some(w) = mentions(&error, MEMORY_WORDS) {
            return tagged("memory-error", format!("error mentions `{w}`: {error}"));
        }
    }
    if let Some(t) = trace {
        let memory_failure = of_kind(t, "tool_result").find(|e| {
            tool_name(&e.body).is_some_and(|n| n.starts_with("memory")) && is_error_result(&e.body)
        });
        if let Some(e) = memory_failure {
            return tagged(
                "memory-tool-error",
                format!("memory tool failed: {}", short(&e.body)),
            );
        }
        if let Some(e) = of_kind(t, "tool_result").find(|e| is_error_result(&e.body)) {
            let name = tool_name(&e.body).unwrap_or("a tool");
            return tagged("tool-error", format!("{name} failed: {}", short(&e.body)));
        }
    }
    if failed {
        if let Some(w) = mentions(&error, TOOL_WORDS) {
            return tagged("tool-failure", format!("error mentions `{w}`: {error}"));
        }
    }
    if let Some(t) = trace {
        let called = of_kind(t, "tool_call").next().is_some();
        if called {
            let retrieved: Vec<String> = of_kind(t, "tool_result")
                .flat_map(|e| extract_urls(&e.body.to_string()))
                .map(|u| normalize_url(&u))
                .collect();
            let missing: Vec<&str> = grade
                .missing_citations()
                .filter(|u| !retrieved.contains(&normalize_url(u)))
                .collect();
            if !missing.is_empty() {
                return tagged(
                    "page-not-retrieved",
                    format!("no tool result held {}", missing.join(", ")),
                );
            }
        }
    }
    let reason = grade
        .failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    let reason = match (failed, error.is_empty()) {
        (true, false) => format!("failed: {error}"),
        _ if reason.is_empty() => "failed grading".to_string(),
        _ => reason,
    };
    tagged("model", reason)
}

/// A trace body, cut to one readable line.
fn short(body: &Value) -> String {
    let s = body.to_string();
    if s.chars().count() <= 160 {
        s
    } else {
        let cut: String = s.chars().take(160).collect();
        format!("{cut}…")
    }
}
