//! The failure-tagging rules: each rule, and their order.

use std::collections::HashSet;

use chrono::Utc;
use pmos_evals::client::{Event, TraceEntry};
use pmos_evals::grade::{Check, Grade};
use pmos_evals::outcome::Outcome;
use pmos_evals::tag::{tag_failure, Tag, RULES};
use serde_json::{json, Value};

fn ended(status: &str, error: Option<&str>) -> Outcome {
    Outcome {
        task_id: Some("tsk_1".into()),
        status: Some(status.into()),
        error: error.map(str::to_string),
        result: (status == "done").then(|| "an answer".to_string()),
        ..Outcome::default()
    }
}

fn with_trace(mut o: Outcome, entries: &[(&str, Value)]) -> Outcome {
    o.trace = Some(
        entries
            .iter()
            .map(|(k, b)| TraceEntry {
                at: None,
                kind: (*k).into(),
                body: b.clone(),
            })
            .collect(),
    );
    o
}

fn failing(checks: Vec<Check>) -> Grade {
    Grade { failures: checks }
}

fn wrong() -> Grade {
    failing(vec![Check::MissingText {
        text: "5696".into(),
    }])
}

fn rule_of(o: &Outcome, g: &Grade) -> (Tag, String) {
    let t = tag_failure(o, g);
    (t.tag, t.rule)
}

fn is(tag: Tag, rule: &str) -> (Tag, String) {
    (tag, rule.to_string())
}

#[test]
fn rules_are_unique_and_end_with_model() {
    let ids: HashSet<&str> = RULES.iter().map(|r| r.0).collect();
    assert_eq!(ids.len(), RULES.len());
    assert_eq!(RULES.last().unwrap().1, Tag::Model);
    for tag in Tag::ALL {
        assert!(RULES.iter().any(|r| r.1 == tag), "no rule gives {tag}");
    }
}

#[test]
fn harness_rules() {
    let unreachable = Outcome {
        runtime_error: Some("connection refused".into()),
        ..Outcome::default()
    };
    assert_eq!(
        rule_of(&unreachable, &failing(vec![])),
        is(Tag::Harness, "runtime-unreachable")
    );

    let mut queued = ended("queued", None);
    queued.timed_out = true;
    assert_eq!(
        rule_of(&queued, &wrong()),
        is(Tag::Harness, "never-claimed")
    );

    assert_eq!(
        rule_of(&ended("cancelled", None), &wrong()),
        is(Tag::Harness, "cancelled")
    );

    for e in [
        "interrupted twice by restarts; ask again",
        "lease expired",
        "worker crashed",
        "the model gateway answered 502",
    ] {
        assert_eq!(
            rule_of(&ended("failed", Some(e)), &wrong()),
            is(Tag::Harness, "interrupted"),
            "{e}"
        );
    }

    let mut silent = ended("working", None);
    silent.timed_out = true;
    assert_eq!(
        rule_of(&silent, &wrong()),
        is(Tag::Harness, "silent-timeout")
    );

    let mut empty = ended("done", None);
    empty.result = Some("   ".into());
    assert_eq!(
        rule_of(&empty, &failing(vec![Check::NoAnswer])),
        is(Tag::Harness, "done-without-answer")
    );
}

#[test]
fn a_timeout_while_working_with_progress_is_the_model() {
    let mut o = ended("working", None);
    o.timed_out = true;
    o.events = vec![Event {
        at: Utc::now(),
        kind: "milestone".into(),
        text: Some("Searching the support docs".into()),
    }];
    assert_eq!(rule_of(&o, &wrong()).0, Tag::Model);
}

