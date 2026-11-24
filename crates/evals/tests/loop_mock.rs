//! The improvement-loop additions end to end against the mocked runtime and gateway: repeats,
//! drafts checks, `bot: personal`, `must_decline`, judges and calibration, and the `gate`,
//! `compare`, `judge calibrate` and `affected` commands. No network beyond loopback.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use common::{
    prod_gateway, spawn, spawn_gateway, write_category, Gateway, Mock, Script, CONSOLE_TOKEN,
    GATEWAY_TOKEN, TOKEN,
};
use pmos_evals::client::Runtime;
use pmos_evals::judge::{self, Judge};
use pmos_evals::run::{run_category, Follow, JudgeRun, RunOptions};
use pmos_evals::spec::load_category;
use pmos_evals::tag::Tag;
use serde_json::json;

fn opts() -> RunOptions {
    RunOptions {
        requested_by: "ana".into(),
        follow: Follow {
            poll: Duration::from_millis(10),
            timeout: Duration::from_millis(2000),
            cancel_wait: Duration::from_secs(1),
        },
        concurrency: 4,
        date: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
        note: None,
        gateway_workarounds: None,
        only: Vec::new(),
        repeats: 1,
        decline_phrases: vec!["don't answer".into(), "couldn't find".into()],
        judges: None,
        judges_skipped: None,
        isolation: None,
    }
}

/// A category file with no bar, a bot and repeats.
fn category_file(root: &Path, name: &str, bot: &str, repeats: u32) {
    std::fs::write(
        root.join("categories").join(name).join("category.yaml"),
        format!("name: {name}\nbot: {bot}\nrepeats: {repeats}\ndescription: A test category.\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn repeats_run_each_task_k_times_and_report_pass_hat_k() {
    let mock = Arc::new(Mock::default());
    let good = Script {
        result: Some("KMIP is 5696.".into()),
        ..Script::default()
    };
    let bad = Script {
        result: Some("No idea.".into()),
        ..Script::default()
    };
    mock.script_seq("q-flaky", vec![good.clone(), bad.clone(), good.clone()]);
    mock.script("q-steady", good);
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yaml = |id: &str, ask: &str| {
        format!("id: {id}\nask: \"{ask}\"\ncluster: ports\nexpect:\n  must_include: [\"5696\"]\n")
    };
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[
            ("flaky", yaml("flaky", "q-flaky")),
            ("steady", yaml("steady", "q-steady")),
        ],
    );
    category_file(dir.path(), "cat", "docs", 3);
    let category = load_category(dir.path(), "cat").unwrap();
    assert_eq!(category.repeats, 3);

    let rt = Runtime::new(&url, TOKEN, None);
    let mut o = opts();
    o.repeats = category.repeats;
    o.concurrency = 1;
    let report = run_category(&rt, &url, &category, &o).await;
    assert_eq!(mock.created_bodies().len(), 6);
    assert_eq!(report.repeats, 3);
    let lines: Vec<(&str, u32, bool)> = report
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t.repeat, t.passed))
        .collect();
    assert_eq!(
        lines,
        [
            ("flaky", 1, true),
            ("flaky", 2, false),
            ("flaky", 3, true),
            ("steady", 1, true),
            ("steady", 2, true),
            ("steady", 3, true),
        ]
    );
    assert!(report
        .tasks
        .iter()
        .all(|t| t.cluster.as_deref() == Some("ports")));
    let md = report.to_markdown();
    assert!(
        md.contains("**Reports only.** 5 of 6 attempts passed (83%) over 2 tasks run 3 times each"),
        "{md}"
    );
    assert!(
        md.contains("| All attempts passed (pass^3) | 50% (1 of 2 tasks passed every run) |"),
        "{md}"
    );
    assert!(
        md.contains("| `flaky` | **2/3** | no | model/missing-fact ×1 |"),
        "{md}"
    );
    // One cluster: the standard error falls back to the binomial.
    assert!(md.contains("(standard error, binomial)"), "{md}");
}

fn jira_task() -> String {
    "id: story\nask: \"q-story\"\nexpect:\n  approvals: 1\n  drafts:\n    - tool: jira.create_issue\n      count: 1\n      require_fields: [project, issue_type, summary]\n      allowed: {project: [PM], issue_type: [Story]}\n      must_include: [SAML]\n    - tool: slack.send_message\n      count: 0\n".into()
}

