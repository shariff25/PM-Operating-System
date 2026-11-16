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
                sub: None,
            }),
            vec![check],
        ),
        None => (None, vec![]),
    };
    TaskReport {
        id: id.into(),
        ask: format!("Question {id}?"),
        repeat: 1,
        cluster: None,
        regression: false,
        turns: Vec::new(),
        failed_turn: None,
        judges: Vec::new(),
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
        pass_bar: Some(0.8),
        runtime_url: "http://127.0.0.1:8750".into(),
        requested_by: "ana".into(),
        gateway_workarounds: None,
        note: None,
        repeats: 1,
        judges: Vec::new(),
        judges_skipped: None,
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
                url: "https://docs.example.com/docs/a".into(),
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
    assert!(md.contains("| `tool-a` | **FAIL** | tool (`page-not-retrieved`) | failed | 1.3 | 12.5 | doesn't cite https://docs.example.com/docs/a |"), "{md}");
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

/// A report as `pmos-evals run` wrote it before repeats, sub-tags, judges and optional bars.
const OLD_REPORT: &str = r#"{
  "category": "docs-qa",
  "bot": "docs",
  "date": "2026-11-09",
  "started_at": "2026-11-09T09:00:00Z",
  "finished_at": "2026-11-09T09:03:00Z",
  "pass_bar": 0.8,
  "runtime_url": "http://127.0.0.1:8750",
  "requested_by": "ana",
  "gateway_workarounds": null,
  "note": "weekly",
  "tasks": [
    {
      "id": "vault-port-requirements",
      "ask": "Which ports?",
      "task_id": "tsk_1",
      "status": "done",
      "passed": true,
      "failures": [],
      "tag": null,
      "queue_secs": 1.0,
      "total_secs": 9.0,
      "attempts": 1,
      "trace_read": false
    },
    {
      "id": "cloud-join-token",
      "ask": "Where is the join token?",
      "task_id": "tsk_2",
      "status": "done",
      "passed": false,
      "failures": [{"check": "missing_text", "text": "join token"}],
      "tag": {"tag": "model", "rule": "model", "reason": "doesn't say \"join token\""},
      "queue_secs": 1.0,
      "total_secs": 9.0,
      "attempts": 1,
      "trace_read": true
    }
  ]
}"#;

#[test]
fn old_single_repeat_reports_still_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("2026-11-09-docs-qa.json"), OLD_REPORT).unwrap();
    let r = load_reports(dir.path(), "docs-qa").unwrap().remove(0);
    assert_eq!(r.repeats, 1);
    assert_eq!(r.pass_bar, Some(0.8));
    assert!(r.judges.is_empty());
    assert_eq!(r.tasks[1].repeat, 1);
    assert_eq!(r.tasks[1].cluster, None);
    assert!(!r.tasks[1].regression);
    assert_eq!(r.tasks[1].tag.as_ref().unwrap().sub, None);
    assert_eq!(r.met_bar(), Some(false));
    // Its numbers, comparison and markdown work as for a new report.
    let s = pmos_evals::stats::summaries(&r);
    assert_eq!((s[0].passes, s[0].runs, s[1].passes), (1, 1, 0));
    assert_eq!(pmos_evals::stats::pass_hat_k(&s), Some(0.5));
    let md = r.to_markdown();
    assert!(
        md.contains("**Below the bar.** 1 of 2 tasks passed (50%)"),
        "{md}"
    );
    assert!(
        md.contains("| `cloud-join-token` | **FAIL** | model (`model`) |"),
        "{md}"
    );
    assert!(!md.contains("pass^"), "{md}");
    let read = pmos_evals::report::read(&dir.path().join("2026-11-09-docs-qa.md")).unwrap();
    assert_eq!(read, r);
}

#[test]
fn reports_show_sub_tags_and_reports_only_categories() {
    let mut r = report("2026-11-09", 1, 3);
    r.pass_bar = None;
    for (i, sub) in ["model/missing-fact", "model/missing-fact"]
        .iter()
        .enumerate()
    {
        r.tasks[i + 1].tag.as_mut().unwrap().sub = Some(sub.to_string());
    }
    let md = r.to_markdown();
    assert!(
        md.contains(
            "**Reports only.** 1 of 3 tasks passed (33%); the category has no pass bar yet."
        ),
        "{md}"
    );
    assert!(
        md.contains("| `model/missing-fact` | 2 | `t01`, `t02` |"),
        "{md}"
    );
    assert!(
        md.contains("| `t01` | **FAIL** | model/missing-fact (`model`) |"),
        "{md}"
    );
    assert_eq!(r.met_bar(), None);
}

#[test]
fn repeats_show_passes_out_of_k_and_pass_hat_k() {
    let mut r = report("2026-11-09", 0, 0);
    r.repeats = 3;
    // t-a passes 3/3, t-b 1/3.
    for (id, passes) in [("t-a", 3), ("t-b", 1)] {
        for k in 1..=3u32 {
            let mut l = if k <= passes {
                line(id, None)
            } else {
                line(
                    id,
                    Some((Tag::Model, "model", Check::MissingText { text: "x".into() })),
                )
            };
            l.repeat = k;
            r.tasks.push(l);
        }
    }
    assert_eq!((r.passed(), r.total()), (4, 6));
    let md = r.to_markdown();
    assert!(
        md.contains(
            "4 of 6 attempts passed (67%) over 2 tasks run 3 times each, against a bar of 80%."
        ),
        "{md}"
    );
    assert!(
        md.contains("| pass^3 | 50% (1 of 2 tasks passed every run) |"),
        "{md}"
    );
    assert!(md.contains("| `t-a` | 3/3 |"), "{md}");
    assert!(md.contains("| `t-b` | **1/3** | model ×2 |"), "{md}");
    assert!(md.contains("### `t-b`, run 3 of 3"), "{md}");
    assert!(md.contains("| Pass rate | 66.7% ± "), "{md}");
}
