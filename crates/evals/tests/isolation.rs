//! Isolated attempts (`pmos_evals::isolate`): each attempt takes a lane of its own and gets a
//! fresh tool server there first, so nothing one attempt left (a pending reply) changes the
//! next. The mocked runtime plays the tool server's one-pending-reply rule.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use common::{spawn, write_category, Mock, Script, CONSOLE_TOKEN, TOKEN};
use pmos_evals::client::Runtime;
use pmos_evals::isolate::{FreshFn, Isolation};
use pmos_evals::run::{run_category, Follow, RunOptions};
use pmos_evals::spec::load_category;
use serde_json::json;

const ASK: &str = "Draft a reply in the field-questions thread.";

fn opts(repeats: u32, concurrency: usize) -> RunOptions {
    RunOptions {
        requested_by: "ana".into(),
        follow: Follow {
            poll: Duration::from_millis(10),
            timeout: Duration::from_millis(3000),
        },
        concurrency,
        date: NaiveDate::from_ymd_opt(2026, 11, 22).unwrap(),
        note: None,
        gateway_workarounds: None,
        only: Vec::new(),
        repeats,
        decline_phrases: Vec::new(),
        judges: None,
        judges_skipped: None,
        isolation: None,
    }
}

/// A mock whose ask drafts one reply, the first time only until it is made fresh.
fn replying_mock() -> Arc<Mock> {
    let mock = Arc::new(Mock {
        one_draft_until_fresh: true,
        ..Mock::default()
    });
    mock.script(
        ASK,
        Script {
            work_ms: 60,
            ..Script::drafting(vec![(
                "slack.post_reply",
                json!({"channel": "C0FIELD", "thread_ts": "1731500000.000100", "text": "Yes."}),
            )])
        },
    );
    mock
}

fn reply_task() -> (String, String) {
    (
        "reply".to_string(),
        format!(
            "id: reply\nask: \"{ASK}\"\nexpect:\n  approvals: 1\n  drafts:\n    - tool: slack.post_reply\n      count: 1\n"
        ),
    )
}

fn category(root: &std::path::Path) -> pmos_evals::spec::Category {
    let (id, yaml) = reply_task();
    write_category(root, "cat", "pm-ana", 0.8, &[(id.as_str(), yaml)]);
    load_category(root, "cat").unwrap()
}

/// Each lane's fresh tool server is its mock's [`Mock::fresh`].
fn fresh_fn(mocks: Vec<Arc<Mock>>) -> FreshFn {
    Arc::new(move |lane: usize| {
        let mock = mocks[lane - 1].clone();
        Box::pin(async move {
            mock.fresh();
            Ok(())
        })
    })
}

/// Without isolation, the first attempt's pending reply blocks the other two: the defect E1
/// found. This is what the isolated run below must not do.
#[tokio::test]
async fn without_isolation_a_pending_reply_fails_the_later_attempts() {
    let mock = replying_mock();
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let cat = category(dir.path());
    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let report = run_category(&rt, &url, &cat, &opts(3, 1)).await;
    assert_eq!(report.passed(), 1, "{:#?}", report.tasks);
    assert_eq!(report.isolation, None);
}

#[tokio::test]
async fn with_isolation_every_attempt_gets_a_fresh_tool_server() {
    let lanes = vec![replying_mock(), replying_mock()];
    let mut urls = Vec::new();
    for m in &lanes {
        urls.push(spawn(m.clone()).await);
    }
    let dir = tempfile::tempdir().unwrap();
    let cat = category(dir.path());
    let mut o = opts(5, 1);
    o.isolation = Some(Isolation {
        lanes: urls
            .iter()
            .map(|u| Runtime::new(u, TOKEN, Some(CONSOLE_TOKEN)))
            .collect(),
        urls: urls.clone(),
        fresh: fresh_fn(lanes.clone()),
        how: "the test's reset".into(),
    });
    // `rt` is not used with isolation: a runtime that isn't there proves it.
    let unused = Runtime::new("http://127.0.0.1:1", TOKEN, None);
    let report = run_category(&unused, "http://127.0.0.1:1", &cat, &o).await;

    assert_eq!(report.passed(), 5, "{:#?}", report.tasks);
    // One fresh tool server per attempt, and every attempt on one of the lanes.
    let fresh: usize = lanes.iter().map(|m| *m.fresh_calls.lock().unwrap()).sum();
    assert_eq!(fresh, 5);
    let created: usize = lanes.iter().map(|m| m.created_bodies().len()).sum();
    assert_eq!(created, 5);
    // Both lanes worked at once (concurrency 1 is ignored: one attempt per lane).
    assert!(lanes.iter().all(|m| !m.created_bodies().is_empty()));
    // A lane runs one attempt at a time: each task there started after the one before ended.
    for m in &lanes {
        let spans = m.spans();
        for w in spans.windows(2) {
            assert!(w[1].0 >= w[0].1, "two attempts overlapped on one lane");
        }
    }
    let what = report.isolation.as_deref().unwrap();
    assert!(
        what.contains("a fresh tool server for every attempt"),
        "{what}"
    );
    assert!(what.contains("2 lanes"), "{what}");
    assert_eq!(report.runtime_url, urls.join(", "));
    let md = report.to_markdown();
    assert!(
        md.contains("| Isolation | a fresh tool server for every attempt"),
        "{md}"
    );
}