fn page_task() -> String {
    "id: page\nask: \"q-page\"\nexpect:\n  drafts:\n    - tool: confluence.create_draft\n      allowed: {space: [PM]}\n".into()
}

#[tokio::test]
async fn personal_bots_drafts_and_their_tags() {
    let mock = Arc::new(Mock::default());
    mock.script(
        "q-story",
        Script::drafting(vec![(
            "jira.create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "Require SAML SSO", "epic": "PM-101"}),
        )]),
    );
    mock.script(
        "q-page",
        Script::drafting(vec![(
            "confluence.create_draft",
            json!({"space": "ENG", "title": "Notes", "body": "SSO and audit export"}),
        )]),
    );
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(
        dir.path(),
        "drafting",
        "personal",
        0.5,
        &[("story", jira_task()), ("page", page_task())],
    );
    category_file(dir.path(), "drafting", "personal", 1);
    let category = load_category(dir.path(), "drafting").unwrap();
    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let report = run_category(&rt, &url, &category, &opts()).await;

    // `bot: personal` went to the requester's own bot.
    assert!(mock
        .created_bodies()
        .iter()
        .all(|b| b["bot"] == "pm-ana" && b["requested_by"] == "ana"));
    assert_eq!(report.bot, "pm-ana");
    assert_eq!(mock.drafts_read.lock().unwrap().len(), 2);

    let by_id = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap();
    assert!(by_id("story").passed, "{:?}", by_id("story").failures);
    let page = by_id("page");
    assert!(!page.passed);
    let tag = page.tag.as_ref().unwrap();
    assert_eq!(
        (tag.tag, tag.rule.as_str(), tag.sub.as_deref()),
        (
            Tag::Tool,
            "draft-wrong-target",
            Some("tool/wrong-confluence-space")
        )
    );
    let md = report.to_markdown();
    assert!(
        md.contains("| `tool/wrong-confluence-space` | 1 | `page` |"),
        "{md}"
    );
    assert!(
        md.contains("`confluence.create_draft` draft apr_0: `space` is `ENG`, allowed `PM`"),
        "{md}"
    );
}

#[tokio::test]
async fn drafts_the_runtime_cant_serve_are_a_harness_failure() {
    let mock = Arc::new(Mock {
        no_drafts: true,
        ..Mock::default()
    });
    mock.script(
        "q-story",
        Script::drafting(vec![("jira.create_issue", json!({"project": "PM"}))]),
    );
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(
        dir.path(),
        "drafting",
        "personal",
        0.5,
        &[("story", jira_task())],
    );
    let category = load_category(dir.path(), "drafting").unwrap();
    let rt = Runtime::new(&url, TOKEN, None);
    let report = run_category(&rt, &url, &category, &opts()).await;
    let tag = report.tasks[0].tag.as_ref().unwrap();
    assert_eq!(
        (tag.tag, tag.sub.as_deref()),
        (Tag::Harness, Some("harness/drafts-unreadable"))
    );
    assert!(tag.reason.contains("404"), "{}", tag.reason);
}

