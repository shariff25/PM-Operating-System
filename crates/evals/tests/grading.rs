//! The grading rules.

use std::collections::BTreeMap;

use pmos_evals::client::Draft;
use pmos_evals::grade::{
    draft_text, extract_urls, grade as grade_with, normalize_text, normalize_url, Check, Grade,
};
use pmos_evals::outcome::Outcome;
use pmos_evals::spec::{DraftCheck, EvalTask, Expect};
use serde_json::json;

const PAGE: &str = "https://docs.example.com/docs/acme-saas-ip-whitelisting";

fn task(expect: Expect) -> EvalTask {
    EvalTask {
        id: "t".into(),
        ask: "q".into(),
        expect,
        ..EvalTask::default()
    }
}

fn grade(t: &EvalTask, o: &Outcome) -> Grade {
    grade_with(t, o, &[])
}

fn done(result: &str) -> Outcome {
    Outcome {
        task_id: Some("tsk_1".into()),
        status: Some("done".into()),
        result: Some(result.into()),
        ..Outcome::default()
    }
}

#[test]
fn citations_match_loosely() {
    let n = normalize_url(PAGE);
    for variant in [
        "https://docs.example.com/docs/acme-saas-ip-whitelisting/",
        "https://docs.example.com/docs/acme-saas-ip-whitelisting.md",
        "https://docs.example.com/docs/acme-saas-ip-whitelisting?utm_source=slack",
        "https://docs.example.com/docs/acme-saas-ip-whitelisting.md?x=1#30-ranges",
        "http://www.docs.example.com/docs/acme-saas-ip-whitelisting",
        "HTTPS://Docs.Example.com/docs/Acme-SaaS-IP-Whitelisting",
        "https://docs.example.com/v1/docs/acme-saas-ip-whitelisting",
    ] {
        assert_eq!(normalize_url(variant), n, "{variant}");
    }
    assert_ne!(
        normalize_url("https://docs.example.com/docs/acme-saas-ip-whitelisting-2"),
        n
    );
    assert_ne!(
        normalize_url("https://docs.example.com/apidocs/acme-saas-ip-whitelisting"),
        n
    );
}

#[test]
fn links_are_found_in_markdown_and_prose() {
    let text =
        "See [IP ranges](https://a.example/x.md), <https://b.example/y>, and https://c.example/z. \
                Also (https://d.example/w) and `https://e.example/v`; not http-ish words or httpx.";
    assert_eq!(
        extract_urls(text),
        vec![
            "https://a.example/x.md",
            "https://b.example/y",
            "https://c.example/z",
            "https://d.example/w",
            "https://e.example/v",
        ]
    );
    assert!(extract_urls("no links here, https:// alone").is_empty());
}

#[test]
fn text_is_compared_loosely() {
    assert_eq!(normalize_text("  **Docker   Hub** "), "docker hub");
    assert_eq!(normalize_text("AMD SEV-SNP"), "amd sev snp");
    assert_eq!(normalize_text("1,800 requests"), "1800 requests");
    assert_eq!(normalize_text("a, b"), "a, b");
    assert_eq!(
        normalize_text("`/crypto/v1/keys/rekey`"),
        "/crypto/v1/keys/rekey"
    );
    assert_eq!(
        normalize_text("on\u{a0}premises — or not"),
        "on premises or not"
    );
}

#[test]
fn a_good_answer_passes() {
    let t = task(Expect {
        must_cite: vec![PAGE.into()],
        must_include: vec!["ip-ranges.example.com".into(), "30 days".into()],
        must_not_include: vec!["<think>".into()],
        approvals: Some(0),
        ..Expect::default()
    });
    let answer = format!(
        "Acme announces new IPs at **ip-ranges.example.com** at least 30\u{a0}days ahead.\n\n[Acme SaaS IP Whitelisting]({PAGE}.md)"
    );
    let g = grade(&t, &done(&answer));
    assert!(g.passed(), "{:?}", g.failures);
}

#[test]
fn each_expectation_is_checked() {
    let t = task(Expect {
        must_cite: vec![PAGE.into(), "https://docs.example.com/docs/other".into()],
        must_include: vec!["30 days".into(), "IPv6".into()],
        must_not_include: vec!["<think>".into()],
        approvals: None,
        ..Expect::default()
    });
    let g = grade(&t, &done(&format!("<think>hmm</think> 30 days. {PAGE}")));
    assert_eq!(
        g.failures,
        vec![
            Check::MissingCitation {
                url: "https://docs.example.com/docs/other".into()
            },
            Check::MissingText {
                text: "IPv6".into()
            },
            Check::ForbiddenText {
                text: "<think>".into()
            },
        ]
    );
}

