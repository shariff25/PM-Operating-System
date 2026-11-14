//! A full `run`, the load test and the CLI, against the mocked runtime. No network beyond
//! loopback.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use common::{entry, spawn, write_category, Mock, Script, CONSOLE_TOKEN, TOKEN};
use pmos_evals::client::Runtime;
use pmos_evals::config::Config;
use pmos_evals::load::run_load;
use pmos_evals::run::{run_category, Follow, RunOptions};
use pmos_evals::spec::load_category;
use pmos_evals::tag::Tag;
use serde_json::json;

const PAGE: &str =
    "https://support.fortanix.com/docs/fortanix-data-security-manager-port-requirements";

fn task(id: &str, ask: &str, extra: &str) -> (String, String) {
    (
        id.to_string(),
        format!("id: {id}\nask: \"{ask}\"\nexpect:\n  must_cite: [{PAGE}]\n  must_include: [\"5696\", \"KMIP\"]\n  must_not_include: [\"<think>\"]\n{extra}"),
    )
}

/// Eight tasks covering a pass, each tag, and an approval-producing pass.
fn scripted(mock: &Mock) -> Vec<(String, String)> {
    let good = format!("KMIP uses port 5,696. See [Port Requirements]({PAGE}.md?ref=pmos).");
    let tool_result_with_page = entry(
        "tool_result",
        json!({"tool": "docs.search", "content": [{"url": format!("{PAGE}.md")}]}),
    );
    let tool_call = entry(
        "tool_call",
        json!({"tool": "docs.search", "args": {"query": "dsm ports"}}),
    );
    mock.script(
        "q-pass",
        Script {
            result: Some(good.clone()),
            ..Script::default()
        },
    );
    mock.script(
        "q-model",
        Script {
            result: Some(format!("Port 443. [docs]({PAGE}/)")),
            trace: vec![tool_call.clone(), tool_result_with_page.clone()],
            ..Script::default()
        },
    );
    mock.script("q-tool", Script {
        result: Some("KMIP is on 5696.".into()),
        trace: vec![tool_call.clone(), entry("tool_result", json!({"tool": "docs.search", "content": [{"url": "https://support.fortanix.com/docs/other"}]}))],
        ..Script::default()
    });
    mock.script(
        "q-harness",
        Script {
            status: "failed",
            result: None,
            error: Some("interrupted twice by restarts; ask again".into()),
            attempts: 2,
            ..Script::default()
        },
    );
    mock.script(
        "q-policy",
        Script {
            status: "failed",
            result: None,
            error: Some("could not finish".into()),
            trace: vec![entry(
                "policy_denial",
                json!({"tool": "slack.post", "reason": "never"}),
            )],
            ..Script::default()
        },
    );
    mock.script(
        "q-memory",
        Script {
            status: "failed",
            result: None,
            error: Some("Maximum context length exceeded".into()),
            ..Script::default()
        },
    );
    mock.script(
        "q-queued",
        Script {
            never_claimed: true,
            ..Script::default()
        },
    );
    mock.script(
        "q-approval",
        Script {
            status: "needs_approval",
            approvals: 1,
            result: Some(good),
            ..Script::default()
        },
    );
    let mut v: Vec<(String, String)> = [
        "pass", "model", "tool", "harness", "policy", "memory", "queued",
    ]
    .iter()
    .map(|k| task(&format!("t-{k}"), &format!("q-{k}"), ""))
    .collect();
    v.push(task("t-approval", "q-approval", "  approvals: 1\n"));
    v
}

fn opts(timeout_ms: u64) -> RunOptions {
    RunOptions {
        requested_by: "ana".into(),
        follow: Follow {
            poll: Duration::from_millis(10),
            timeout: Duration::from_millis(timeout_ms),
        },
        concurrency: 8,
        date: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        note: Some("test run".into()),
        gateway_workarounds: Some("off".into()),
        only: Vec::new(),
    }
}

