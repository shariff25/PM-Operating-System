//! Tasks with a `thread`: parsing and validation, and a run against the mocked runtime.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use common::{spawn, write_category, Mock, Script, TOKEN};
use pmos_evals::client::Runtime;
use pmos_evals::run::{run_category, Follow, RunOptions};
use pmos_evals::spec::load_category;
use pmos_evals::tag::Tag;

fn opts(timeout_ms: u64, repeats: u32) -> RunOptions {
    RunOptions {
        requested_by: "ana".into(),
        follow: Follow {
            poll: Duration::from_millis(10),
            timeout: Duration::from_millis(timeout_ms),
            cancel_wait: Duration::from_secs(1),
        },
        concurrency: 4,
        date: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        note: None,
        gateway_workarounds: None,
        only: Vec::new(),
        repeats,
        decline_phrases: vec!["don't answer".into()],
        judges: None,
        judges_skipped: None,
        isolation: None,
    }
}

const THREAD: &str = "id: t-thread
thread:
  - ask: \"Write a PRD for dark mode.\"
    expect:
      must_include: [\"?\"]
  - ask: \"customer\"
    expect:
      must_not_include: [\"who is the user\"]
  - ask: \"Now draft it.\"
";

fn says(text: &str) -> Script {
    Script {
        result: Some(text.into()),
        ..Script::default()
    }
}

async fn run_thread(
    mock: Arc<Mock>,
    yaml: &str,
    timeout_ms: u64,
    repeats: u32,
) -> pmos_evals::report::Report {
    let url = spawn(mock).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "docs", 0.5, &[("t-thread", yaml.into())]);
    let category = load_category(dir.path(), "cat").unwrap();
    let rt = Runtime::new(&url, TOKEN, None);
    run_category(&rt, &url, &category, &opts(timeout_ms, repeats)).await
}

fn mock_with_answers() -> Arc<Mock> {
    let mock = Arc::new(Mock::default());
    mock.script("Write a PRD for dark mode.", says("Who is the user?"));
    mock.script("customer", says("Got it: customers who run the console."));
    mock.script("Now draft it.", says("Here is the draft."));
    mock
}

// ---- spec ----

fn load_one(yaml: &str) -> Result<pmos_evals::spec::Category, Vec<String>> {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "docs", 0.5, &[("t-thread", yaml.into())]);
    load_category(dir.path(), "cat").map_err(|e| e.0)
}

#[test]
fn a_thread_task_loads() {
    let c = load_one(THREAD).unwrap();
    let t = &c.tasks[0];
    assert!(t.is_thread());
    assert_eq!(t.thread.len(), 3);
    assert_eq!(t.first_ask(), "Write a PRD for dark mode.");
    assert!(t.thread[0].expect.is_some() && t.thread[2].expect.is_none());
    assert_eq!(t.cluster(), "t-thread");
}

#[test]
fn a_task_has_ask_or_thread_never_both() {
    let both = format!("{THREAD}ask: \"one more\"\n");
    let errors = load_one(&both).unwrap_err();
    assert!(
        errors.iter().any(
            |e| e.contains("t-thread.yaml") && e.contains("either `ask` or `thread`, not both")
        ),
        "{errors:#?}"
    );
    let neither = load_one("id: t-thread\nexpect:\n  approvals: 0\n").unwrap_err();
    assert!(neither
        .iter()
        .any(|e| e.contains("`ask` is empty; a task has `ask`")));
}

#[test]
fn a_thread_is_validated_turn_by_turn() {
    let top = format!("{THREAD}expect:\n  approvals: 0\n");
    assert!(load_one(&top)
        .unwrap_err()
        .iter()
        .any(|e| e.contains("no top-level `expect`")));

    let one = "id: t-thread\nthread:\n  - ask: \"x\"\n    expect: {must_include: [a]}\n";
    assert!(load_one(one)
        .unwrap_err()
        .iter()
        .any(|e| e.contains("at least two turns")));

    let ungraded = "id: t-thread\nthread:\n  - ask: \"x\"\n  - ask: \"y\"\n";
    assert!(load_one(ungraded)
        .unwrap_err()
        .iter()
        .any(|e| e.contains("no turn of the `thread` has an `expect`")));

    let bad = "id: t-thread\nthread:\n  - ask: \" \"\n    expect: {}\n  - ask: \"y\"\n    expect: {must_cite: [/docs/p]}\n";
    let errors = load_one(bad).unwrap_err();
    for want in [
        "thread turn 1: `ask` is empty",
        "thread turn 1: `expect` has no checks",
        "thread turn 2: must_cite `/docs/p` is not an absolute http(s) URL",
    ] {
        assert!(
            errors.iter().any(|e| e.contains(want)),
            "{want}: {errors:#?}"
        );
    }
    let unknown = "id: t-thread\nthread:\n  - ask: \"x\"\n    expects: {}\n  - ask: \"y\"\n";
    assert!(load_one(unknown)
        .unwrap_err()
        .iter()
        .any(|e| e.contains("unknown field `expects`")));
}