#[test]
fn policy_rules() {
    let denied = with_trace(
        ended("failed", Some("could not finish")),
        &[("policy_denial", json!({"tool": "slack.post"}))],
    );
    assert_eq!(rule_of(&denied, &wrong()), is(Tag::Policy, "policy-denial"));
    // A denial in the trace counts even when the task otherwise finished.
    let done = with_trace(ended("done", None), &[("policy_denial", json!({}))]);
    assert_eq!(rule_of(&done, &wrong()), is(Tag::Policy, "policy-denial"));
    for e in [
        "tool server answered 403",
        "Write denied: setting is never",
        "blocked by policy",
    ] {
        assert_eq!(
            rule_of(&ended("failed", Some(e)), &wrong()),
            is(Tag::Policy, "policy-error"),
            "{e}"
        );
    }
    // Policy beats harness when an error names both.
    let both = ended("failed", Some("worker: denied by policy"));
    assert_eq!(rule_of(&both, &wrong()).0, Tag::Policy);
}

#[test]
fn memory_rules() {
    let o = ended("failed", Some("Maximum context length exceeded"));
    assert_eq!(rule_of(&o, &wrong()), is(Tag::Memory, "memory-error"));
    let tool = with_trace(
        ended("done", None),
        &[(
            "tool_result",
            json!({"tool": "memory.read", "isError": true}),
        )],
    );
    assert_eq!(
        rule_of(&tool, &wrong()),
        is(Tag::Memory, "memory-tool-error")
    );
}

#[test]
fn tool_rules() {
    for body in [
        json!({"tool": "docs.search", "isError": true, "content": [{"text": "unavailable"}]}),
        json!({"tool": "docs.read", "error": {"code": "not_found"}}),
        json!({"name": "jira.search", "status": 502}),
        json!({"tool": "docs.search", "is_error": true}),
    ] {
        let o = with_trace(ended("done", None), &[("tool_result", body.clone())]);
        assert_eq!(rule_of(&o, &wrong()), is(Tag::Tool, "tool-error"), "{body}");
    }
    let ok_result = with_trace(
        ended("done", None),
        &[("tool_result", json!({"tool": "docs.search", "error": null}))],
    );
    assert_eq!(rule_of(&ok_result, &wrong()).0, Tag::Model);

    let o = ended("failed", Some("the docs MCP server is unavailable"));
    assert_eq!(rule_of(&o, &wrong()), is(Tag::Tool, "tool-failure"));
}

#[test]
fn a_page_that_was_never_retrieved_is_the_tool_and_one_that_was_is_the_model() {
    let page = "https://support.acme.com/docs/what-is-a-join-token";
    let missing = failing(vec![Check::MissingCitation { url: page.into() }]);
    let call = (
        "tool_call",
        json!({"tool": "docs.search", "args": {"query": "join token"}}),
    );

    let other = with_trace(
        ended("done", None),
        &[
            call.clone(),
            (
                "tool_result",
                json!({"tool": "docs.search", "content": [{"url": "https://support.acme.com/docs/other.md"}]}),
            ),
        ],
    );
    assert_eq!(
        rule_of(&other, &missing),
        is(Tag::Tool, "page-not-retrieved")
    );

    // The `.md` form in a tool result is the same page.
    let found = with_trace(
        ended("done", None),
        &[
            call.clone(),
            (
                "tool_result",
                json!({"tool": "docs.read", "content": format!("{page}.md")}),
            ),
        ],
    );
    assert_eq!(rule_of(&found, &missing), is(Tag::Model, "model"));

    // No tool calls at all: the bot never looked, which is the model.
    let none = with_trace(ended("done", None), &[]);
    assert_eq!(rule_of(&none, &missing).0, Tag::Model);

    // No trace: nothing says retrieval failed.
    assert_eq!(rule_of(&ended("done", None), &missing).0, Tag::Model);
}

#[test]
fn everything_else_is_the_model_with_a_reason() {
    let t = tag_failure(&ended("done", None), &wrong());
    assert_eq!(t.tag, Tag::Model);
    assert_eq!(t.reason, "doesn't say \"5696\"");
    let approvals = failing(vec![Check::Approvals {
        expected: 0,
        got: 1,
    }]);
    assert_eq!(
        tag_failure(&ended("needs_approval", None), &approvals).tag,
        Tag::Model
    );
    let other = tag_failure(&ended("failed", Some("I could not answer")), &wrong());
    assert_eq!(
        (other.tag, other.reason.as_str()),
        (Tag::Model, "failed: i could not answer")
    );
}