#[tokio::test]
async fn full_run_grades_tags_and_writes_reports() {
    let mock = Arc::new(Mock::default());
    let tasks = scripted(&mock);
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, String)> = tasks.iter().map(|(i, y)| (i.as_str(), y.clone())).collect();
    write_category(dir.path(), "cat", "docs", 0.5, &refs);
    let category = load_category(dir.path(), "cat").unwrap();

    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let report = run_category(&rt, &url, &category, &opts(1500)).await;

    // Every task was created as an eval for the right bot and person.
    let bodies = mock.created_bodies();
    assert_eq!(bodies.len(), 8);
    assert!(bodies
        .iter()
        .all(|b| b["source"] == "eval" && b["bot"] == "docs" && b["requested_by"] == "ana"));

    let by_id = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap();
    assert!(by_id("t-pass").passed, "{:?}", by_id("t-pass").failures);
    assert!(
        by_id("t-approval").passed,
        "{:?}",
        by_id("t-approval").failures
    );
    let tag = |id: &str| by_id(id).tag.clone().unwrap();
    assert_eq!(tag("t-model").tag, Tag::Model);
    assert_eq!(tag("t-tool").tag, Tag::Tool);
    assert_eq!(tag("t-tool").rule, "page-not-retrieved");
    assert_eq!(tag("t-harness").tag, Tag::Harness);
    assert_eq!(tag("t-harness").rule, "interrupted");
    assert_eq!(tag("t-policy").tag, Tag::Policy);
    assert_eq!(tag("t-memory").tag, Tag::Memory);
    assert_eq!(tag("t-queued").tag, Tag::Harness);
    assert_eq!(tag("t-queued").rule, "never-claimed");
    assert!(by_id("t-queued").total_secs.is_none());
    assert!(by_id("t-model").trace_read);

    // Listed in category order, 2 of 8 passed, below a 50% bar; harness is the top bottleneck.
    let ids: Vec<&str> = report.tasks.iter().map(|t| t.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    assert_eq!(report.passed(), 2);
    assert!(!report.met_bar());
    assert_eq!(report.failures_by_tag()[0], (Tag::Harness, 2));

    // Timings come from the runtime's clock: ~20 ms queued, ~50 ms in all.
    let pass = by_id("t-pass");
    let (q, t) = (pass.queue_secs.unwrap(), pass.total_secs.unwrap());
    assert!((0.015..0.2).contains(&q), "queue {q}");
    assert!((0.045..0.2).contains(&t), "total {t}");

    let reports = dir.path().join("reports");
    let md = pmos_evals::report::write(&reports, &report).unwrap();
    assert_eq!(md.file_name().unwrap(), "2026-11-09-cat.md");
    assert!(reports.join("2026-11-09-cat.json").is_file());
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(
        text.contains("Top bottleneck: **harness** (2 of 6 failures)"),
        "{text}"
    );
    assert!(
        text.contains("| `t-tool` | **FAIL** | tool (`page-not-retrieved`)"),
        "{text}"
    );
    assert!(text.contains("`off` (declared by the runner's environment)"));
    // A second run the same day doesn't overwrite the first.
    let again = pmos_evals::report::write(&reports, &report).unwrap();
    assert_eq!(again.file_name().unwrap(), "2026-11-09-cat-2.md");
}

#[tokio::test]
async fn without_a_trace_token_tagging_uses_status_and_events() {
    let mock = Arc::new(Mock::default());
    let tasks = scripted(&mock);
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, String)> = tasks.iter().map(|(i, y)| (i.as_str(), y.clone())).collect();
    write_category(dir.path(), "cat", "docs", 0.5, &refs);
    let category = load_category(dir.path(), "cat").unwrap();

    let rt = Runtime::new(&url, TOKEN, None);
    let mut o = opts(1500);
    o.only = vec!["t-tool".into(), "t-policy".into(), "t-harness".into()];
    let report = run_category(&rt, &url, &category, &o).await;
    assert_eq!(report.tasks.len(), 3);
    let by_id = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap();
    // The trace is console-only, so the retrieval miss and the denial can't be seen.
    assert!(!by_id("t-tool").trace_read);
    assert_eq!(by_id("t-tool").tag.as_ref().unwrap().tag, Tag::Model);
    assert_eq!(by_id("t-policy").tag.as_ref().unwrap().tag, Tag::Model);
    assert_eq!(by_id("t-harness").tag.as_ref().unwrap().tag, Tag::Harness);
    assert!(report
        .to_markdown()
        .contains("not readable for 3 failure(s)"));
}