// ---- running ----

#[tokio::test]
async fn a_thread_runs_in_one_conversation_and_passes() {
    let mock = mock_with_answers();
    let report = run_thread(mock.clone(), THREAD, 1500, 1).await;
    assert_eq!(report.tasks.len(), 1, "one line for the whole thread");
    let t = &report.tasks[0];
    assert!(t.passed, "{:?}", t.failures);
    assert_eq!(t.failed_turn, None);
    assert_eq!(t.ask, "Write a PRD for dark mode.");
    let verdicts: Vec<_> = t.turns.iter().map(|u| u.verdict()).collect();
    assert_eq!(verdicts, ["pass", "pass", "not graded"]);

    // Three runtime tasks, in order, one conversation, each an eval for the same bot and PM.
    let bodies = mock.created_bodies();
    let texts: Vec<_> = bodies.iter().map(|b| b["text"].as_str().unwrap()).collect();
    assert_eq!(
        texts,
        ["Write a PRD for dark mode.", "customer", "Now draft it."]
    );
    let conv = bodies[0]["conversation"].as_str().unwrap();
    assert!(conv.starts_with("eval-t-thread-r1-") && conv.len() <= 128);
    assert!(bodies.iter().all(|b| b["conversation"] == conv
        && b["source"] == "eval"
        && b["bot"] == "docs"
        && b["requested_by"] == "ana"));
    let ids: Vec<_> = t.turns.iter().map(|u| u.task_id.clone().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.windows(2).all(|w| w[0] != w[1]));

    let md = report.to_markdown();
    assert!(md.contains("## Threads"), "{md}");
    assert!(md.contains("  - turn 1: pass"), "{md}");
    assert!(md.contains("  - turn 3: not graded"), "{md}");
    assert!(report.passed() == 1 && report.total() == 1);
}

#[tokio::test]
async fn each_repeat_gets_a_fresh_conversation() {
    let mock = mock_with_answers();
    let report = run_thread(mock.clone(), THREAD, 1500, 2).await;
    assert_eq!(report.tasks.len(), 2);
    assert!(report.tasks.iter().all(|t| t.passed));
    let bodies = mock.created_bodies();
    assert_eq!(bodies.len(), 6, "the whole thread ran twice");
    let mut convs: Vec<&str> = bodies
        .iter()
        .map(|b| b["conversation"].as_str().unwrap())
        .collect();
    convs.sort();
    convs.dedup();
    assert_eq!(convs.len(), 2);
    assert!(convs.iter().any(|c| c.contains("-r1-")) && convs.iter().any(|c| c.contains("-r2-")));
}

#[tokio::test]
async fn a_failing_second_turn_fails_the_thread_and_names_the_turn() {
    let mock = mock_with_answers();
    mock.script("customer", says("Who is the user? Please tell me."));
    let report = run_thread(mock.clone(), THREAD, 1500, 1).await;
    let t = &report.tasks[0];
    assert!(!t.passed);
    assert_eq!(t.failed_turn, Some(2));
    let tag = t.tag.as_ref().unwrap();
    assert_eq!(tag.tag, Tag::Model);
    assert!(tag.reason.starts_with("turn 2 of 3: "), "{}", tag.reason);
    let verdicts: Vec<_> = t.turns.iter().map(|u| u.verdict()).collect();
    assert_eq!(
        verdicts,
        ["pass", "FAIL", "not graded"],
        "a wrong answer doesn't stop the thread"
    );
    assert_eq!(mock.created_bodies().len(), 3);

    let md = report.to_markdown();
    assert!(md.contains("turn 2: "), "{md}");
    assert!(md.contains("- **Failed at:** turn 2 of 3"), "{md}");
    assert!(
        md.contains("  - turn 2: FAIL (model/forbidden-text)"),
        "{md}"
    );
    // The JSON keeps the turns and no text.
    let json = serde_json::to_string(&report).unwrap();
    let back: pmos_evals::report::Report = serde_json::from_str(&json).unwrap();
    assert_eq!(back, report);
    assert!(!json.contains("Who is the user"));
}

#[tokio::test]
async fn a_first_turn_that_fails_still_runs_the_rest() {
    let mock = mock_with_answers();
    mock.script("Write a PRD for dark mode.", says("Here is a PRD."));
    let report = run_thread(mock.clone(), THREAD, 1500, 1).await;
    let t = &report.tasks[0];
    assert_eq!(t.failed_turn, Some(1));
    assert_eq!(t.turns.len(), 3);
    assert!(t.turns[1].passed);
}

