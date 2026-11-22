//! Repeats, pass^k, the clustered standard error, comparing runs, and the gate's decision.

use chrono::{NaiveDate, TimeZone, Utc};
use pmos_evals::gate::{baseline_from_git, decide, FailReason};
use pmos_evals::grade::Check;
use pmos_evals::judge::{JudgeStatus, JudgeVerdict};
use pmos_evals::report::{Report, TaskReport};
use pmos_evals::stats::{clustered_se, compare, pass_hat_k, report_se, summaries, SeMethod};

fn line(id: &str, repeat: u32, passed: bool, cluster: Option<&str>) -> TaskReport {
    TaskReport {
        id: id.into(),
        ask: format!("{id}?"),
        repeat,
        cluster: cluster.map(str::to_string),
        regression: false,
        turns: Vec::new(),
        failed_turn: None,
        task_id: Some(format!("tsk_{id}_{repeat}")),
        status: Some("done".into()),
        passed,
        failures: if passed {
            vec![]
        } else {
            vec![Check::MissingText {
                text: "5696".into(),
            }]
        },
        tag: None,
        queue_secs: None,
        total_secs: None,
        attempts: 1,
        trace_read: true,
        judges: vec![],
    }
}

/// A report from `(task, cluster, passes of each repeat)`.
fn report(date: &str, tasks: &[(&str, Option<&str>, &[bool])]) -> Report {
    let date: NaiveDate = date.parse().unwrap();
    let at = Utc.from_utc_datetime(&date.and_hms_opt(9, 0, 0).unwrap());
    let repeats = tasks.first().map(|t| t.2.len() as u32).unwrap_or(1);
    Report {
        category: "docs-qa".into(),
        bot: "docs".into(),
        date,
        started_at: at,
        finished_at: at,
        pass_bar: Some(0.8),
        runtime_url: "http://127.0.0.1:8750".into(),
        requested_by: "ana".into(),
        gateway_workarounds: None,
        note: None,
        gateway_models: None,
        persona_version: None,
        persona_hash: None,
        repeats,
        judges: vec![],
        judges_skipped: None,
        isolation: None,
        tasks: tasks
            .iter()
            .flat_map(|(id, c, runs)| {
                runs.iter()
                    .enumerate()
                    .map(|(i, p)| line(id, i as u32 + 1, *p, *c))
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

const ALL: &[bool] = &[true, true, true];
const NONE: &[bool] = &[false, false, false];
const ONE: &[bool] = &[true, false, false];
const TWO: &[bool] = &[true, true, false];

#[test]
fn passes_out_of_k_and_pass_hat_k() {
    let r = report(
        "2026-11-09",
        &[("a", None, ALL), ("b", None, ONE), ("c", None, NONE)],
    );
    let s = summaries(&r);
    let got: Vec<(&str, u32, u32)> = s
        .iter()
        .map(|t| (t.id.as_str(), t.passes, t.runs))
        .collect();
    assert_eq!(got, [("a", 3, 3), ("b", 1, 3), ("c", 0, 3)]);
    assert!((r.pass_rate() - 4.0 / 9.0).abs() < 1e-12);
    assert!((pass_hat_k(&s).unwrap() - 1.0 / 3.0).abs() < 1e-12);
    assert_eq!(pass_hat_k(&[]), None);
}

/// Worked by hand. Clusters A = {a: 3/3, b: 1/3} and B = {c: 0/3}: n = 9, ȳ = 4/9.
/// S_A = 4 − 6·4/9 = 4/3, S_B = 0 − 3·4/9 = −4/3, Σ S² = 32/9.
/// SE = sqrt(2/1 · 32/9) / 9 = (8/3) / 9 = 8/27.
#[test]
fn clustered_standard_error_matches_the_hand_computation() {
    let r = report(
        "2026-11-09",
        &[
            ("a", Some("A"), ALL),
            ("b", Some("A"), ONE),
            ("c", Some("B"), NONE),
        ],
    );
    let se = report_se(&r).unwrap();
    assert_eq!(se.method, SeMethod::Clustered);
    assert_eq!(se.clusters, 2);
    assert!((se.se - 8.0 / 27.0).abs() < 1e-12, "{}", se.se);

    // Without clusters, each task is its own: G = 3, S = (5/3, −1/3, −4/3), Σ S² = 42/9.
    // SE = sqrt(3/2 · 42/9) / 9 = sqrt(7) / 9.
    let r = report(
        "2026-11-09",
        &[("a", None, ALL), ("b", None, ONE), ("c", None, NONE)],
    );
    let se = report_se(&r).unwrap();
    assert_eq!((se.method, se.clusters), (SeMethod::Clustered, 3));
    assert!((se.se - 7f64.sqrt() / 9.0).abs() < 1e-12, "{}", se.se);
}

#[test]
fn one_attempt_per_cluster_falls_back_to_the_binomial() {
    // 3 of 4 pass, each task alone: sqrt(0.75 · 0.25 / 4).
    let se = clustered_se([("a", true), ("b", true), ("c", true), ("d", false)]).unwrap();
    assert_eq!(se.method, SeMethod::Binomial);
    assert!((se.se - (0.75f64 * 0.25 / 4.0).sqrt()).abs() < 1e-12);
    // One cluster: no between-cluster variance to estimate, so binomial too.
    let se = clustered_se([("a", true), ("a", false)]).unwrap();
    assert_eq!(se.method, SeMethod::Binomial);
    assert!((se.se - 0.25f64.sqrt() / 2f64.sqrt()).abs() < 1e-12);
    assert_eq!(clustered_se(std::iter::empty()), None);
}

/// Paired differences d = (0, −2/3, +1/3, −1/3): mean −1/6, sample sd sqrt(5/27),
/// SE = sd / 2, interval −1/6 ± 1.96 · SE.
#[test]
fn compare_pairs_tasks_and_finds_flips() {
    let base = report(
        "2026-11-02",
        &[
            ("a", None, ALL),
            ("b", None, ALL),
            ("c", None, NONE),
            ("d", None, TWO),
            ("gone", None, ALL),
        ],
    );
    let cand = report(
        "2026-11-09",
        &[
            ("a", None, ALL),
            ("b", None, ONE),
            ("c", None, ONE),
            ("d", None, ONE),
            ("new", None, NONE),
        ],
    );
    let c = compare(&base, &cand);
    assert_eq!(c.paired.len(), 4);
    assert_eq!(c.added, ["new"]);
    assert_eq!(c.removed, ["gone"]);
    let mean = c.mean.unwrap();
    assert!((mean + 1.0 / 6.0).abs() < 1e-12, "{mean}");
    let se = (5.0f64 / 27.0).sqrt() / 2.0;
    assert!((c.se.unwrap() - se).abs() < 1e-12);
    let (lo, hi) = c.interval().unwrap();
    assert!((lo - (mean - 1.96 * se)).abs() < 1e-12 && (hi - (mean + 1.96 * se)).abs() < 1e-12);
    assert!(!c.worse(), "the interval straddles 0");
    // No task went from all to none or back.
    assert_eq!(c.broke().count(), 0);
    assert_eq!(c.fixed().count(), 0);

    let flipped = report(
        "2026-11-09",
        &[
            ("a", None, NONE),
            ("b", None, ALL),
            ("c", None, ALL),
            ("d", None, TWO),
        ],
    );
    let c = compare(&base, &flipped);
    let broke: Vec<&str> = c.broke().map(|p| p.id.as_str()).collect();
    let fixed: Vec<&str> = c.fixed().map(|p| p.id.as_str()).collect();
    assert_eq!((broke, fixed), (vec!["a"], vec!["c"]));
    let md = c.to_markdown();
    assert!(md.contains("- **Flipped to failing:** `a`"), "{md}");
    assert!(md.contains("- **Flipped to passing:** `c`"), "{md}");
    assert!(md.contains("| `a` | 3/3 | 0/3 | -100.0 pts |"), "{md}");
    assert!(md.contains("- **Tasks no longer run:** `gone`"), "{md}");
}

#[test]
fn different_repeat_counts_compare_by_rate() {
    let base = report("2026-11-02", &[("a", None, &[true]), ("b", None, &[false])]);
    let cand = report("2026-11-09", &[("a", None, TWO), ("b", None, ONE)]);
    let c = compare(&base, &cand);
    // a: 2/3 − 1 = −1/3; b: 1/3 − 0 = +1/3.
    assert!(c.mean.unwrap().abs() < 1e-12);
    assert_eq!(c.broke().count() + c.fixed().count(), 0);
}

fn named(r: Report) -> Option<(String, Report)> {
    Some(("2026-11-02-docs-qa.json".into(), r))
}

#[test]
fn the_gate_passes_an_unchanged_run_and_one_with_no_baseline() {
    let tasks: &[(&str, Option<&str>, &[bool])] =
        &[("a", None, ALL), ("b", None, TWO), ("c", None, NONE)];
    let r = decide(
        named(report("2026-11-02", tasks)),
        "`origin/main`",
        report("2026-11-09", tasks),
        "2026-11-09-docs-qa.json",
    );
    assert!(r.passed(), "{:?}", r.reasons);
    let md = r.to_markdown();
    assert!(md.starts_with("## Eval gate: `docs-qa` passed\n"), "{md}");
    assert!(md.contains("No gate condition failed."), "{md}");
    assert!(
        md.contains("Baseline: `2026-11-02-docs-qa.json` from `origin/main`"),
        "{md}"
    );

    let r = decide(None, "`origin/main`", report("2026-11-09", tasks), "x.json");
    assert!(r.passed());
    assert!(
        r.to_markdown().contains("No baseline"),
        "{}",
        r.to_markdown()
    );
}

#[test]
fn the_gate_fails_when_a_regression_task_flips() {
    let base = report(
        "2026-11-02",
        &[("a", None, ALL), ("b", None, ALL), ("c", None, ALL)],
    );
    let mut cand = report(
        "2026-11-09",
        &[("a", None, NONE), ("b", None, ALL), ("c", None, ALL)],
    );
    // Not marked: a flip alone, with an interval that straddles 0, passes.
    let r = decide(named(base.clone()), "b", cand.clone(), "c");
    assert!(
        !r.reasons
            .iter()
            .any(|x| matches!(x, FailReason::RegressionFlipped { .. })),
        "{:?}",
        r.reasons
    );
    for l in cand.tasks.iter_mut().filter(|l| l.id == "a") {
        l.regression = true;
    }
    let r = decide(named(base), "b", cand, "c");
    assert!(r
        .reasons
        .contains(&FailReason::RegressionFlipped { task: "a".into() }));
    assert!(r
        .to_markdown()
        .contains("regression task `a` flipped to failing"));
}

#[test]
fn the_gate_fails_when_the_interval_is_below_zero() {
    let tasks: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
    let base_t: Vec<(&str, Option<&str>, &[bool])> =
        tasks.iter().map(|t| (t.as_str(), None, ALL)).collect();
    // Every task drops by one run in three: d = −1/3 everywhere, SE 0.
    let cand_t: Vec<(&str, Option<&str>, &[bool])> =
        tasks.iter().map(|t| (t.as_str(), None, TWO)).collect();
    let r = decide(
        named(report("2026-11-02", &base_t)),
        "b",
        report("2026-11-09", &cand_t),
        "c",
    );
    assert!(
        r.reasons
            .iter()
            .any(|x| matches!(x, FailReason::Worse { hi, .. } if *hi < 0.0)),
        "{:?}",
        r.reasons
    );
    assert!(r.to_markdown().contains("wholly below 0"));
}

#[test]
fn the_gate_fails_when_a_check_that_passed_now_fails() {
    // `a` passed all three before; now it fails every run on a new check, though the pass
    // rate overall barely moves and no task flips (it already failed one check).
    let mut base = report("2026-11-02", &[("a", None, ONE), ("b", None, ALL)]);
    let mut cand = report("2026-11-09", &[("a", None, ONE), ("b", None, ALL)]);
    let cite = Check::MissingCitation {
        url: "https://docs.example.com/docs/x".into(),
    };
    for l in cand.tasks.iter_mut().filter(|l| l.id == "a") {
        l.passed = false;
        l.failures.push(cite.clone());
    }
    let r = decide(named(base.clone()), "b", cand.clone(), "c");
    assert!(
        r.reasons.contains(&FailReason::CheckFlipped {
            task: "a".into(),
            check: "must_cite:docs.example.com/docs/x".into(),
        }),
        "{:?}",
        r.reasons
    );
    // The check that already failed in the baseline (must_include 5696) isn't a flip.
    assert!(!r.reasons.iter().any(
        |x| matches!(x, FailReason::CheckFlipped { check, .. } if check.starts_with("must_include"))
    ));

    // Failing in only some candidate runs isn't a flip.
    let mut some = cand.clone();
    let first = some.tasks.iter_mut().find(|l| l.id == "a").unwrap();
    first.failures.retain(|c| c != &cite);
    let r = decide(named(base.clone()), "b", some, "c");
    assert!(!r
        .reasons
        .iter()
        .any(|x| matches!(x, FailReason::CheckFlipped { .. })));

    // A baseline run cut short (timed out) never says the check passed.
    base.tasks[0].failures = vec![Check::TimedOut {
        status: "working".into(),
    }];
    let r = decide(named(base), "b", cand, "c");
    assert!(!r
        .reasons
        .iter()
        .any(|x| matches!(x, FailReason::CheckFlipped { .. })));
}

fn judged(mut r: Report, task: &str, verdict: bool, blocking: bool) -> Report {
    r.judges = vec![JudgeStatus {
        name: "faithful".into(),
        blocking,
        reason: if blocking {
            "blocking"
        } else {
            "advisory: not calibrated"
        }
        .into(),
    }];
    for l in &mut r.tasks {
        l.judges = vec![JudgeVerdict {
            name: "faithful".into(),
            verdict: Some(if l.id == task { verdict } else { true }),
            blocking,
            unknown: false,
            error: None,
        }];
    }
    r
}

#[test]
fn a_blocking_judge_fails_the_gate_and_an_advisory_one_never_does() {
    let tasks: &[(&str, Option<&str>, &[bool])] = &[("a", None, ALL), ("b", None, ALL)];
    let base = judged(report("2026-11-02", tasks), "a", true, false);
    let cand = judged(report("2026-11-09", tasks), "a", false, true);
    let r = decide(named(base.clone()), "b", cand, "c");
    assert_eq!(
        r.reasons,
        vec![FailReason::JudgeFlipped {
            task: "a".into(),
            judge: "faithful".into()
        }]
    );
    let advisory = judged(report("2026-11-09", tasks), "a", false, false);
    let r = decide(named(base), "b", advisory, "c");
    assert!(r.passed(), "{:?}", r.reasons);
    let md = r.to_markdown();
    assert!(
        md.contains("- `faithful`: advisory: not calibrated; said no 3 time(s)."),
        "{md}"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[test]
fn the_baseline_comes_from_the_reports_at_a_git_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "evals@example.invalid"]);
    git(repo, &["config", "user.name", "evals"]);
    let reports = repo.join("evals/reports");
    std::fs::create_dir_all(&reports).unwrap();
    let old = report("2026-11-02", &[("a", None, ALL)]);
    let new = report("2026-11-09", &[("a", None, TWO)]);
    let mut other = report("2026-11-10", &[("a", None, ALL)]);
    other.category = "docs-qa-extra".into();
    pmos_evals::report::write(&reports, &old).unwrap();
    pmos_evals::report::write(&reports, &new).unwrap();
    pmos_evals::report::write(&reports, &other).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "reports"]);
    // A report added after the ref isn't the baseline.
    pmos_evals::report::write(&reports, &report("2026-11-12", &[("a", None, NONE)])).unwrap();

    let (name, got) = baseline_from_git(&reports, "main", "docs-qa")
        .unwrap()
        .unwrap();
    assert_eq!(name, "2026-11-09-docs-qa.json");
    assert_eq!(got, new);
    assert_eq!(
        baseline_from_git(&reports, "main", "slack-reply").unwrap(),
        None
    );
    let err = baseline_from_git(&reports, "origin/main", "docs-qa").unwrap_err();
    assert!(err.contains("git ref `origin/main` not found"), "{err}");
}