#[tokio::test]
async fn a_refused_create_is_a_harness_failure() {
    let mock = Arc::new(Mock::default());
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(
        dir.path(),
        "cat",
        "docs",
        1.0,
        &[(
            "t-1",
            "id: t-1\nask: \"anything\"\nexpect:\n  must_include: [\"x\"]\n".to_string(),
        )],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    let rt = Runtime::new(&url, TOKEN, None);
    let mut o = opts(1000);
    o.requested_by = "stranger".into();
    let report = run_category(&rt, &url, &category, &o).await;
    let t = &report.tasks[0];
    let tagged = t.tag.as_ref().unwrap();
    assert_eq!(
        (tagged.tag, tagged.rule.as_str()),
        (Tag::Harness, "runtime-unreachable")
    );
    assert!(tagged.reason.contains("403"), "{}", tagged.reason);
}

#[tokio::test]
async fn load_test_reports_percentiles_against_targets() {
    let mock = Arc::new(Mock {
        serial: true,
        ..Mock::default()
    });
    mock.script(
        "ask",
        Script {
            queue_ms: 0,
            work_ms: 60,
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let follow = Follow {
        poll: Duration::from_millis(10),
        timeout: Duration::from_secs(5),
    };
    let result = run_load(&rt, "docs", "ana", &["ask".to_string()], 5, &follow).await;
    assert_eq!(mock.created_bodies().len(), 5);
    assert_eq!(result.unfinished(), 0);

    // One task at a time: queue times about 0, 60, 120, 180 and 240 ms.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.yaml"), common::CONFIG).unwrap();
    let config = Config::load(dir.path()).unwrap();
    let m = result.measure(&config.latency_targets);
    let (q50, q90) = (m[0].value.unwrap(), m[1].value.unwrap());
    let (t50, t90) = (m[2].value.unwrap(), m[3].value.unwrap());
    assert!(q90 > q50 && t90 > t50, "{m:?}");
    assert!((0.1..0.4).contains(&q90), "queue p90 {q90}");
    assert!(t90 >= q90 + 0.05, "total p90 {t90}");
    assert!(result.passed(&config.latency_targets));
    let md = result.to_markdown("docs", &config.latency_targets);
    assert!(
        md.contains("**Within the targets.** 5 of 5 tasks finished."),
        "{md}"
    );
    assert!(
        md.contains("| queue p90 |") && md.contains("test plan"),
        "{md}"
    );

    // The same run against a target it misses.
    let mut tight = config.latency_targets.clone();
    tight.queue_p90.seconds = 0.01;
    assert!(!result.passed(&tight));
    assert!(result
        .to_markdown("docs", &tight)
        .contains("**Missed a target.** Latency targets are for information"));
    // Latency doesn't fail the test unless asked to.
    assert!(result.verdict(&tight, false));
    assert!(!result.verdict(&tight, true));
}

#[tokio::test]
async fn load_test_counts_unfinished_tasks() {
    let mock = Arc::new(Mock::default());
    mock.script(
        "stuck",
        Script {
            hangs: true,
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let follow = Follow {
        poll: Duration::from_millis(10),
        timeout: Duration::from_millis(200),
    };
    let result = run_load(
        &rt,
        "docs",
        "ana",
        &["stuck".to_string(), "fine".to_string()],
        4,
        &follow,
    )
    .await;
    assert_eq!(result.unfinished(), 2);
    assert_eq!(result.total_times().len(), 2);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.yaml"), common::CONFIG).unwrap();
    let config = Config::load(dir.path()).unwrap();
    assert!(!result.passed(&config.latency_targets));
    assert!(
        !result.verdict(&config.latency_targets, false),
        "unfinished tasks always fail"
    );
    assert!(result
        .to_markdown("docs", &config.latency_targets)
        .contains("timed out while `working`"));
}

/// The binary end to end: `run` writes a report and exits by the bar, `history` reads the
/// reports, `check` validates.
#[tokio::test]
async fn cli_run_history_and_check() {
    let mock = Arc::new(Mock::default());
    let good = format!("KMIP is 5696: {PAGE}");
    mock.script(
        "q-pass",
        Script {
            result: Some(good),
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let (id, yaml) = task("t-pass", "q-pass", "  approvals: 0\n");
    write_category(dir.path(), "cat", "docs", 1.0, &[(id.as_str(), yaml)]);
    let root = dir.path().to_path_buf();

    let bin = env!("CARGO_BIN_EXE_pmos-evals");
    let run = |args: Vec<String>| {
        let root = root.clone();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(bin)
                .arg("--root")
                .arg(&root)
                .args(&args)
                .env_remove("PMOS_EVALS_TOKEN")
                .env_remove("PMOS_BOTS_URL")
                .env_remove("PMOS_EVALS_AS")
                .env("PMOS_GATEWAY_WORKAROUNDS", "off")
                .output()
                .unwrap()
        })
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

    let check = run(s(&["check"])).await.unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("cat: 1 tasks for bot `docs`"));

    let no_token = run(s(&["run", "--category", "cat", "--bots-url", &url]))
        .await
        .unwrap();
    assert_eq!(no_token.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_token.stderr).contains("PMOS_EVALS_TOKEN"));

    for date in ["2026-11-02", "2026-11-09"] {
        let out = run(s(&[
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
        ]))
        .await
        .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 of 1 passed"));
    }
    let json = std::fs::read_to_string(root.join("reports/2026-11-09-cat.json")).unwrap();
    assert!(json.contains("\"gateway_workarounds\": \"off\""));

    let history = run(s(&["history", "--category", "cat"])).await.unwrap();
    let text = String::from_utf8_lossy(&history.stdout).to_string();
    assert_eq!(history.status.code(), Some(0), "{text}");
    assert!(
        text.contains("cat: met its bar two weeks in a row"),
        "{text}"
    );

    // A failing run the next week: the run exits 1 and the streak is broken.
    mock.script(
        "q-pass",
        Script {
            result: Some("no idea".into()),
            ..Script::default()
        },
    );
    let out = run(s(&[
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
        "2026-11-16",
    ]))
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let history = run(s(&["history", "--category", "cat"])).await.unwrap();
    assert_eq!(history.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&history.stdout).contains("has not met"));
}