#[test]
fn status_and_approvals() {
    let base = Expect {
        must_include: vec!["x".into()],
        ..Expect::default()
    };
    let with = |approvals| {
        task(Expect {
            approvals,
            ..base.clone()
        })
    };
    let ended = |status: &str, approvals: usize| Outcome {
        status: Some(status.into()),
        result: Some("x".into()),
        approvals,
        ..Outcome::default()
    };
    // approvals: 0 means done with none.
    assert!(grade(&with(Some(0)), &ended("done", 0)).passed());
    assert_eq!(
        grade(&with(Some(0)), &ended("needs_approval", 1)).failures,
        vec![
            Check::Status {
                expected: "`done`".into(),
                got: "needs_approval".into()
            },
            Check::Approvals {
                expected: 0,
                got: 1
            },
        ]
    );
    // approvals: n > 0 means needs_approval with exactly n.
    assert!(grade(&with(Some(2)), &ended("needs_approval", 2)).passed());
    assert_eq!(
        grade(&with(Some(2)), &ended("needs_approval", 1)).failures,
        vec![Check::Approvals {
            expected: 2,
            got: 1
        }]
    );
    assert!(!grade(&with(Some(1)), &ended("done", 1)).passed());
    // No approvals expectation: done or needs_approval, any count.
    assert!(grade(&with(None), &ended("done", 0)).passed());
    assert!(grade(&with(None), &ended("needs_approval", 3)).passed());
    assert_eq!(
        grade(&with(None), &ended("failed", 0)).failures,
        vec![Check::Status {
            expected: "`done` or `needs_approval`".into(),
            got: "failed".into()
        }]
    );
}

#[test]
fn no_answer_timeouts_and_runtime_errors() {
    let t = task(Expect {
        must_include: vec!["x".into()],
        ..Expect::default()
    });
    assert_eq!(grade(&t, &done("  \n")).failures, vec![Check::NoAnswer]);
    let timed_out = Outcome {
        status: Some("working".into()),
        timed_out: true,
        ..Outcome::default()
    };
    assert_eq!(
        grade(&t, &timed_out).failures,
        vec![Check::TimedOut {
            status: "working".into()
        }]
    );
    let unreachable = Outcome {
        runtime_error: Some("connection refused".into()),
        ..Outcome::default()
    };
    assert_eq!(
        grade(&t, &unreachable).failures,
        vec![Check::Runtime {
            error: "connection refused".into()
        }]
    );
}

fn phrases() -> Vec<String> {
    vec!["don't answer".into(), "couldn't find".into()]
}

#[test]
fn must_decline_needs_a_decline_phrase() {
    let t = task(Expect {
        must_decline: true,
        must_not_include: vec!["<think>".into()],
        approvals: Some(0),
        ..Expect::default()
    });
    // Matched loosely: a curly apostrophe and other case count.
    for answer in [
        "The support docs don\u{2019}t answer that: roadmap dates aren't published.",
        "I COULDN'T FIND anything on that in the support docs.",
    ] {
        let g = grade_with(&t, &done(answer), &phrases());
        assert!(g.passed(), "{answer}: {:?}", g.failures);
    }
    let made_up = grade_with(&t, &done("The next release ships in Q3."), &phrases());
    assert_eq!(made_up.failures, vec![Check::NotDeclined]);
    // With no phrases configured nothing can pass.
    assert_eq!(
        grade_with(&t, &done("The docs don't answer it."), &[]).failures,
        vec![Check::NotDeclined]
    );
    // An empty answer is no answer, not a decline.
    assert_eq!(
        grade_with(&t, &done(""), &phrases()).failures,
        vec![Check::NoAnswer]
    );
}

fn draft(id: &str, tool: &str, args: serde_json::Value) -> Draft {
    Draft {
        id: id.into(),
        tool: tool.into(),
        args,
        status: "pending".into(),
    }
}

fn with_drafts(drafts: Vec<Draft>) -> Outcome {
    Outcome {
        task_id: Some("tsk_1".into()),
        status: Some("needs_approval".into()),
        result: Some("Drafted it; waiting for your approval.".into()),
        approvals: drafts.len(),
        drafts,
        ..Outcome::default()
    }
}

