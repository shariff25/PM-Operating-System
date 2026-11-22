//! Report rendering, the JSON round trip, and the two-weeks rule.

use chrono::{NaiveDate, TimeZone, Utc};
use pmos_evals::grade::Check;
use pmos_evals::history::{evaluate, load_reports};
use pmos_evals::report::{meets, write, GatewayModels, Report, TaskReport};
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
        gateway_models: None,
        persona_version: None,
        repeats: 1,
        judges: Vec::new(),
        judges_skipped: None,
        isolation: None,
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
        md.contains("| All attempts passed (pass^3) | 50% (1 of 2 tasks passed every run) |"),
        "{md}"
    );
    assert!(md.contains("| `t-a` | 3/3 | yes |"), "{md}");
    assert!(md.contains("| `t-b` | **1/3** | no | model ×2 |"), "{md}");
    assert!(md.contains("### `t-b`, run 3 of 3"), "{md}");
    assert!(
        md.contains("| Pass rate (average over attempts) | 66.7% ± "),
        "{md}"
    );
}

/// The all-attempts-pass rate beside the average, per task and per category, in the markdown
/// and the `.json`; and attempts lost to the provider or infrastructure shown apart in the
/// headline, while the bar still counts them.
#[test]
fn summary_has_the_all_attempts_rate_and_the_infra_losses_apart() {
    let mut r = report("2026-11-22", 0, 0);
    r.repeats = 3;
    let interrupted = || {
        let mut l = line(
            "t-b",
            Some((
                Tag::Harness,
                "interrupted",
                Check::Status {
                    expected: "`done`".into(),
                    got: "failed".into(),
                },
            )),
        );
        l.tag.as_mut().unwrap().sub = Some("harness/interrupted".into());
        l
    };
    // t-a: 3/3. t-b: 2 passes, 1 lost to the provider. t-c: 1 pass, 2 the bot's failures.
    let mut push = |mut l: TaskReport, k: u32| {
        l.repeat = k;
        r.tasks.push(l);
    };
    for k in 1..=3 {
        push(line("t-a", None), k);
    }
    push(line("t-b", None), 1);
    push(interrupted(), 2);
    push(line("t-b", None), 3);
    push(line("t-c", None), 1);
    for k in 2..=3 {
        push(
            line(
                "t-c",
                Some((Tag::Model, "model", Check::MissingText { text: "x".into() })),
            ),
            k,
        );
    }
    let s = r.summary();
    assert_eq!((s.attempts, s.passed), (9, 6));
    assert!((s.pass_rate - 6.0 / 9.0).abs() < 1e-9);
    assert_eq!((s.tasks, s.tasks_all_attempts_passed), (3, 1));
    assert!((s.all_attempts_pass_rate - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(s.infra_losses, 1);
    assert!((s.pass_rate_excluding_infra.unwrap() - 6.0 / 8.0).abs() < 1e-9);
    // The bar (80%) is judged on every attempt, losses included: 6 of 9 is below it, though
    // 6 of 8 would be closer.
    assert_eq!(s.met_bar, Some(false));
    assert_eq!(r.met_bar(), Some(false));
    let b = &s.by_task[1];
    assert_eq!(
        (
            b.id.as_str(),
            b.passes,
            b.attempts,
            b.all_attempts_passed,
            b.infra_losses
        ),
        ("t-b", 2, 3, false, 1)
    );
    assert!(s.by_task[0].all_attempts_passed);

    let md = r.to_markdown();
    assert!(
        md.contains("**All attempts passed** for 1 of 3 tasks (33%)"),
        "{md}"
    );
    assert!(md.contains("the bar is judged on the average"), "{md}");
    assert!(
        md.contains(
            "**Lost to the model provider or infrastructure:** 1 attempt (`harness/interrupted`"
        ),
        "{md}"
    );
    assert!(
        md.contains("without them, 6 of 8 attempts passed (75%)"),
        "{md}"
    );
    assert!(
        md.contains("They still count as failures for the bar."),
        "{md}"
    );
    assert!(md.contains("| `t-b` | **2/3** | no |"), "{md}");
    assert!(md.contains("| `t-a` | 3/3 | yes |"), "{md}");
    assert!(
        md.contains("| Isolation | none: every attempt shared one tool server"),
        "{md}"
    );

    // The .json carries the summary, and still reads back as the same report.
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &r).unwrap();
    let json_path = path.with_extension("json");
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(raw["summary"]["tasks_all_attempts_passed"], 1);
    assert_eq!(raw["summary"]["infra_losses"], 1);
    assert_eq!(raw["summary"]["by_task"][2]["id"], "t-c");
    assert_eq!(raw["summary"]["by_task"][2]["all_attempts_passed"], false);
    let back = pmos_evals::report::read(&json_path).unwrap();
    assert_eq!(back, r);

    // No losses: no such line.
    let clean = report("2026-11-22", 2, 2);
    assert!(!clean.to_markdown().contains("Lost to the model provider"));
    assert_eq!(clean.summary().infra_losses, 0);
}

/// Copy one of the repository's categories into `root/categories`.
fn copy_category(root: &std::path::Path, name: &str) {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/categories")
        .join(name);
    for entry in walk(&src) {
        let rel = entry.strip_prefix(&src).unwrap();
        let dst = root.join("categories").join(name).join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(&entry, dst).unwrap();
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// The production role models of the test evals directory (its `prod-gateway.yaml`).
fn production() -> std::collections::BTreeMap<String, String> {
    [("default", "qwen-prod"), ("extraction", "nemo-prod")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// What a gateway serving the production role models reports.
fn prod_models() -> GatewayModels {
    GatewayModels {
        roles: production(),
        upstreams: production()
            .keys()
            .map(|r| (r.clone(), "local".to_string()))
            .collect(),
        stub: false,
        cloud_test: false,
        workarounds: true,
    }
}

/// An evals directory (config.yaml naming `prod-gateway.yaml` as the production gateway) and
/// a content directory with persona `echo` at `version`.
fn bench_dirs(version: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let evals = dir.path().join("evals");
    let content = dir.path().join("content");
    std::fs::create_dir_all(&evals).unwrap();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals");
    let config = std::fs::read_to_string(repo.join("config.yaml"))
        .unwrap()
        .replace(
            "production_gateway: ../deploy/gateway.yaml",
            "production_gateway: prod-gateway.yaml",
        );
    assert!(config.contains("prod-gateway.yaml"));
    std::fs::write(evals.join("config.yaml"), config).unwrap();
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        "upstreams: {q: 'http://vllm:8000/v1'}\nroles:\n  default: {upstream: q, model: qwen-prod}\n  extraction: {upstream: q, model: nemo-prod}\n",
    )
    .unwrap();
    set_persona_version(&content, version);
    (dir, evals, content)
}

fn set_persona_version(content: &std::path::Path, version: &str) {
    std::fs::create_dir_all(content.join("personas/echo")).unwrap();
    std::fs::write(
        content.join("personas/echo/PERSONA.md"),
        format!("---\nname: echo\nversion: \"{version}\"\n---\n\n# Echo\n"),
    )
    .unwrap();
}

/// `pmos lint`'s `persona-benchmark` input: a persona passes only when every category naming
/// it has met its bar two weeks in a row (D10), in runs that count: on the production role
/// models, and on the persona's current version (N5).
#[test]
fn a_persona_is_benchmarked_after_two_weeks_at_its_bar() {
    use pmos_evals::history::benchmarked_personas;
    // The repository's reports don't count (none recorded its models): no marketplace bot is
    // benchmarked.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(benchmarked_personas(&root.join("evals"), &root.join("content")).is_empty());
    assert!(benchmarked_personas(
        std::path::Path::new("/nonexistent/evals"),
        &root.join("content")
    )
    .is_empty());

    let (_dir, evals, content) = bench_dirs("0.1.0");
    copy_category(&evals, "echo");
    copy_category(&evals, "chief");
    let reports = evals.join("reports");
    let run = |date: &str, passed: usize| {
        let mut r = report(date, passed, 10);
        r.category = "echo".into();
        r.pass_bar = Some(0.7);
        r.gateway_models = Some(prod_models());
        r.persona_version = Some("0.1.0".into());
        write(&reports, &r).unwrap();
    };
    // One week at the bar is not enough.
    run("2026-11-09", 8);
    assert!(benchmarked_personas(&evals, &content).is_empty());
    // Two weeks in a row: Echo passes; Chief, with no report, does not.
    run("2026-11-16", 7);
    let set = benchmarked_personas(&evals, &content);
    assert_eq!(
        set.into_iter().collect::<Vec<_>>(),
        vec!["echo".to_string()]
    );
    // A new version of the persona needs its own two weeks.
    set_persona_version(&content, "0.2.0");
    assert!(benchmarked_personas(&evals, &content).is_empty());
    set_persona_version(&content, "0.1.0");
    // Without readable production role models, nothing counts.
    std::fs::remove_file(evals.join("prod-gateway.yaml")).unwrap();
    assert!(benchmarked_personas(&evals, &content).is_empty());
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        "upstreams: {q: 'http://vllm:8000/v1'}\nroles:\n  default: {upstream: q, model: qwen-prod}\n  extraction: {upstream: q, model: nemo-prod}\n",
    )
    .unwrap();
    assert_eq!(benchmarked_personas(&evals, &content).len(), 1);
    // Swapping a production model restarts the streak.
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        "upstreams: {q: 'http://vllm:8000/v1'}\nroles:\n  default: {upstream: q, model: qwen-next}\n  extraction: {upstream: q, model: nemo-prod}\n",
    )
    .unwrap();
    assert!(benchmarked_personas(&evals, &content).is_empty());
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        "upstreams: {q: 'http://vllm:8000/v1'}\nroles:\n  default: {upstream: q, model: qwen-prod}\n  extraction: {upstream: q, model: nemo-prod}\n",
    )
    .unwrap();
    // A second category for Echo that hasn't met its bar holds it back.
    let second = evals.join("categories/echo-launch");
    std::fs::create_dir_all(second.join("tasks")).unwrap();
    std::fs::copy(
        evals.join("categories/echo/category.yaml"),
        second.join("category.yaml"),
    )
    .unwrap();
    for t in walk(&evals.join("categories/echo/tasks")) {
        std::fs::copy(&t, second.join("tasks").join(t.file_name().unwrap())).unwrap();
    }
    let yaml = std::fs::read_to_string(second.join("category.yaml"))
        .unwrap()
        .replace("name: echo\n", "name: echo-launch\n");
    std::fs::write(second.join("category.yaml"), yaml).unwrap();
    assert!(benchmarked_personas(&evals, &content).is_empty());
    // A category that doesn't load still counts against the persona it names.
    std::fs::write(
        second.join("category.yaml"),
        "name: echo-launch\nbot: personal\npersona: echo\nunknown: field\n",
    )
    .unwrap();
    assert!(benchmarked_personas(&evals, &content).is_empty());
}

/// Review finding N5: only runs whose gateway reported the production role models count, and
/// the reason a run doesn't is given.
#[test]
fn only_runs_on_the_production_role_models_count() {
    use pmos_evals::history::{counts, split_counting};
    let prod = production();
    let mut r = report("2026-11-16", 9, 10);
    // Before 2026-11-22 (E1): no models recorded, so never counted.
    assert!(counts(&r, &prod, None)
        .unwrap_err()
        .contains("weren't recorded"));
    r.gateway_models = Some(prod_models());
    assert_eq!(counts(&r, &prod, None), Ok(()));
    // A model production doesn't use, on any role.
    let mut other = prod_models();
    other
        .roles
        .insert("extraction".into(), "some-remote-model".into());
    r.gateway_models = Some(other);
    let why = counts(&r, &prod, None).unwrap_err();
    assert!(
        why.contains("extraction: `some-remote-model`, production `nemo-prod`"),
        "{why}"
    );
    // A role production doesn't have, or one missing.
    let mut extra = prod_models();
    extra.roles.insert("reasoning".into(), "qwen-prod".into());
    extra.upstreams.insert("reasoning".into(), "local".into());
    r.gateway_models = Some(extra);
    assert!(counts(&r, &prod, None).is_err());
    let mut fewer = prod_models();
    fewer.roles.remove("extraction");
    r.gateway_models = Some(fewer);
    assert!(counts(&r, &prod, None).is_err());
    // The same model names, but the stub, or a testing-only upstream.
    for tweak in [
        |m: &mut GatewayModels| m.stub = true,
        |m: &mut GatewayModels| m.cloud_test = true,
        |m: &mut GatewayModels| {
            m.upstreams.insert("default".into(), "remote-test".into());
        },
    ] {
        let mut m = prod_models();
        tweak(&mut m);
        r.gateway_models = Some(m);
        assert!(counts(&r, &prod, None).is_err());
    }
    // For a persona: its version then must be its version now.
    r.gateway_models = Some(prod_models());
    assert!(counts(&r, &prod, Some("0.1.0"))
        .unwrap_err()
        .contains("version wasn't recorded"));
    r.persona_version = Some("0.1.0".into());
    assert_eq!(counts(&r, &prod, Some("0.1.0")), Ok(()));
    assert!(counts(&r, &prod, Some("0.2.0")).is_err());
    // Splitting a category's reports; with no production models, none counts.
    let old = report("2026-11-09", 9, 10);
    let (kept, left) = split_counting(
        vec![
            ("old.json".into(), old.clone()),
            ("new.json".into(), r.clone()),
        ],
        &Ok(prod.clone()),
        None,
    );
    assert_eq!((kept.len(), left.len()), (1, 1));
    assert_eq!(left[0].0, "old.json");
    let (kept, left) = split_counting(
        vec![("new.json".into(), r.clone())],
        &Err("missing".into()),
        None,
    );
    assert!(kept.is_empty() && left[0].2.contains("no production role models"));
    // The report says which models ran, never a host, and round-trips.
    let md = r.to_markdown();
    assert!(md.contains("default: `qwen-prod` (local)"), "{md}");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &r).unwrap();
    assert_eq!(load_reports(dir.path(), "docs-qa").unwrap()[0], r);
    let unrecorded = report("2026-11-16", 9, 10).to_markdown();
    assert!(unrecorded.contains("not recorded, so this run counts towards no bar"));
}

/// The gateway's roles answer becomes the report's field; an upstream that isn't one of the
/// neutral labels (an address, say) is refused rather than written into a report.
#[test]
fn the_gateways_roles_answer_is_read_as_labels_only() {
    let v = serde_json::json!({"stub": false, "cloud_test": false, "workarounds": true,
        "roles": [{"role": "default", "alias": "pmos/default", "model": "qwen-prod", "upstream": "local"}]});
    let m = GatewayModels::from_roles_answer(&v).unwrap();
    assert_eq!(m.roles["default"], "qwen-prod");
    assert_eq!(m.upstreams["default"], "local");
    let bad = serde_json::json!({"stub": false, "cloud_test": false, "workarounds": true,
        "roles": [{"role": "default", "model": "m", "upstream": "https://api.example.com"}]});
    assert!(GatewayModels::from_roles_answer(&bad).is_err());
    assert!(GatewayModels::from_roles_answer(&serde_json::json!({"roles": []})).is_err());
}
