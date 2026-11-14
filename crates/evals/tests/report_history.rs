//! Report rendering, the JSON round trip, and the two-weeks rule.

use chrono::{NaiveDate, TimeZone, Utc};
use pmos_evals::grade::Check;
use pmos_evals::history::{evaluate, load_reports};
use pmos_evals::report::{meets, write, Report, TaskReport};
use pmos_evals::tag::{Tag, Tagged};

fn line(id: &str, failure: Option<(Tag, &str, Check)>) -> TaskReport {
    let passed = failure.is_none();
    let (tag, failures) = match failure {
        Some((tag, rule, check)) => (
            Some(Tagged {
                tag,
                rule: rule.into(),
                reason: format!("because | {rule}"),
            }),
            vec![check],
        ),
        None => (None, vec![]),
    };
    TaskReport {
        id: id.into(),
        ask: format!("Question {id}?"),
        task_id: Some(format!("tsk_{id}")),
        status: Some(if passed { "done" } else { "failed" }.into()),
        passed,
        failures,
        tag,
        queue_secs: Some(1.3),
        total_secs: Some(12.5),
        attempts: 1,
        trace_read: true,
    }
}

fn report(date: &str, passed: usize, total: usize) -> Report {
    let date: NaiveDate = date.parse().unwrap();
    let at = Utc.from_utc_datetime(&date.and_hms_opt(9, 0, 0).unwrap());
    let tasks = (0..total)
        .map(|i| {
            let id = format!("t{i:02}");
            if i < passed {
                line(&id, None)
            } else {
                line(
                    &id,
                    Some((Tag::Model, "model", Check::MissingText { text: "x".into() })),
                )
            }
        })
        .collect();
    Report {
        category: "docs-qa".into(),
        bot: "docs".into(),
        date,
        started_at: at,
        finished_at: at + chrono::Duration::minutes(3),
        pass_bar: 0.8,
        runtime_url: "http://127.0.0.1:8750".into(),
        requested_by: "ana".into(),
        gateway_workarounds: None,
        note: None,
        tasks,
    }
}

#[test]
fn the_bar_is_inclusive() {
    assert!(meets(16, 20, 0.8));
    assert!(!meets(15, 20, 0.8));
    assert!(meets(7, 10, 0.7));
    assert!(meets(0, 3, 0.0));
    assert!(!meets(0, 0, 0.0));
    assert!(meets(3, 3, 1.0));
}

#[test]
fn markdown_report_puts_the_top_bottleneck_first() {
    let mut r = report("2026-11-09", 1, 1);
    r.note = Some("model swap | qwen".into());
    r.tasks.push(line(
        "tool-a",
        Some((
            Tag::Tool,
            "page-not-retrieved",
            Check::MissingCitation {
                url: "https://support.acme.com/docs/a".into(),
            },
        )),
    ));
    r.tasks.push(line(
        "tool-b",
        Some((
            Tag::Tool,
            "tool-error",
            Check::MissingText {
                text: "9092".into(),
            },
        )),
    ));
    r.tasks.push(line(
        "harness-a",
        Some((
            Tag::Harness,
            "interrupted",
            Check::Status {
                expected: "`done`".into(),
                got: "failed".into(),
            },
        )),
    ));
    let mut unread = line(
        "model-a",
        Some((
            Tag::Model,
            "model",
            Check::ForbiddenText {
                text: "<think>".into(),
            },
        )),
    );
    unread.trace_read = false;
    r.tasks.push(unread);
    let md = r.to_markdown();

    assert!(
        md.starts_with("# Benchmark: docs-qa (2026-11-09)\n"),
        "{md}"
    );
    assert!(
        md.contains("**Below the bar.** 1 of 5 tasks passed (20%), against a bar of 80%."),
        "{md}"
    );
    assert!(md.contains("| Note | model swap \\| qwen |"), "{md}");
    assert!(md.contains("| Gateway workarounds | not declared |"));
    assert!(md.contains("| Traces | not readable for 1 failure(s)"));
    assert!(
        md.contains("Top bottleneck: **tool** (2 of 4 failures)."),
        "{md}"
    );
    // Tags by count, then in the fixed order model, tool, policy, memory, harness.
    let tool = md
        .find("| tool | 2 | `tool-a`, `tool-b` |")
        .expect("tool row");
    let model = md.find("| model | 1 | `model-a` |").expect("model row");
    let harness = md
        .find("| harness | 1 | `harness-a` |")
        .expect("harness row");
    assert!(tool < model && model < harness, "{md}");
    assert!(
        md.contains("| `t00` | pass |  | done | 1.3 | 12.5 |  |"),
        "{md}"
    );
    assert!(md.contains("| `tool-a` | **FAIL** | tool (`page-not-retrieved`) | failed | 1.3 | 12.5 | doesn't cite https://support.acme.com/docs/a |"), "{md}");
    assert!(md.contains("### `harness-a`"));
    assert!(
        md.contains("- **Tag:** harness by rule `interrupted`: because \\| interrupted"),
        "{md}"
    );
    assert!(md.contains("- says \"<think>\""));
}