fn jira_check() -> DraftCheck {
    DraftCheck {
        tool: "jira.create_issue".into(),
        require_fields: vec!["project".into(), "summary".into()],
        allowed: BTreeMap::from([
            ("project".to_string(), vec!["PM".to_string()]),
            ("issue_type".to_string(), vec!["Story".to_string()]),
        ]),
        must_include: vec!["SAML".into()],
        must_not_include: vec!["roadmap".into()],
        count: Some(1),
    }
}

fn drafts_task(checks: Vec<DraftCheck>) -> EvalTask {
    task(Expect {
        drafts: checks,
        ..Expect::default()
    })
}

#[test]
fn a_draft_that_meets_every_check_passes() {
    let t = drafts_task(vec![jira_check()]);
    let o = with_drafts(vec![draft(
        "apr_1",
        "jira.create_issue",
        json!({"project": "PM", "issue_type": "Story", "summary": "Require SAML SSO", "epic": "PM-101"}),
    )]);
    let g = grade(&t, &o);
    assert!(g.passed(), "{:?}", g.failures);
}

#[test]
fn draft_count() {
    let t = drafts_task(vec![jira_check()]);
    let one = || {
        draft(
            "apr_1",
            "jira.create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "SAML"}),
        )
    };
    let mut two = with_drafts(vec![one(), one()]);
    two.drafts[1].id = "apr_2".into();
    assert_eq!(
        grade(&t, &two).failures,
        vec![Check::DraftCount {
            tool: "jira.create_issue".into(),
            expected: Some(1),
            got: 2
        }]
    );
    // Other tools' drafts don't count toward this one.
    let other = with_drafts(vec![draft(
        "apr_1",
        "slack.send_message",
        json!({"channel": "C1", "text": "SAML"}),
    )]);
    assert!(grade(&t, &other).failures.contains(&Check::DraftCount {
        tool: "jira.create_issue".into(),
        expected: Some(1),
        got: 0
    }));
    // No count: at least one.
    let mut c = jira_check();
    c.count = None;
    let none = grade(&drafts_task(vec![c.clone()]), &with_drafts(vec![]));
    assert_eq!(
        none.failures,
        vec![Check::DraftCount {
            tool: "jira.create_issue".into(),
            expected: None,
            got: 0
        }]
    );
    assert!(none.failures[0]
        .to_string()
        .contains("expected at least one"));
    assert!(grade(&drafts_task(vec![c]), &two).passed());
    // count: 0 says the tool must not be drafted.
    let never = DraftCheck {
        tool: "slack.send_message".into(),
        count: Some(0),
        ..DraftCheck::default()
    };
    assert!(grade(&drafts_task(vec![never.clone()]), &with_drafts(vec![])).passed());
    assert!(!grade(&drafts_task(vec![never]), &other).passed());
}

#[test]
fn draft_required_fields() {
    let t = drafts_task(vec![jira_check()]);
    for args in [
        json!({"issue_type": "Story", "project": "PM", "summary": "  "}),
        json!({"issue_type": "Story", "project": "PM", "summary": null}),
        json!({"issue_type": "Story", "project": "PM"}),
    ] {
        let mut a = args.clone();
        a["description"] = json!("SAML");
        let g = grade(
            &t,
            &with_drafts(vec![draft("apr_9", "jira.create_issue", a)]),
        );
        assert_eq!(
            g.failures,
            vec![Check::DraftMissingField {
                tool: "jira.create_issue".into(),
                draft: "apr_9".into(),
                field: "summary".into()
            }],
            "{args}"
        );
    }
    // Lists, objects and numbers count when not empty.
    let mut c = jira_check();
    c.require_fields = vec!["labels".into(), "points".into()];
    c.allowed.clear();
    c.must_include.clear();
    let ok = json!({"labels": ["sso"], "points": 3});
    assert!(grade(
        &drafts_task(vec![c.clone()]),
        &with_drafts(vec![draft("a", "jira.create_issue", ok)])
    )
    .passed());
    let empty = json!({"labels": [], "points": 3});
    assert!(!grade(
        &drafts_task(vec![c]),
        &with_drafts(vec![draft("a", "jira.create_issue", empty)])
    )
    .passed());
}

