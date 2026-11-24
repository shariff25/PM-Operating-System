//! A task the runner times out is cancelled on the runtime, and the runner waits briefly for
//! its worker to let it go before the lane's next attempt (`docs/evals.md`, "Timeouts"). Eval
//! run E3 lost 10 to 166 s of the next attempt's time to the old task still running.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use common::{spawn, write_category, Mock, Script, CONSOLE_TOKEN, TOKEN};
use pmos_evals::client::Runtime;
use pmos_evals::report::Cancel;
use pmos_evals::run::{follow_task, run_category, Follow, RunOptions};
use pmos_evals::spec::load_category;

fn follow(timeout_ms: u64, cancel_wait_ms: u64) -> Follow {
    Follow {
        poll: Duration::from_millis(10),
        timeout: Duration::from_millis(timeout_ms),
        cancel_wait: Duration::from_millis(cancel_wait_ms),
    }
}

fn hangs(stop_after_cancel_ms: Option<i64>) -> Script {
    Script {
        queue_ms: 0,
        hangs: true,
        stop_after_cancel_ms,
        ..Script::default()
    }
}

/// One worker (a lane): the first task hangs and is timed out. Cancelled, its worker lets it
/// go, and the next task starts at once instead of waiting behind it.
#[tokio::test]
async fn a_timed_out_task_is_cancelled_and_the_next_one_starts_on_a_free_worker() {
    let mock = Arc::new(Mock {
        serial: true,
        ..Mock::default()
    });
    mock.script("stuck", hangs(Some(80)));
    mock.script(
        "next",
        Script {
            queue_ms: 0,
            work_ms: 20,
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);

    let stuck = follow_task(&rt, "docs", "ana", "stuck", None, &follow(200, 2000)).await;
    assert!(stuck.timed_out);
    // The status the runner saw before giving up stays, so the tag reads the timeout.
    assert_eq!(stuck.status.as_deref(), Some("working"));
    assert_eq!(
        *mock.cancels.lock().unwrap(),
        vec![stuck.task_id.clone().unwrap()]
    );
    let cancel = stuck.cancel.clone().unwrap();
    assert!(
        cancel.cancelled && cancel.worker_stopped == Some(true),
        "{cancel:?}"
    );
    assert!(cancel.error.is_none());
    assert!(cancel.secs >= 0.07 && cancel.secs < 1.5, "{cancel:?}");
    // Its own cancel's events aren't the task's: tagging sees only the worker's two.
    assert_eq!(stuck.events.len(), 2, "{:?}", stuck.events);
    let tag = pmos_evals::tag::tag_failure(
        &stuck,
        &pmos_evals::grade::grade(&Default::default(), &stuck, &[]),
    );
    assert_ne!(tag.rule, "cancelled");

    let next = follow_task(&rt, "docs", "ana", "next", None, &follow(2000, 2000)).await;
    assert_eq!(next.status.as_deref(), Some("done"));
    assert!(next.cancel.is_none());
    assert!(
        next.queue_secs.unwrap() < 0.3,
        "the next task waited {:?} s for the worker",
        next.queue_secs
    );
}

/// A worker that never says it stopped: the runner waits `cancel_wait`, records that, and
/// goes on.
#[tokio::test]
async fn a_worker_that_never_stops_costs_at_most_the_wait() {
    let mock = Arc::new(Mock::default());
    mock.script("stuck", hangs(None));
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let started = Instant::now();
    let stuck = follow_task(&rt, "docs", "ana", "stuck", None, &follow(100, 300)).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        stuck
            .cancel
            .as_ref()
            .map(|c| (c.cancelled, c.worker_stopped)),
        Some((true, Some(false)))
    );
}

