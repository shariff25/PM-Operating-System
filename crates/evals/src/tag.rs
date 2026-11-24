//! Failure tags (D10): every failed task gets exactly one bottleneck, and a sub-tag from
//! `evals/taxonomy.yaml` where the failure maps to one.
//!
//! The rules run in the order of [`RULES`]; the first that matches decides the tag, and
//! `model` is what is left. "Error" means the task's `error` field, lowercased; "trace" is the
//! task's trace when the runner could read it.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{status, TraceEntry};
use crate::grade::{extract_urls, normalize_url, Check, Grade};
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
    /// The sub-tag in full (`model/missing-citation`), from `evals/taxonomy.yaml`. Reports
    /// written before sub-tags have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
}

/// The rules in the order they run: (rule id, tag, what it matches). The documentation in
/// `docs/evals.md` is this table.
pub const RULES: &[(&str, Tag, &str)] = &[
    ("runtime-unreachable", Tag::Harness, "a call to the runtime failed, so the task was never created or couldn't be followed"),
    ("policy-denial", Tag::Policy, "the trace has a `policy_denial` entry"),
    ("policy-error", Tag::Policy, "the task failed with an error naming a policy refusal: `policy`, `denied`, `forbidden`, `not permitted` or `403`"),
    ("never-claimed", Tag::Harness, "timed out while still `queued`: no worker picked it up"),
    ("cancelled", Tag::Harness, "the task was cancelled; nothing in a benchmark cancels tasks"),
    ("interrupted", Tag::Harness, "the task failed with an error naming the runtime or worker: `interrupted`, `restart`, `lease`, `heartbeat`, `worker`, `sandbox` or `gateway` (the model endpoint itself unreachable), or `model is busy` (it answered 429 or 503 after the gateway's retries), each at the start of a word"),
    ("silent-timeout", Tag::Harness, "timed out while `working` with no events: the worker never reported progress"),
    ("done-without-answer", Tag::Harness, "the task ended `done` with an empty result"),
    ("drafts-unreadable", Tag::Harness, "the task has `drafts` checks and its drafts couldn't be read"),
    ("duplicate-approvals", Tag::Harness, "more approval items than expected from a task that ran more than once (attempts > 1): the retry drafted them again"),
    ("memory-error", Tag::Memory, "the task failed with an error naming memory or the context window: `memory`, `context length`, `context window` or `maximum context`"),
    ("memory-tool-error", Tag::Memory, "the trace has a failed result from a `memory.*` tool"),
    ("tool-error", Tag::Tool, "the trace has a failed tool result (`isError`, `is_error` or `error` set, or a `status` of 400 or more)"),
    ("tool-failure", Tag::Tool, "the task failed with an error naming a tool: `tool`, `mcp`, `connector` or `unavailable`"),
    ("page-not-retrieved", Tag::Tool, "a page to cite is missing, the bot called tools, and no tool result held that page: retrieval didn't surface it"),
    ("draft-wrong-target", Tag::Tool, "a draft goes to the wrong place: its Jira `project`, Confluence `space`, or Slack `channel` or `thread_ts` isn't one of the `allowed` values"),
    ("model", Tag::Model, "anything else: the bot had what it needed and answered wrong, answered what it should have declined, timed out while working, failed with any other error, or drafted the wrong number or content"),
];

/// Every sub-tag the rules emit. `evals/taxonomy.yaml` must list each of them.
pub const SUB_TAGS: &[&str] = &[
    "harness/runtime-unreachable",
    "policy/denied-in-trace",
    "policy/refused",
    "harness/never-claimed",
    "harness/cancelled",
    "harness/interrupted",
    "harness/silent-timeout",
    "harness/empty-answer",
    "harness/drafts-unreadable",
    "harness/approval-count",
    "memory/context-limit",
    "memory/tool-failed",
    "tool/call-failed",
    "tool/unavailable",
    "tool/page-not-retrieved",
    "tool/wrong-jira-project",
    "tool/wrong-confluence-space",
    "tool/wrong-slack-channel",
    "model/answered-beyond-sources",
    "model/missing-citation",
    "model/leaked-reasoning",
    "model/missing-fact",
    "model/forbidden-text",
    "model/wrong-draft-count",
    "model/incomplete-draft",
    "model/wrong-draft-value",
    "model/draft-content",
    "model/failed",
    "model/timed-out",
    "model/no-answer",
];

/// The sub-tag each rule but `model` and `draft-wrong-target` gives.
fn rule_sub(rule: &str) -> Option<&'static str> {
    Some(match rule {
        "runtime-unreachable" => "harness/runtime-unreachable",
        "policy-denial" => "policy/denied-in-trace",
        "policy-error" => "policy/refused",
        "never-claimed" => "harness/never-claimed",
        "cancelled" => "harness/cancelled",
        "interrupted" => "harness/interrupted",
        "silent-timeout" => "harness/silent-timeout",
        "done-without-answer" => "harness/empty-answer",
        "drafts-unreadable" => "harness/drafts-unreadable",
        "duplicate-approvals" => "harness/approval-count",
        "memory-error" => "memory/context-limit",
        "memory-tool-error" => "memory/tool-failed",
        "tool-error" => "tool/call-failed",
        "tool-failure" => "tool/unavailable",
        "page-not-retrieved" => "tool/page-not-retrieved",
        _ => return None,
    })
}