#[tokio::test]
async fn must_decline_through_a_run() {
    let mock = Arc::new(Mock::default());
    mock.script(
        "q-declined",
        Script {
            result: Some("The support docs don't answer that; roadmaps aren't public.".into()),
            ..Script::default()
        },
    );
    mock.script(
        "q-made-up",
        Script {
            result: Some("Acme Vault 6 ships in Q3 with quantum keys.".into()),
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yaml = |id: &str, ask: &str| {
        format!("id: {id}\nask: \"{ask}\"\nexpect:\n  must_decline: true\n  approvals: 0\n")
    };
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[
            ("declined", yaml("declined", "q-declined")),
            ("made-up", yaml("made-up", "q-made-up")),
        ],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    let rt = Runtime::new(&url, TOKEN, None);
    let report = run_category(&rt, &url, &category, &opts()).await;
    assert!(report.tasks[0].passed, "{:?}", report.tasks[0].failures);
    let made_up = &report.tasks[1];
    assert!(!made_up.passed);
    assert_eq!(
        made_up.tag.as_ref().unwrap().sub.as_deref(),
        Some("model/answered-beyond-sources")
    );
}

fn judge(name: &str, needs_sources: bool) -> Judge {
    Judge {
        name: name.into(),
        failure_mode: "model/unfaithful-to-source".into(),
        question: "Is every claim supported?".into(),
        alias: judge::DEFAULT_ALIAS.into(),
        applies_to: vec!["cat".into()],
        labels: format!("{name}.labels.jsonl"),
        needs_sources,
    }
}

/// Says yes when the output it is shown mentions 5696, else no.
fn gateway() -> Arc<Gateway> {
    Arc::new(Gateway {
        reply: Box::new(|user: &str| {
            let output = user.split("<output>").nth(1).unwrap_or_default();
            if output.contains("5696") {
                "<think>it's on the page</think>Yes.".into()
            } else {
                "No".into()
            }
        }),
        calls: Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn judges_record_verdicts_without_changing_pass_or_fail() {
    let mock = Arc::new(Mock::default());
    let read = common::entry(
        "tool_result",
        json!({"tool": "docs.read", "content": "KMIP uses port 5696."}),
    );
    mock.script(
        "q-good",
        Script {
            result: Some("KMIP is 5696.".into()),
            trace: vec![read.clone()],
            ..Script::default()
        },
    );
    mock.script(
        "q-bad",
        Script {
            result: Some("KMIP is 443.".into()),
            trace: vec![read],
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let gw = gateway();
    let gw_url = spawn_gateway(gw.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yaml = |id: &str, ask: &str| {
        format!("id: {id}\nask: \"{ask}\"\nexpect:\n  must_include: [KMIP]\n")
    };
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[
            ("good", yaml("good", "q-good")),
            ("bad", yaml("bad", "q-bad")),
        ],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    let mut o = opts();
    let faithful = judge("faithful", true);
    let status = judge::status(&faithful, None);
    o.judges = Some(JudgeRun {
        gateway: pmos_evals::judge::Gateway::new(&gw_url, GATEWAY_TOKEN),
        judges: vec![(faithful, status)],
        labels: None,
    });

    // Without a console token the trace can't be read, so a judge that needs sources skips.
    let blind = Runtime::new(&url, TOKEN, None);
    let report = run_category(&blind, &url, &category, &o).await;
    assert!(gw.calls.lock().unwrap().is_empty());
    let v = &report.tasks[0].judges[0];
    assert_eq!(v.verdict, None);
    assert!(v.error.as_deref().unwrap().contains("no tool results"));

    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let report = run_category(&rt, &url, &category, &o).await;
    let verdict = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap().judges[0].verdict;
    assert_eq!((verdict("good"), verdict("bad")), (Some(true), Some(false)));
    // Both pass their deterministic checks; the judge never fails a task.
    assert!(report.tasks.iter().all(|t| t.passed));
    assert!(!report.tasks[0].judges[0].blocking);
    let calls = gw.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["model"], "pmos/extraction");
    let user = calls[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains("Question: Is every claim supported?"),
        "{user}"
    );
    assert!(user.contains("KMIP uses port 5696"), "the sources: {user}");
    let md = report.to_markdown();
    assert!(
        md.contains("| `faithful` | advisory: not calibrated | 1 | 1 | 0 | 0 |"),
        "{md}"
    );
}

/// A judge may answer "unknown"; the report counts it apart. With a label sink the run keeps
/// a labelling-pack candidate per judged attempt (Slack results left out, label empty), and
/// `judge::pack` adds a mix of them to the labels file for a person, which `calibrate` then
/// leaves out until they are labelled.
#[tokio::test]
async fn unknown_verdicts_label_candidates_and_the_pack() {
    let mock = Arc::new(Mock::default());
    let docs = common::entry(
        "tool_result",
        json!({"tool": "docs.read", "content": "KMIP uses port 5696."}),
    );
    let slack = common::entry(
        "tool_result",
        json!({"tool": "slack.read_thread", "content": "a fixture Slack message"}),
    );
    for (ask, answer) in [
        ("q-good", "KMIP is 5696."),
        ("q-bad", "KMIP is 443."),
        ("q-vague", "KMIP is maybe somewhere."),
    ] {
        mock.script(
            ask,
            Script {
                result: Some(answer.into()),
                trace: vec![docs.clone(), slack.clone()],
                ..Script::default()
            },
        );
    }
    let url = spawn(mock.clone()).await;
    let gw = Arc::new(Gateway {
        reply: Box::new(|user: &str| {
            let output = user.split("<output>").nth(1).unwrap_or_default();
            if output.contains("5696") {
                "yes".into()
            } else if output.contains("maybe") {
                "Unknown.".into()
            } else {
                "no".into()
            }
        }),
        calls: Mutex::new(Vec::new()),
    });
    let gw_url = spawn_gateway(gw.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yaml = |id: &str, ask: &str| {
        format!("id: {id}\nask: \"{ask}\"\nexpect:\n  must_include: [KMIP]\n")
    };
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[
            ("good", yaml("good", "q-good")),
            ("bad", yaml("bad", "q-bad")),
            ("vague", yaml("vague", "q-vague")),
        ],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    let faithful = judge("faithful", true);
    let status = judge::status(&faithful, None);
    let sink = pmos_evals::run::LabelSink::default();
    let mut o = opts();
    o.judges = Some(JudgeRun {
        gateway: pmos_evals::judge::Gateway::new(&gw_url, GATEWAY_TOKEN),
        judges: vec![(faithful.clone(), status)],
        labels: Some(sink.clone()),
    });
    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let report = run_category(&rt, &url, &category, &o).await;
    let v = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap().judges[0].clone();
    assert_eq!(v("good").verdict, Some(true));
    assert_eq!(v("bad").verdict, Some(false));
    let vague = v("vague");
    assert_eq!((vague.verdict, vague.unknown), (None, true));
    assert!(vague.error.unwrap().contains("unknown"));
    let md = report.to_markdown();
    assert!(
        md.contains("| `faithful` | advisory: not calibrated | 1 | 1 | 1 | 0 |"),
        "{md}"
    );
    // The judge is told it may say unknown.
    let calls = gw.calls.lock().unwrap().clone();
    assert!(calls[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("yes, no, or unknown"));

    let lines = sink.lock().unwrap().clone();
    assert_eq!(lines.len(), 3);
    for (name, l) in &lines {
        assert_eq!(name, "faithful");
        assert_eq!(l.label, judge::Human::Pending);
        assert!(l.input.contains("KMIP uses port 5696."), "{}", l.input);
        assert!(!l.input.contains("a fixture Slack message"), "{}", l.input);
        assert!(
            l.input.contains("Slack tool result(s) left out"),
            "{}",
            l.input
        );
    }
    let said: Vec<_> = lines
        .iter()
        .map(|(_, l)| l.judge_said.clone().unwrap())
        .collect();
    assert!(said.contains(&"unknown".to_string()));

    // The candidates on disk, then the pack: two lines, one the judge said no to and one yes.
    let root = dir.path();
    let cands = root.join("candidates");
    let written = judge::write_candidates(&cands, &report.date, "cat", &lines).unwrap();
    assert_eq!(written.len(), 1);
    std::fs::create_dir_all(root.join("judges")).unwrap();
    std::fs::write(root.join("judges/faithful.labels.jsonl"), "").unwrap();
    let packed = judge::pack(root, &faithful, &cands, 2).unwrap();
    assert_eq!((packed.no, packed.yes, packed.unknown), (1, 1, 0));
    let labels = judge::load_labels(root, &faithful).unwrap();
    assert_eq!(labels.len(), 2);
    assert!(labels.iter().all(|l| l.label == judge::Human::Pending));
    // Packing again adds the one left, never a line already there.
    let again = judge::pack(root, &faithful, &cands, 20).unwrap();
    assert_eq!(
        (again.no, again.yes, again.unknown, again.already),
        (0, 0, 1, 2)
    );
    assert_eq!(judge::load_labels(root, &faithful).unwrap().len(), 3);
}

fn cli(root: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_pmos-evals"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env_remove("PMOS_EVALS_TOKEN")
        .env_remove("PMOS_BOTS_URL")
        .env_remove("PMOS_EVALS_AS")
        .env_remove("PMOS_EVALS_LANES")
        .env_remove("PMOS_EVALS_FRESH_CMD")
        .env_remove("PMOS_GATEWAY_URL")
        .env_remove("PMOS_EVALS_GATEWAY_TOKEN")
        .env_remove("PMOS_EVALS_BASELINE_REF")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    drop(pipe);
    child.wait_with_output().unwrap()
}

async fn cli_async(root: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let stdin = stdin.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        cli(&root, &refs, stdin.as_deref())
    })
    .await
    .unwrap()
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test]
async fn cli_run_compare_and_gate() {
    let mock = Arc::new(Mock::default());
    let good = Script {
        result: Some("KMIP is 5696.".into()),
        ..Script::default()
    };
    let bad = Script {
        result: Some("No idea.".into()),
        ..Script::default()
    };
    mock.script("q-a", good.clone());
    mock.script("q-b", good.clone());
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = |id: &str, ask: &str, extra: &str| {
        format!("id: {id}\nask: \"{ask}\"\n{extra}expect:\n  must_include: [\"5696\"]\n")
    };
    write_category(
        root,
        "cat",
        "docs",
        0.5,
        &[
            ("a", yaml("a", "q-a", "regression: true\n")),
            ("b", yaml("b", "q-b", "")),
        ],
    );
    common::write_taxonomy(root);
    let run = |date: &'static str| {
        let url = url.clone();
        async move {
            cli_async(
                root,
                &[
                    "run",
                    "--category",
                    "cat",
                    "--bots-url",
                    &url,
                    "--token",
                    TOKEN,
                    "--as",
                    "ana",
                    "--date",
                    date,
                    "--repeats",
                    "2",
                ],
                None,
            )
            .await
        }
    };
    let first = run("2026-11-09").await;
    assert_eq!(first.status.code(), Some(0), "{}", out(&first));
    let reports = root.join("reports");
    let baseline = reports.join("2026-11-09-cat.json");
    let json = std::fs::read_to_string(&baseline).unwrap();
    assert!(json.contains("\"repeats\": 2"), "{json}");

    // The same again: the second report compares itself with the first.
    let second = run("2026-11-10").await;
    assert_eq!(second.status.code(), Some(0), "{}", out(&second));
    let md = std::fs::read_to_string(reports.join("2026-11-10-cat.md")).unwrap();
    assert!(md.contains("## Compared with the previous run"), "{md}");
    assert!(md.contains("Previous run: `2026-11-09-cat.json`"), "{md}");
    assert!(
        md.contains("Mean paired difference in pass rate: **0.0 pts**"),
        "{md}"
    );

    // A gate against the first run with nothing changed passes.
    let gate = cli_async(
        root,
        &[
            "gate",
            "--category",
            "cat",
            "--baseline",
            baseline.to_str().unwrap(),
            "--bots-url",
            &url,
            "--token",
            TOKEN,
            "--as",
            "ana",
            "--date",
            "2026-11-11",
        ],
        None,
    )
    .await;
    let text = String::from_utf8_lossy(&gate.stdout).to_string();
    assert_eq!(gate.status.code(), Some(0), "{}", out(&gate));
    assert!(text.starts_with("## Eval gate: `cat` passed"), "{text}");
    assert!(
        !text.contains("report:"),
        "stdout is the markdown only: {text}"
    );

    // Now the regression task breaks: the gate fails, as a run would show.
    mock.script("q-a", bad);
    let gate = cli_async(
        root,
        &[
            "gate",
            "--category",
            "cat",
            "--baseline",
            baseline.to_str().unwrap(),
            "--bots-url",
            &url,
            "--token",
            TOKEN,
            "--as",
            "ana",
            "--date",
            "2026-11-12",
        ],
        None,
    )
    .await;
    let text = String::from_utf8_lossy(&gate.stdout).to_string();
    assert_eq!(gate.status.code(), Some(1), "{}", out(&gate));
    assert!(text.contains("## Eval gate: `cat` **FAILED**"), "{text}");
    assert!(
        text.contains("regression task `a` flipped to failing"),
        "{text}"
    );
    assert!(
        text.contains("`a`: check `must_include:5696` passed in the baseline and now fails"),
        "{text}"
    );

    // The same decision from a report, without running anything.
    let failed = reports.join("2026-11-12-cat.json");
    let again = cli_async(
        root,
        &[
            "gate",
            "--category",
            "cat",
            "--baseline",
            baseline.to_str().unwrap(),
            "--candidate",
            failed.to_str().unwrap(),
        ],
        None,
    )
    .await;
    assert_eq!(again.status.code(), Some(1), "{}", out(&again));
    // Or with the baseline from a directory of reports, such as the base branch's: the
    // latest there is the baseline.
    let base_dir = root.join("base-reports");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::copy(&baseline, base_dir.join("2026-11-09-cat.json")).unwrap();
    let from_dir = cli_async(
        root,
        &[
            "gate",
            "--category",
            "cat",
            "--baseline-reports",
            base_dir.to_str().unwrap(),
            "--candidate",
            failed.to_str().unwrap(),
        ],
        None,
    )
    .await;
    assert_eq!(from_dir.status.code(), Some(1), "{}", out(&from_dir));
    assert!(
        String::from_utf8_lossy(&from_dir.stdout)
            .contains("Baseline: `2026-11-09-cat.json` from `"),
        "{}",
        out(&from_dir)
    );
    let empty = root.join("no-reports");
    std::fs::create_dir_all(&empty).unwrap();
    let none = cli_async(
        root,
        &[
            "gate",
            "--category",
            "cat",
            "--baseline-reports",
            empty.to_str().unwrap(),
            "--candidate",
            failed.to_str().unwrap(),
        ],
        None,
    )
    .await;
    assert_eq!(none.status.code(), Some(0), "{}", out(&none));
    assert!(String::from_utf8_lossy(&none.stdout).contains("No baseline"));
    let wrong = cli_async(
        root,
        &[
            "gate",
            "--category",
            "other",
            "--candidate",
            failed.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
        None,
    )
    .await;
    assert_eq!(wrong.status.code(), Some(2), "{}", out(&wrong));

    // compare: any two reports, by .json or .md.
    let cmp = cli_async(
        root,
        &[
            "compare",
            baseline.to_str().unwrap(),
            reports.join("2026-11-12-cat.md").to_str().unwrap(),
        ],
        None,
    )
    .await;
    let text = String::from_utf8_lossy(&cmp.stdout).to_string();
    assert_eq!(cmp.status.code(), Some(0), "{}", out(&cmp));
    assert!(
        text.starts_with(
            "# Compare: 2026-11-09-cat.json (2026-11-09) → 2026-11-12-cat.md (2026-11-12)"
        ),
        "{text}"
    );
    assert!(text.contains("- **Flipped to failing:** `a`"), "{text}");

    // check validates the sub-tags the reports now carry.
    let check = cli_async(root, &["check", "--content", "missing"], None).await;
    assert_eq!(check.status.code(), Some(0), "{}", out(&check));
    let mut bad_report = std::fs::read_to_string(&failed).unwrap();
    bad_report = bad_report.replace("model/missing-fact", "model/made-up-tag");
    std::fs::write(&failed, bad_report).unwrap();
    let check = cli_async(root, &["check", "--content", "missing"], None).await;
    assert_eq!(check.status.code(), Some(2));
    assert!(
        out(&check).contains("reports/2026-11-12-cat.json: tag `model/made-up-tag` is not in"),
        "{}",
        out(&check)
    );
}

#[tokio::test]
async fn cli_judge_calibration_decides_blocking() {
    let gw = gateway();
    let gw_url = spawn_gateway(gw.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_category(
        root,
        "cat",
        "docs",
        0.5,
        &[(
            "a",
            "id: a\nask: \"x\"\nexpect:\n  must_include: [y]\n".into(),
        )],
    );
    common::write_taxonomy(root);
    let judges = root.join("judges");
    std::fs::create_dir_all(&judges).unwrap();
    std::fs::write(
        judges.join("faithful.yaml"),
        "name: faithful\nfailure_mode: model/unfaithful-to-source\nquestion: Is every claim supported?\napplies_to: [cat]\nlabels: faithful.labels.jsonl\n",
    )
    .unwrap();
    // No labels yet: calibrate refuses, check passes and calls the judge advisory.
    std::fs::write(judges.join("faithful.labels.jsonl"), "").unwrap();
    let gw_args = [
        "--gateway-url",
        gw_url.as_str(),
        "--gateway-token",
        GATEWAY_TOKEN,
    ];
    let mut args = vec!["judge", "calibrate", "faithful"];
    args.extend(gw_args);
    let none = cli_async(root, &args, None).await;
    assert_eq!(none.status.code(), Some(2));
    assert!(out(&none).contains("no labels yet"), "{}", out(&none));
    let check = cli_async(root, &["check", "--content", "missing"], None).await;
    assert!(
        out(&check).contains("judge faithful: 0 labels, advisory: not calibrated"),
        "{}",
        out(&check)
    );

    // 120 labels, half yes and half no; the mock judge gets 54 of 60 yes and all 60 no right.
    let mut lines = String::new();
    for i in 0..120 {
        let yes = i % 2 == 0;
        let output = if yes && i % 20 != 0 {
            "Port 5696."
        } else {
            "Port 443."
        };
        lines.push_str(
            &serde_json::to_string(
                &json!({"input": format!("q{i}"), "output": output, "label": yes}),
            )
            .unwrap(),
        );
        lines.push('\n');
    }
    // Plus a line a person marked unknown and two still waiting: neither counts.
    lines.push_str("{\"input\": \"qu\", \"output\": \"Port 443.\", \"label\": \"unknown\"}\n");
    lines.push_str("{\"input\": \"qp1\", \"output\": \"Port 443.\", \"label\": null, \"from\": \"t run 1\", \"judge_said\": \"no\"}\n");
    lines.push_str("{\"input\": \"qp2\", \"output\": \"Port 5696.\", \"label\": null}\n");
    std::fs::write(judges.join("faithful.labels.jsonl"), lines).unwrap();
    let mut args = vec!["judge", "calibrate", "faithful", "--date", "2026-11-15"];
    args.extend(gw_args);
    let calls_before = gw.calls.lock().unwrap().len();
    let cal = cli_async(root, &args, None).await;
    assert_eq!(cal.status.code(), Some(0), "{}", out(&cal));
    assert!(
        out(&cal).contains("n 120 (60 yes, 60 no), TPR 0.90, TNR 1.00, 0 unparsed (0 unknown); left out: 1 labelled unknown, 2 not labelled yet"),
        "{}",
        out(&cal)
    );
    // Only the 120 answered labels were put to the judge.
    assert_eq!(gw.calls.lock().unwrap().len() - calls_before, 120);
    assert!(out(&cal).contains("blocking: n 120"), "{}", out(&cal));
    let written: judge::Calibration = serde_json::from_str(
        &std::fs::read_to_string(judges.join("faithful.calibration.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written.alias, "pmos/extraction");
    assert_eq!(
        written.question_sha256,
        judge::question_hash("Is every claim supported?", "pmos/extraction")
    );
    assert_eq!(gw.calls.lock().unwrap().len(), 120);

    let check = cli_async(root, &["check", "--content", "missing"], None).await;
    assert!(
        out(&check).contains(
            "judge faithful: 120 labels (1 labelled unknown, 2 waiting for a person), blocking"
        ),
        "{}",
        out(&check)
    );
    // Rewording the question makes it advisory until it is calibrated again.
    std::fs::write(
        judges.join("faithful.yaml"),
        "name: faithful\nfailure_mode: model/unfaithful-to-source\nquestion: Is every claim on the page?\napplies_to: [cat]\nlabels: faithful.labels.jsonl\n",
    )
    .unwrap();
    let check = cli_async(root, &["check", "--content", "missing"], None).await;
    assert!(
        out(&check).contains("advisory: the question changed since calibration"),
        "{}",
        out(&check)
    );
}

#[tokio::test]
async fn cli_affected_reads_changed_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals");
    let content = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let every: String = pmos_evals::spec::category_names(&root)
        .unwrap()
        .iter()
        .map(|n| format!("{n}\n"))
        .collect();
    // A skill change runs every category, not only the Docs bot's (owner, 2026-11-23).
    let o = cli_async(
        &root,
        &["affected", "--content", content.to_str().unwrap()],
        Some("content/skills/docs/SKILL.md\nREADME.md\n"),
    )
    .await;
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), every);
    // A change outside the evals, the gateway config and the bots' content runs nothing.
    let o = cli_async(
        &root,
        &["affected", "--content", content.to_str().unwrap()],
        Some("README.md\ncontent/templates/prd.md\n"),
    )
    .await;
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    let o = cli_async(
        &root,
        &[
            "affected",
            "--content",
            content.to_str().unwrap(),
            "--changed",
            "-",
        ],
        Some("evals/config.yaml\n"),
    )
    .await;
    let all = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        all.contains("docs-qa\n") && all.contains("slack-reply\n"),
        "{all}"
    );
}

/// Review finding N5: `run` records the models the gateway itself reports (never what the
/// operator types in `--note`), and `history` counts a run only when they are the production
/// role models; the others are listed with the reason.
#[tokio::test]
async fn cli_run_records_the_gateways_models_and_history_counts_only_production_ones() {
    let mock = Arc::new(Mock::default());
    mock.script(
        "q-a",
        Script {
            result: Some("KMIP is 5696.".into()),
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let gw_url = spawn_gateway(gateway()).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_category(
        root,
        "cat",
        "docs",
        0.5,
        &[(
            "a",
            "id: a\nask: \"q-a\"\nexpect:\n  must_include: [\"5696\"]\n".to_string(),
        )],
    );
    // `write_category` names `prod-gateway.yaml` as the production gateway config.
    let prod = |default: &str| {
        std::fs::write(
            root.join("prod-gateway.yaml"),
            format!("upstreams: {{q: 'http://vllm:8000/v1'}}\nroles:\n  default: {{upstream: q, model: {default}}}\n  extraction: {{upstream: q, model: nemo-prod}}\n"),
        )
        .unwrap();
    };
    prod("qwen-prod");
    let run = |date: &'static str, with_gateway: bool| {
        let url = url.clone();
        let gw_url = gw_url.clone();
        async move {
            let mut args = vec![
                "run",
                "--category",
                "cat",
                "--bots-url",
                &url,
                "--token",
                TOKEN,
                "--as",
                "ana",
                "--date",
                date,
                "--no-judges",
                "--note",
                "model: whatever the operator typed",
            ];
            if with_gateway {
                args.extend(["--gateway-url", &gw_url, "--gateway-token", GATEWAY_TOKEN]);
            }
            cli_async(root, &args, None).await
        }
    };
    // Two weeks with the models of the runtime's own (production) gateway recorded.
    *mock.gateway.lock().unwrap() = Some(prod_gateway());
    for date in ["2026-11-09", "2026-11-16"] {
        let o = run(date, true).await;
        assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    }
    // A runtime that doesn't report its gateway: nothing recorded, though the runner has one.
    *mock.gateway.lock().unwrap() = None;
    let o = run("2026-11-17", true).await;
    assert!(out(&o).contains("won't count towards a bar"), "{}", out(&o));
    // R3: the runtime's gateway is a test one serving the same model names, while the runner
    // was pointed at another gateway. The report records the runtime's, which doesn't count.
    let mut test_gw = prod_gateway();
    test_gw["class"] = json!("test");
    *mock.gateway.lock().unwrap() = Some(test_gw);
    let o = run("2026-11-18", true).await;
    assert!(out(&o).contains("counts towards no bar"), "{}", out(&o));
    let json18: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("reports/2026-11-18-cat.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json18["gateway_models"]["class"], "test");
    // Without the runner's own gateway the models are still recorded: they come from the
    // runtime.
    *mock.gateway.lock().unwrap() = Some(prod_gateway());
    let o = run("2026-11-19", false).await;
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    let json19: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("reports/2026-11-19-cat.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json19["gateway_models"]["class"], "production");
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("reports/2026-11-16-cat.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        json["gateway_models"]["roles"],
        json!({"default": "qwen-prod", "extraction": "nemo-prod"})
    );
    assert_eq!(json["gateway_models"]["upstreams"]["default"], "local");
    assert!(
        !json.to_string().contains(&gw_url),
        "no gateway address in a report"
    );
    let md = std::fs::read_to_string(root.join("reports/2026-11-16-cat.md")).unwrap();
    assert!(md.contains("default: `qwen-prod` (local)"), "{md}");

    let history = || cli(root, &["history", "--category", "cat"], None);
    let h = history();
    assert_eq!(h.status.code(), Some(0), "{}", out(&h));
    assert!(
        out(&h).contains("met its bar two weeks in a row"),
        "{}",
        out(&h)
    );
    assert!(
        out(&h).contains("2026-11-17-cat.json: the gateway's models weren't recorded"),
        "{}",
        out(&h)
    );
    assert!(
        out(&h).contains("2026-11-18-cat.json: the runtime's gateway is a `test` gateway"),
        "{}",
        out(&h)
    );
    // Production moves to another model: the earlier runs no longer count.
    prod("qwen-next");
    let h = history();
    assert_eq!(h.status.code(), Some(1), "{}", out(&h));
    assert!(
        out(&h).contains("default: `qwen-prod`, production `qwen-next`"),
        "{}",
        out(&h)
    );
}