/// A cancel the runtime refuses, or never answers, doesn't hang the run: it is recorded as
/// failed, with the reason, and nothing is waited for.
#[tokio::test]
async fn a_failed_cancel_is_recorded_and_doesnt_hang_the_run() {
    let mock = Arc::new(Mock {
        cancel_fails: Some(500),
        ..Mock::default()
    });
    mock.script("stuck", hangs(Some(10)));
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let stuck = follow_task(&rt, "docs", "ana", "stuck", None, &follow(100, 5000)).await;
    let cancel = stuck.cancel.unwrap();
    assert!(!cancel.cancelled && cancel.worker_stopped.is_none());
    assert!(cancel.error.unwrap().contains("500"));
    assert!(cancel.secs < 1.0);

    let mock = Arc::new(Mock {
        cancel_hangs: true,
        ..Mock::default()
    });
    mock.script("stuck", hangs(Some(10)));
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let started = Instant::now();
    let stuck = follow_task(&rt, "docs", "ana", "stuck", None, &follow(100, 5000)).await;
    // The cancel call gives up within its own bound (15 s; the client's 30 s never comes).
    assert!(started.elapsed() < Duration::from_secs(20));
    let cancel = stuck.cancel.unwrap();
    assert!(!cancel.cancelled);
    assert!(cancel.error.unwrap().contains("didn't answer the cancel"));
}

/// A task still queued at the timeout is cancelled with no worker to wait for.
#[tokio::test]
async fn a_queued_task_is_cancelled_without_waiting() {
    let mock = Arc::new(Mock::default());
    mock.script(
        "queued",
        Script {
            never_claimed: true,
            ..Script::default()
        },
    );
    let url = spawn(mock.clone()).await;
    let rt = Runtime::new(&url, TOKEN, None);
    let out = follow_task(&rt, "docs", "ana", "queued", None, &follow(100, 5000)).await;
    assert_eq!(out.status.as_deref(), Some("queued"));
    let cancel = out.cancel.unwrap();
    assert!(cancel.cancelled && cancel.worker_stopped.is_none());
    assert!(cancel.secs < 1.0);
}

/// The report records the cancel on the attempt's line, and the Markdown says it.
#[tokio::test]
async fn the_report_records_that_an_attempt_timed_out_and_was_cancelled() {
    let mock = Arc::new(Mock::default());
    mock.script("stuck", hangs(Some(20)));
    mock.script("fine", Script::default());
    let url = spawn(mock.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    write_category(
        dir.path(),
        "cat",
        "docs",
        0.5,
        &[
            (
                "t-stuck",
                "id: t-stuck\nask: \"stuck\"\nexpect:\n  must_include: [\"answer\"]\n".to_string(),
            ),
            (
                "t-fine",
                "id: t-fine\nask: \"fine\"\nexpect:\n  must_include: [\"answer\"]\n".to_string(),
            ),
        ],
    );
    let category = load_category(dir.path(), "cat").unwrap();
    let rt = Runtime::new(&url, TOKEN, Some(CONSOLE_TOKEN));
    let opts = RunOptions {
        requested_by: "ana".into(),
        follow: follow(300, 2000),
        concurrency: 2,
        date: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
        note: None,
        gateway_workarounds: None,
        only: Vec::new(),
        repeats: 1,
        decline_phrases: Vec::new(),
        judges: None,
        judges_skipped: None,
        isolation: None,
    };
    let report = run_category(&rt, &url, &category, &opts).await;
    let line = |id: &str| report.tasks.iter().find(|t| t.id == id).unwrap();
    let stuck = line("t-stuck");
    assert!(!stuck.passed);
    assert!(matches!(
        stuck.cancel,
        Some(Cancel {
            cancelled: true,
            worker_stopped: Some(true),
            ..
        })
    ));
    assert!(line("t-fine").cancel.is_none());

    // The JSON keeps it, and reads back; a line without one has no `cancel` key.
    let json = serde_json::to_value(&report).unwrap();
    let lines = json["tasks"].as_array().unwrap();
    let stuck_json = lines.iter().find(|t| t["id"] == "t-stuck").unwrap();
    assert_eq!(stuck_json["cancel"]["cancelled"], true);
    assert_eq!(stuck_json["cancel"]["worker_stopped"], true);
    assert!(lines
        .iter()
        .find(|t| t["id"] == "t-fine")
        .unwrap()
        .get("cancel")
        .is_none());
    let back: pmos_evals::report::Report = serde_json::from_value(json).unwrap();
    assert_eq!(back, report);

    let md = report.to_markdown();
    assert!(
        md.contains("- **Timed out:** the runner timed out and cancelled it; its worker stopped"),
        "{md}"
    );
}