#[test]
fn a_clean_run_says_so() {
    let mut r = report("2026-11-09", 3, 3);
    r.gateway_workarounds = Some("off".into());
    let md = r.to_markdown();
    assert!(md.contains("**Met the bar.** 3 of 3 tasks passed (100%)"));
    assert!(md.contains("No failures."));
    assert!(!md.contains("## Failure details"));
}

#[test]
fn reports_round_trip_through_json_and_load_by_category() {
    let dir = tempfile::tempdir().unwrap();
    let a = report("2026-11-02", 16, 20);
    let b = report("2026-11-09", 18, 20);
    let mut other = report("2026-11-09", 1, 2);
    other.category = "other".into();
    write(dir.path(), &b).unwrap();
    write(dir.path(), &a).unwrap();
    write(dir.path(), &other).unwrap();
    let loaded = load_reports(dir.path(), "docs-qa").unwrap();
    assert_eq!(loaded, vec![a, b]);
    assert!(load_reports(&dir.path().join("missing"), "docs-qa")
        .unwrap()
        .is_empty());

    std::fs::write(dir.path().join("broken.json"), "{}").unwrap();
    let err = load_reports(dir.path(), "docs-qa").unwrap_err();
    assert!(
        err.contains("broken.json") && err.contains("not a pmos-evals report"),
        "{err}"
    );
}

#[test]
fn two_consecutive_weeks_meet_the_rule() {
    let h = evaluate(
        "docs-qa",
        0.8,
        &[report("2026-11-03", 16, 20), report("2026-11-12", 17, 20)],
    );
    assert!(h.two_weeks_met);
    assert_eq!(h.streak, 2);
    assert!(h
        .to_text()
        .starts_with("docs-qa: met its bar two weeks in a row (bar 80%, streak 2 weeks)."));
    assert!(
        h.to_text().contains("2026-W46: met   2026-11-12 17/20"),
        "{}",
        h.to_text()
    );
}

#[test]
fn one_week_is_not_enough() {
    let h = evaluate(
        "docs-qa",
        0.8,
        &[report("2026-11-09", 20, 20), report("2026-11-13", 20, 20)],
    );
    assert!(!h.two_weeks_met);
    assert_eq!(h.streak, 1);
    assert!(evaluate("docs-qa", 0.8, &[])
        .to_text()
        .contains("No reports yet."));
}

#[test]
fn a_gap_week_breaks_the_streak() {
    let h = evaluate(
        "docs-qa",
        0.8,
        &[report("2026-10-26", 20, 20), report("2026-11-09", 20, 20)],
    );
    assert!(!h.two_weeks_met);
    assert_eq!(h.streak, 1);
}

#[test]
fn one_run_below_the_bar_fails_its_week() {
    // A rerun after a model change fell below the bar in the latest week.
    let h = evaluate(
        "docs-qa",
        0.8,
        &[
            report("2026-11-02", 18, 20),
            report("2026-11-09", 18, 20),
            report("2026-11-11", 12, 20),
        ],
    );
    assert!(!h.two_weeks_met);
    assert_eq!(h.streak, 0);
    assert!(h.to_text().contains("2026-11-11 12/20 (below)"));
    // A failing week earlier on doesn't matter once two later weeks pass.
    let h = evaluate(
        "docs-qa",
        0.8,
        &[
            report("2026-10-26", 2, 20),
            report("2026-11-02", 18, 20),
            report("2026-11-09", 18, 20),
        ],
    );
    assert!(h.two_weeks_met);
    assert_eq!(h.streak, 2);
}

#[test]
fn the_current_bar_is_applied_to_old_reports() {
    let reports = [report("2026-11-02", 15, 20), report("2026-11-09", 15, 20)];
    assert!(!evaluate("docs-qa", 0.8, &reports).two_weeks_met);
    assert!(evaluate("docs-qa", 0.75, &reports).two_weeks_met);
}

#[test]
fn weeks_are_consecutive_across_a_year_end() {
    // 2026 has an ISO week 53: Monday 28 December. The next week is 2027-W01.
    let h = evaluate(
        "docs-qa",
        0.8,
        &[report("2026-12-29", 20, 20), report("2027-01-05", 20, 20)],
    );
    assert!(h.two_weeks_met);
    let h = evaluate(
        "docs-qa",
        0.8,
        &[report("2026-12-21", 20, 20), report("2027-01-05", 20, 20)],
    );
    assert!(!h.two_weeks_met);
}