#[tokio::test]
async fn a_turn_that_times_out_ends_the_thread_there() {
    let mock = mock_with_answers();
    mock.script(
        "customer",
        Script {
            hangs: true,
            events: false,
            ..Script::default()
        },
    );
    let report = run_thread(mock.clone(), THREAD, 300, 1).await;
    let t = &report.tasks[0];
    assert!(!t.passed);
    assert_eq!(t.failed_turn, Some(2));
    let tag = t.tag.as_ref().unwrap();
    assert_eq!(tag.tag, Tag::Harness);
    assert_eq!(tag.rule, "silent-timeout");
    assert!(tag.reason.starts_with("turn 2 of 3: "));
    let verdicts: Vec<_> = t.turns.iter().map(|u| u.verdict()).collect();
    assert_eq!(verdicts, ["pass", "FAIL", "not run"]);
    assert_eq!(
        mock.created_bodies().len(),
        2,
        "the turn after a timeout is not asked"
    );
    assert!(t.turns[2].skipped && t.turns[2].task_id.is_none());
    assert!(report.to_markdown().contains("turn 3: not run"));
}

#[tokio::test]
async fn the_timeout_is_per_turn() {
    // Each turn takes 200 ms; the thread takes 600 ms, far beyond a single 400 ms timeout.
    let mock = Arc::new(Mock::default());
    for ask in ["Write a PRD for dark mode.", "customer", "Now draft it."] {
        mock.script(
            ask,
            Script {
                queue_ms: 10,
                work_ms: 190,
                result: Some("Who is the user?".into()),
                ..Script::default()
            },
        );
    }
    let yaml = "id: t-thread
thread:
  - ask: \"Write a PRD for dark mode.\"
    expect: {must_include: [\"?\"]}
  - ask: \"customer\"
  - ask: \"Now draft it.\"
    expect: {must_include: [\"?\"]}
";
    let report = run_thread(mock, yaml, 400, 1).await;
    assert!(report.tasks[0].passed, "{:?}", report.tasks[0].failures);
}

#[tokio::test]
async fn a_runtime_that_refuses_conversations_is_a_harness_failure_with_a_hint() {
    let mock = Arc::new(Mock {
        reject_conversation: true,
        ..Mock::default()
    });
    let report = run_thread(mock.clone(), THREAD, 1000, 1).await;
    let t = &report.tasks[0];
    assert!(!t.passed);
    assert_eq!(t.failed_turn, Some(1));
    let tag = t.tag.as_ref().unwrap();
    assert_eq!(tag.rule, "runtime-unreachable");
    assert!(
        tag.reason.contains("name a task's conversation"),
        "{}",
        tag.reason
    );
    assert!(tag.reason.contains("docs/evals.md"), "{}", tag.reason);
    assert_eq!(t.turns.iter().filter(|u| u.skipped).count(), 2);
    assert!(mock.created_bodies().is_empty());
}

#[tokio::test]
async fn a_thread_turn_checks_its_own_drafts() {
    let mock = Arc::new(Mock::default());
    mock.script("Draft the story.", says("What project?"));
    mock.script(
        "Project PM, as a task.",
        Script::drafting(vec![(
            "jira.create_issue",
            serde_json::json!({"project": "PM", "issue_type": "Task", "summary": "SAML"}),
        )]),
    );
    let yaml = "id: t-thread
thread:
  - ask: \"Draft the story.\"
    expect:
      approvals: 0
  - ask: \"Project PM, as a task.\"
    expect:
      approvals: 1
      drafts:
        - tool: jira.create_issue
          count: 1
          allowed: {issue_type: [Task]}
";
    let report = run_thread(mock.clone(), yaml, 1500, 1).await;
    assert!(report.tasks[0].passed, "{:?}", report.tasks[0].failures);
    assert_eq!(mock.drafts_read.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_single_ask_is_unchanged() {
    let mock = Arc::new(Mock::default());
    mock.script("one", says("fine, 42"));
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[(
            "t-one",
            "id: t-one\nask: \"one\"\nexpect:\n  must_include: [\"42\"]\n".into(),
        )],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    assert!(!category.tasks[0].is_thread());
    let rt = Runtime::new(&url, TOKEN, None);
    let report = run_category(&rt, &url, &category, &opts(1000, 1)).await;
    let t = &report.tasks[0];
    assert!(t.passed && t.turns.is_empty() && t.failed_turn.is_none());
    assert!(
        mock.created_bodies()[0].get("conversation").is_none(),
        "a single ask names no conversation"
    );
    let md = report.to_markdown();
    assert!(!md.contains("## Threads"));
    assert!(!serde_json::to_string(&report).unwrap().contains("turns"));
}