/// A lane that can't get a fresh tool server runs nothing there: the attempt fails as the
/// harness's (never the bot's), and the next attempt tries again.
#[tokio::test]
async fn a_failed_fresh_start_is_a_harness_failure_not_a_bot_one() {
    let mock = replying_mock();
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let cat = category(dir.path());
    let calls = Arc::new(std::sync::Mutex::new(0usize));
    let c = calls.clone();
    let m = mock.clone();
    let fresh: FreshFn = Arc::new(move |_lane| {
        let first = {
            let mut n = c.lock().unwrap();
            *n += 1;
            *n == 1
        };
        let m = m.clone();
        Box::pin(async move {
            if first {
                Err("the tool server didn't come up".to_string())
            } else {
                m.fresh();
                Ok(())
            }
        })
    });
    let mut o = opts(2, 1);
    o.isolation = Some(Isolation {
        lanes: vec![Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN))],
        urls: vec![url.clone()],
        fresh,
        how: "test".into(),
    });
    let report = run_category(&Runtime::new(&url, TOKEN, None), &url, &cat, &o).await;
    assert_eq!(report.passed(), 1);
    let failed = report.tasks.iter().find(|t| !t.passed).unwrap();
    let tag = failed.tag.as_ref().unwrap();
    assert_eq!(tag.sub.as_deref(), Some("harness/runtime-unreachable"));
    assert!(failed.failures[0]
        .to_string()
        .contains("no fresh tool server for lane 1: the tool server didn't come up"));
    assert_eq!(
        mock.created_bodies().len(),
        1,
        "nothing ran on the failed lane"
    );
}

/// The CLI: `--lanes` and `--fresh-cmd` (both or neither), and the command runs as
/// `<program> <lane>` before each attempt.
#[tokio::test]
async fn cli_runs_the_fresh_command_before_each_attempt() {
    // The command can't reach the mocks' state, so it only records which lane it was run for,
    // and these mocks have no one-reply rule.
    let lanes = vec![Arc::new(Mock::default()), Arc::new(Mock::default())];
    let mut urls = Vec::new();
    for m in &lanes {
        urls.push(spawn(m.clone()).await);
    }
    let dir = tempfile::tempdir().unwrap();
    category(dir.path());
    common::write_taxonomy(dir.path());
    let root = dir.path().to_path_buf();
    for m in &lanes {
        m.script(
            ASK,
            Script::drafting(vec![(
                "slack.post_reply",
                json!({"channel": "C0FIELD", "thread_ts": "1731500000.000100", "text": "Yes."}),
            )]),
        );
    }
    let log = root.join("fresh.log");
    let script = root.join("fresh.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho \"lane $1\" >> '{}'\n", log.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let failing = root.join("fail.sh");
    std::fs::write(&failing, "#!/bin/sh\necho 'lane down' >&2\nexit 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
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
                .env_remove("PMOS_EVALS_LANES")
                .env_remove("PMOS_EVALS_FRESH_CMD")
                .output()
                .unwrap()
        })
    };
    let base = |extra: &[&str]| {
        let mut v: Vec<String> = [
            "run",
            "--category",
            "cat",
            "--token",
            TOKEN,
            "--as",
            "ana",
            "--repeats",
            "3",
            "--date",
            "2026-11-22",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    };
    let lanes_arg = urls.join(",");

    let only_lanes = run(base(&["--lanes", &lanes_arg])).await.unwrap();
    assert_eq!(only_lanes.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&only_lanes.stderr).contains("--lanes needs --fresh-cmd"));
    let only_cmd = run(base(&["--fresh-cmd", script.to_str().unwrap()]))
        .await
        .unwrap();
    assert_eq!(only_cmd.status.code(), Some(2));

    let out = run(base(&[
        "--lanes",
        &lanes_arg,
        "--fresh-cmd",
        script.to_str().unwrap(),
    ]))
    .await
    .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = logged.lines().collect();
    assert_eq!(lines.len(), 3, "{logged}");
    assert!(
        lines.iter().all(|l| *l == "lane 1" || *l == "lane 2"),
        "{logged}"
    );
    let json = std::fs::read_to_string(root.join("reports/2026-11-22-cat.json")).unwrap();
    assert!(
        json.contains("a fresh tool server for every attempt"),
        "{json}"
    );

    // A command that fails: every attempt fails as the harness's, with its stderr.
    let out = run(base(&[
        "--lanes",
        &urls[0],
        "--fresh-cmd",
        failing.to_str().unwrap(),
        "--reports-dir",
        root.join("failing").to_str().unwrap(),
    ]))
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let md = std::fs::read_dir(root.join("failing"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "md"))
        .unwrap();
    let md = std::fs::read_to_string(md).unwrap();
    assert!(md.contains("harness/runtime-unreachable"), "{md}");
    assert!(md.contains("lane down"), "{md}");
}
