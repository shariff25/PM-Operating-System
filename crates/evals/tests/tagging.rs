//! The failure-tagging rules: each rule, and their order.

use std::collections::HashSet;

use chrono::Utc;
use pmos_evals::client::{Event, TraceEntry};
use pmos_evals::grade::{Check, Grade};
use pmos_evals::outcome::Outcome;
use pmos_evals::tag::{tag_failure, Tag, RULES, SUB_TAGS};
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

fn sub_of(o: &Outcome, g: &Grade) -> String {
    tag_failure(o, g).sub.expect("every failure gets a sub-tag")
}

#[test]
fn model_failures_get_a_sub_tag_from_what_failed() {
    let done = ended("done", None);
    let cases: Vec<(Check, &str)> = vec![
        (Check::NotDeclined, "model/answered-beyond-sources"),
        (
            Check::MissingCitation {
                url: "https://support.acme.com/docs/x".into(),
            },
            "model/missing-citation",
        ),
        (
            Check::ForbiddenText {
                text: "<think>".into(),
            },
            "model/leaked-reasoning",
        ),
        (
            Check::MissingText { text: "x".into() },
            "model/missing-fact",
        ),
        (
            Check::ForbiddenText { text: "x".into() },
            "model/forbidden-text",
        ),
        (
            Check::Approvals {
                expected: 1,
                got: 2,
            },
            "model/wrong-draft-count",
        ),
        (
            Check::DraftCount {
                tool: "jira.create_issue".into(),
                expected: Some(1),
                got: 0,
            },
            "model/wrong-draft-count",
        ),
        (
            Check::DraftMissingField {
                tool: "jira.create_issue".into(),
                draft: "a".into(),
                field: "summary".into(),
            },
            "model/incomplete-draft",
        ),
        (
            Check::DraftNotAllowed {
                tool: "jira.create_issue".into(),
                draft: "a".into(),
                field: "issue_type".into(),
                value: Some("Saga".into()),
                allowed: vec!["Story".into()],
            },
            "model/wrong-draft-value",
        ),
        (
            Check::DraftMissingText {
                tool: "jira.create_issue".into(),
                draft: "a".into(),
                text: "SAML".into(),
            },
            "model/draft-content",
        ),
        (Check::NoAnswer, "model/no-answer"),
    ];
    for (check, sub) in cases {
        let g = failing(vec![check.clone()]);
        assert_eq!(rule_of(&done, &g), is(Tag::Model, "model"), "{check:?}");
        assert_eq!(sub_of(&done, &g), sub, "{check:?}");
    }
    // Declining comes first: it says most about what went wrong.
    let both = failing(vec![
        Check::MissingText { text: "x".into() },
        Check::NotDeclined,
    ]);
    assert_eq!(sub_of(&done, &both), "model/answered-beyond-sources");
    assert_eq!(
        sub_of(&ended("failed", Some("no idea")), &wrong()),
        "model/failed"
    );
    let slow = Outcome {
        status: Some("working".into()),
        timed_out: true,
        events: vec![Event {
            at: Utc::now(),
            kind: "milestone".into(),
            text: None,
        }],
        ..Outcome::default()
    };
    assert_eq!(sub_of(&slow, &wrong()), "model/timed-out");
}

#[test]
fn a_draft_in_the_wrong_place_is_a_tool_failure() {
    let wrong_place = |tool: &str, field: &str| {
        failing(vec![Check::DraftNotAllowed {
            tool: tool.into(),
            draft: "apr_1".into(),
            field: field.into(),
            value: Some("ENG".into()),
            allowed: vec!["PM".into()],
        }])
    };
    let o = ended("needs_approval", None);
    for (tool, field, sub) in [
        (
            "confluence.create_draft",
            "space",
            "tool/wrong-confluence-space",
        ),
        ("jira.create_issue", "project", "tool/wrong-jira-project"),
        ("slack.post_reply", "channel", "tool/wrong-slack-channel"),
        ("slack.post_reply", "thread_ts", "tool/wrong-slack-channel"),
    ] {
        let g = wrong_place(tool, field);
        assert_eq!(rule_of(&o, &g), is(Tag::Tool, "draft-wrong-target"));
        assert_eq!(sub_of(&o, &g), sub);
    }
    // A wrong value that isn't where the draft goes is the model's.
    assert_eq!(
        rule_of(&o, &wrong_place("jira.create_issue", "epic")),
        is(Tag::Model, "model")
    );
}

#[test]
fn harness_rules_for_drafts_and_approvals() {
    let unreadable = failing(vec![Check::DraftsUnreadable {
        error: "404".into(),
    }]);
    let o = ended("needs_approval", None);
    assert_eq!(
        rule_of(&o, &unreadable),
        is(Tag::Harness, "drafts-unreadable")
    );
    assert_eq!(sub_of(&o, &unreadable), "harness/drafts-unreadable");

    // Extra approval items after a retry are the retry's, not the model's.
    let extra = failing(vec![Check::Approvals {
        expected: 1,
        got: 2,
    }]);
    let mut retried = ended("needs_approval", None);
    retried.attempts = 2;
    assert_eq!(
        rule_of(&retried, &extra),
        is(Tag::Harness, "duplicate-approvals")
    );
    assert_eq!(sub_of(&retried, &extra), "harness/approval-count");
    // One attempt, or too few items: the model drafted the wrong number.
    assert_eq!(rule_of(&o, &extra).0, Tag::Model);
    let fewer = failing(vec![Check::Approvals {
        expected: 2,
        got: 1,
    }]);
    assert_eq!(rule_of(&retried, &fewer).0, Tag::Model);
}

#[test]
fn every_rule_but_model_and_wrong_target_gives_its_sub_tag() {
    let queued = Outcome {
        status: Some("queued".into()),
        timed_out: true,
        ..Outcome::default()
    };
    let t = tag_failure(&queued, &failing(vec![]));
    assert_eq!(t.sub.as_deref(), Some("harness/never-claimed"));
    let denied = with_trace(
        ended("failed", Some("x")),
        &[("policy_denial", json!({"tool": "slack.post"}))],
    );
    assert_eq!(
        tag_failure(&denied, &wrong()).sub.as_deref(),
        Some("policy/denied-in-trace")
    );
    let memory = ended("failed", Some("maximum context length exceeded"));
    assert_eq!(
        tag_failure(&memory, &wrong()).sub.as_deref(),
        Some("memory/context-limit")
    );
}

#[test]
fn every_emitted_sub_tag_is_listed_and_under_its_rule_tag() {
    let listed: HashSet<&str> = SUB_TAGS.iter().copied().collect();
    assert_eq!(listed.len(), SUB_TAGS.len(), "a sub-tag listed twice");
    for sub in SUB_TAGS {
        let top = sub.split('/').next().unwrap();
        assert!(Tag::ALL.iter().any(|t| t.as_str() == top), "{sub}");
    }
}