#[test]
fn draft_allowed_values() {
    let t = drafts_task(vec![jira_check()]);
    let g = grade(
        &t,
        &with_drafts(vec![draft(
            "apr_1",
            "jira.create_issue",
            json!({"project": "ENG", "summary": "SAML"}),
        )]),
    );
    assert_eq!(
        g.failures,
        vec![
            Check::DraftNotAllowed {
                tool: "jira.create_issue".into(),
                draft: "apr_1".into(),
                field: "issue_type".into(),
                value: None,
                allowed: vec!["Story".into()],
            },
            Check::DraftNotAllowed {
                tool: "jira.create_issue".into(),
                draft: "apr_1".into(),
                field: "project".into(),
                value: Some("ENG".into()),
                allowed: vec!["PM".into()],
            },
        ]
    );
    assert_eq!(
        g.failures[1].to_string(),
        "`jira.create_issue` draft apr_1: `project` is `ENG`, allowed `PM`"
    );
    assert!(g.failures[0]
        .to_string()
        .contains("`issue_type` is missing"));
}

#[test]
fn draft_text_checks() {
    let t = drafts_task(vec![jira_check()]);
    let g = grade(
        &t,
        &with_drafts(vec![draft(
            "apr_1",
            "jira.create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "SSO", "description": "Post the ROADMAP"}),
        )]),
    );
    assert_eq!(
        g.failures,
        vec![
            Check::DraftMissingText {
                tool: "jira.create_issue".into(),
                draft: "apr_1".into(),
                text: "SAML".into()
            },
            Check::DraftForbiddenText {
                tool: "jira.create_issue".into(),
                draft: "apr_1".into(),
                text: "roadmap".into()
            },
        ]
    );
    // The text is the string arguments, sorted by key; other values are left out.
    assert_eq!(
        draft_text(&json!({"title": "B", "body": "A", "n": 3, "space": "PM"})),
        "A\nPM\nB"
    );
}

/// Source markers are removed before any text check, as the tool server removes them before
/// it writes: a marker never splits a phrase, and a check can't be met by one.
#[test]
fn draft_and_answer_text_is_checked_without_source_markers() {
    assert_eq!(
        draft_text(
            &json!({"summary": "Require SAML [[src-3f9a1c]]", "description": "Per `[[src-3f9a1c]]` [[s2]]."})
        ),
        "Per `[[src-3f9a1c]]`.\nRequire SAML"
    );
    let mut check = jira_check();
    check.must_include = vec!["Require SAML SSO".into()];
    check.must_not_include = vec!["src-0b77d2".into()];
    let t = drafts_task(vec![check]);
    let o = with_drafts(vec![draft(
        "apr_1",
        "jira.create_issue",
        json!({"project": "PM", "issue_type": "Story",
               "summary": "Require SAML [[src-3f9a1c]] SSO [[src-0b77d2]]"}),
    )]);
    let g = grade(&t, &o);
    assert!(g.passed(), "{:?}", g.failures);

    let t = task(Expect {
        must_include: vec!["ships in 4.2.".into()],
        ..Expect::default()
    });
    let g = grade(&t, &done("SSO ships in 4.2 [[src-3f9a1c]]."));
    assert!(g.passed(), "{:?}", g.failures);
}

#[test]
fn unreadable_drafts_fail_the_drafts_checks() {
    let t = drafts_task(vec![jira_check()]);
    let mut o = with_drafts(vec![]);
    o.drafts_error = Some("GET /v1/tasks/tsk_1/drafts: the runtime answered 404".into());
    assert_eq!(
        grade(&t, &o).failures,
        vec![Check::DraftsUnreadable {
            error: "GET /v1/tasks/tsk_1/drafts: the runtime answered 404".into()
        }]
    );
}

#[test]
fn check_keys_identify_deterministic_checks() {
    assert_eq!(
        Check::MissingCitation {
            url: format!("{PAGE}.md")
        }
        .key(),
        Check::MissingCitation { url: PAGE.into() }.key()
    );
    assert_eq!(Check::NotDeclined.key().as_deref(), Some("must_decline"));
    assert_eq!(
        Check::DraftCount {
            tool: "jira.create_issue".into(),
            expected: Some(1),
            got: 0
        }
        .key()
        .as_deref(),
        Some("drafts:jira.create_issue:count")
    );
    assert_eq!(Check::TimedOut { status: "x".into() }.key(), None);
    assert_eq!(Check::Runtime { error: "x".into() }.key(), None);
}