const POLICY_WORDS: &[&str] = &["policy", "denied", "forbidden", "not permitted", "403"];
const HARNESS_WORDS: &[&str] = &[
    "interrupted",
    "restart",
    "lease",
    "heartbeat",
    "worker",
    "sandbox",
    "gateway",
    // The harness's own words for a model server that answered 429 or 503 after the
    // gateway's retries (`pmReason` in harness/src/worker.ts): the endpoint, not the bot.
    "model is busy",
    "model is still busy",
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
        sub: rule_sub(&rule).map(str::to_string),
        rule,
        reason: reason.into(),
    }
}

/// The first of `words` that `text` mentions at the start of a word: `restart` matches
/// "restarts", but `lease` doesn't match "please" (the harness's "Please try again" once
/// tagged every model-busy failure as a lost lease) and `403` doesn't match "14033".
fn mentions<'a>(text: &str, words: &[&'a str]) -> Option<&'a str> {
    words.iter().copied().find(|w| {
        text.match_indices(w).any(|(i, _)| {
            !text[..i]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        })
    })
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

/// The sub-tag for a draft sent to the wrong place, or `None` when the field isn't where
/// the draft goes.
fn wrong_target(tool: &str, field: &str) -> Option<&'static str> {
    let system = tool.split('.').next().unwrap_or_default();
    match (system, field) {
        ("jira", "project") => Some("tool/wrong-jira-project"),
        ("confluence", "space") => Some("tool/wrong-confluence-space"),
        ("slack", "channel" | "thread_ts") => Some("tool/wrong-slack-channel"),
        _ => None,
    }
}

/// A failed check's `model` sub-tag and its rank: when several checks fail, the lowest rank
/// decides (declining first, then citations, then facts, then drafts, then status).
fn check_sub(c: &Check) -> Option<(u8, &'static str)> {
    Some(match c {
        Check::NotDeclined => (0, "model/answered-beyond-sources"),
        Check::MissingCitation { .. } => (1, "model/missing-citation"),
        Check::ForbiddenText { text } if text.contains("<think>") => (2, "model/leaked-reasoning"),
        Check::MissingText { .. } => (3, "model/missing-fact"),
        Check::ForbiddenText { .. } => (4, "model/forbidden-text"),
        Check::DraftCount { .. } | Check::Approvals { .. } => (5, "model/wrong-draft-count"),
        Check::DraftMissingField { .. } => (6, "model/incomplete-draft"),
        Check::DraftNotAllowed { .. } => (7, "model/wrong-draft-value"),
        Check::DraftMissingText { .. } | Check::DraftForbiddenText { .. } => {
            (8, "model/draft-content")
        }
        Check::Status { .. } => (9, "model/failed"),
        _ => return None,
    })
}

/// The sub-tag a `model` failure gets: from how it failed, else its highest-ranked failed
/// check ([`check_sub`]).
fn model_sub(outcome: &Outcome, grade: &Grade) -> &'static str {
    if outcome.status_is(status::FAILED) {
        return "model/failed";
    }
    if outcome.timed_out {
        return "model/timed-out";
    }
    grade
        .failures
        .iter()
        .filter_map(check_sub)
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, sub)| sub)
        .unwrap_or("model/no-answer")
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
    if let Some(Check::DraftsUnreadable { error }) = grade
        .failures
        .iter()
        .find(|c| matches!(c, Check::DraftsUnreadable { .. }))
    {
        return tagged("drafts-unreadable", error.clone());
    }
    if outcome.attempts > 1 {
        let extra = grade.failures.iter().find_map(|c| match c {
            Check::Approvals { expected, got } if *got > *expected as usize => {
                Some((*expected, *got))
            }
            _ => None,
        });
        if let Some((expected, got)) = extra {
            return tagged(
                "duplicate-approvals",
                format!(
                    "{got} approval items, expected {expected}, after {} attempts",
                    outcome.attempts
                ),
            );
        }
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
    let target = grade.failures.iter().find_map(|c| match c {
        Check::DraftNotAllowed { tool, field, .. } => {
            wrong_target(tool, field).map(|sub| (sub, c.to_string()))
        }
        _ => None,
    });
    if let Some((sub, reason)) = target {
        let mut t = tagged("draft-wrong-target", reason);
        t.sub = Some(sub.to_string());
        return t;
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
    let mut t = tagged("model", reason);
    t.sub = Some(model_sub(outcome, grade).to_string());
    t
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
