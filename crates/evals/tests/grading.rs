//! The grading rules.

use pmos_evals::grade::{extract_urls, grade, normalize_text, normalize_url, Check};
use pmos_evals::outcome::Outcome;
use pmos_evals::spec::{EvalTask, Expect};

const PAGE: &str = "https://support.acme.com/docs/acme-saas-ip-whitelisting";

fn task(expect: Expect) -> EvalTask {
    EvalTask {
        id: "t".into(),
        ask: "q".into(),
        expect,
    }
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
        "https://support.acme.com/docs/acme-saas-ip-whitelisting/",
        "https://support.acme.com/docs/acme-saas-ip-whitelisting.md",
        "https://support.acme.com/docs/acme-saas-ip-whitelisting?utm_source=slack",
        "https://support.acme.com/docs/acme-saas-ip-whitelisting.md?x=1#30-ranges",
        "http://www.support.acme.com/docs/acme-saas-ip-whitelisting",
        "HTTPS://Support.Acme.com/docs/Acme-SaaS-IP-Whitelisting",
        "https://support.acme.com/v1/docs/acme-saas-ip-whitelisting",
    ] {
        assert_eq!(normalize_url(variant), n, "{variant}");
    }
    assert_ne!(
        normalize_url("https://support.acme.com/docs/acme-saas-ip-whitelisting-2"),
        n
    );
    assert_ne!(
        normalize_url("https://support.acme.com/apidocs/acme-saas-ip-whitelisting"),
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
        must_include: vec!["ip-ranges.acme.com".into(), "30 days".into()],
        must_not_include: vec!["<think>".into()],
        approvals: Some(0),
    });
    let answer = format!(
        "Acme announces new IPs at **ip-ranges.acme.com** at least 30\u{a0}days ahead.\n\n[Acme SaaS IP Whitelisting]({PAGE}.md)"
    );
    let g = grade(&t, &done(&answer));
    assert!(g.passed(), "{:?}", g.failures);
}

#[test]
fn each_expectation_is_checked() {
    let t = task(Expect {
        must_cite: vec![
            PAGE.into(),
            "https://support.acme.com/docs/other".into(),
        ],
        must_include: vec!["30 days".into(), "IPv6".into()],
        must_not_include: vec!["<think>".into()],
        approvals: None,
    });
    let g = grade(&t, &done(&format!("<think>hmm</think> 30 days. {PAGE}")));
    assert_eq!(
        g.failures,
        vec![
            Check::MissingCitation {
                url: "https://support.acme.com/docs/other".into()
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
