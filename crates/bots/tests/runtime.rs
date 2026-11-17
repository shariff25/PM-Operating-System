//! Durability and the clock: lease recovery after a restart (D2), the routine scheduler
//! (D8), and trace redaction and retention (D6). The runtime's methods take `now`, so each
//! case moves the clock instead of waiting. Every case runs on both stores.

#[macro_use]
mod common;

use std::time::Duration;

use chrono::Duration as Span;
use common::{at, Env};
use pmos_bots::model::{
    Draft, Output, Routine, Schedule, Source, TaskStatus, TraceKind, GAVE_UP_REASON, RETRY_EVENT,
};
use pmos_bots::store::TaskFilter;
use serde_json::json;

async fn queued_task(env: &Env) -> String {
    env.ask("What changed overnight?").await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ----- leases ---------------------------------------------------------------------------------

/// A worker that vanishes: the task is retried once, then failed with the stated reason.
async fn an_interrupted_task_is_retried_once_then_failed(env: &Env) {
    let tid = queued_task(env).await;
    let t0 = at(2026, 11, 14, 10, 0);
    let claim = env.rt.claim_now("w1", None, t0).await.unwrap().unwrap();
    assert_eq!(claim.task.attempts, 1);
    assert_eq!(claim.lease_until, t0 + Span::seconds(120));

    // Within the lease nothing happens.
    let r = env
        .rt
        .recover_leases(t0 + Span::seconds(119))
        .await
        .unwrap();
    assert_eq!((r.requeued, r.failed), (0, 0));

    // Lapsed: back in the queue, with the event saying why.
    let t1 = t0 + Span::seconds(121);
    let r = env.rt.recover_leases(t1).await.unwrap();
    assert_eq!((r.requeued, r.failed), (1, 0));
    let task = env.rt.task(&tid).await.unwrap();
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(task.worker, None);
    assert_eq!(task.lease_until, None);
    assert_eq!(task.attempts, 1);
    let events = env.store.events(&tid).await.unwrap();
    assert_eq!(events.last().unwrap().text, RETRY_EVENT);
    assert!(env.feed().await.is_empty(), "a retry isn't announced");

    // The second run is interrupted too: failed, and the PM is told.
    let claim = env.rt.claim_now("w2", None, t1).await.unwrap().unwrap();
    assert_eq!(claim.task.attempts, 2);
    let r = env
        .rt
        .recover_leases(t1 + Span::seconds(121))
        .await
        .unwrap();
    assert_eq!((r.requeued, r.failed), (0, 1));
    let task = env.rt.task(&tid).await.unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.error.as_deref(), Some(GAVE_UP_REASON));
    let feed = env.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0]["kind"], "failed");
    assert_eq!(feed[0]["text"], "interrupted twice by restarts; ask again");
    // Nothing more to recover.
    let r = env
        .rt
        .recover_leases(t1 + Span::seconds(600))
        .await
        .unwrap();
    assert_eq!((r.requeued, r.failed), (0, 0));
}

async fn heartbeats_keep_the_lease(env: &Env) {
    let tid = queued_task(env).await;
    let t0 = at(2026, 11, 14, 10, 0);
    env.rt.claim_now("w1", None, t0).await.unwrap().unwrap();
    let until = env
        .rt
        .heartbeat(&tid, Some("w1"), t0 + Span::seconds(100))
        .await
        .unwrap();
    assert_eq!(until, t0 + Span::seconds(220));
    let r = env
        .rt
        .recover_leases(t0 + Span::seconds(200))
        .await
        .unwrap();
    assert_eq!(r.requeued, 0);
    assert_eq!(env.rt.task(&tid).await.unwrap().status, TaskStatus::Working);
}

/// After recovery a new worker holds the task; the old one's late result is refused.
async fn a_late_worker_cannot_finish_a_reclaimed_task(env: &Env) {
    let tid = queued_task(env).await;
    let t0 = at(2026, 11, 14, 10, 0);
    env.rt.claim_now("w1", None, t0).await.unwrap().unwrap();
    env.rt
        .recover_leases(t0 + Span::seconds(121))
        .await
        .unwrap();
    env.rt
        .claim_now("w2", None, t0 + Span::seconds(122))
        .await
        .unwrap()
        .unwrap();
    let (status, body) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "stale", "worker": "w1"}),
        )
        .await;
    assert_eq!(status, 409, "{body}");
    let (status, _) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "fresh", "worker": "w2"}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        env.rt.task(&tid).await.unwrap().result.as_deref(),
        Some("fresh")
    );
}

/// The start-up pass: a task left `working` by the last process is recovered as soon as the
/// loops start, without waiting 30 seconds.
async fn recovery_runs_on_start_up(env: &Env) {
    let tid = queued_task(env).await;
    let long_ago = pmos_bots::model::now() - Span::minutes(10);
    env.rt
        .claim_now("w-before-restart", None, long_ago)
        .await
        .unwrap()
        .unwrap();
    let loops = env.rt.spawn_loops();
    let mut status = TaskStatus::Working;
    for _ in 0..50 {
        status = env.rt.task(&tid).await.unwrap().status;
        if status == TaskStatus::Queued {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status, TaskStatus::Queued);
    env.rt.stop.cancel();
    for l in loops {
        l.await.unwrap();
    }
}

// ----- routines -------------------------------------------------------------------------------

fn draft(bot: &str, owner: &str, cron: &str) -> Draft {
    Draft {
        bot: bot.into(),
        owner: owner.into(),
        schedule: Schedule {
            cron: cron.into(),
            tz: "Europe/London".into(),
        },
        prompt: "Summarize overnight #launch".into(),
        output: Output::dm(),
        approval: "approve".into(),
    }
}

async fn routine_tasks(env: &Env) -> Vec<pmos_bots::model::Task> {
    env.store
        .tasks(&TaskFilter {
            source: Some(Source::Routine),
            limit: 100,
            ..TaskFilter::default()
        })
        .await
        .unwrap()
}

/// Monday 2026-11-16, 8:00 London is 8:00 UTC (winter time).
async fn a_due_routine_is_queued_once(env: &Env) {
    let created = at(2026, 11, 14, 12, 0); // a Saturday
    let r = env
        .rt
        .create_routine(draft("pm-ana", "ana", "0 8 * * 1-5"), created)
        .await
        .unwrap();
    assert_eq!(r.next_run_at, Some(at(2026, 11, 16, 8, 0)));

    assert_eq!(
        env.rt
            .run_due_routines(at(2026, 11, 16, 7, 59))
            .await
            .unwrap(),
        0
    );
    let now = at(2026, 11, 16, 8, 0);
    assert_eq!(env.rt.run_due_routines(now).await.unwrap(), 1);
    assert_eq!(
        env.rt
            .run_due_routines(now + Span::seconds(30))
            .await
            .unwrap(),
        0,
        "the next tick doesn't run it again"
    );
    let tasks = routine_tasks(env).await;
    assert_eq!(tasks.len(), 1);
    let t = &tasks[0];
    assert_eq!(t.status, TaskStatus::Queued);
    assert_eq!(t.bot, "pm-ana");
    assert_eq!(t.requested_by, "ana");
    assert_eq!(t.text, "Summarize overnight #launch");
    assert_eq!(t.priority.as_str(), "scheduled");
    assert_eq!(t.routine_id.as_deref(), Some(r.id.as_str()));
    assert_eq!(t.slack, None, "the result goes to the owner's DM");
    let r = env.rt.routine(&r.id).await.unwrap();
    assert_eq!(r.last_run_at, Some(now));
    assert_eq!(r.next_run_at, Some(at(2026, 11, 17, 8, 0)));

    // A worker picks it up like any task.
    let claim = env.rt.claim_now("w1", None, now).await.unwrap().unwrap();
    assert_eq!(claim.task.id, t.id);
    assert_eq!(claim.gateway.headers["X-Pmos-Priority"], "scheduled");
}

/// Down from Monday 7:00 to Thursday 9:00: three slots were missed, and it runs once.
async fn missed_runs_during_downtime_run_once(env: &Env) {
    let r = env
        .rt
        .create_routine(
            draft("pm-ana", "ana", "0 8 * * 1-5"),
            at(2026, 11, 16, 7, 0),
        )
        .await
        .unwrap();
    let back = at(2026, 11, 19, 9, 0);
    assert_eq!(env.rt.run_due_routines(back).await.unwrap(), 1);
    assert_eq!(env.rt.run_due_routines(back).await.unwrap(), 0);
    assert_eq!(routine_tasks(env).await.len(), 1);
    let r = env.rt.routine(&r.id).await.unwrap();
    assert_eq!(
        r.next_run_at,
        Some(at(2026, 11, 20, 8, 0)),
        "next is Friday's slot"
    );
}

async fn paused_routines_do_not_run(env: &Env) {
    let r = env
        .rt
        .create_routine(
            draft("pm-ana", "ana", "0 8 * * 1-5"),
            at(2026, 11, 14, 12, 0),
        )
        .await
        .unwrap();
    let paused = env
        .rt
        .update_routine(
            &r.id,
            pmos_bots::runtime::RoutineChange {
                enabled: Some(false),
                ..Default::default()
            },
            at(2026, 11, 15, 12, 0),
        )
        .await
        .unwrap();
    assert!(!paused.enabled);
    assert_eq!(
        env.rt
            .run_due_routines(at(2026, 11, 18, 9, 0))
            .await
            .unwrap(),
        0
    );
    // Resumed on Wednesday: the slots it slept through don't run; the next one does.
    let resumed = env
        .rt
        .update_routine(
            &r.id,
            pmos_bots::runtime::RoutineChange {
                enabled: Some(true),
                ..Default::default()
            },
            at(2026, 11, 18, 9, 0),
        )
        .await
        .unwrap();
    assert_eq!(resumed.next_run_at, Some(at(2026, 11, 19, 8, 0)));
    assert_eq!(
        env.rt
            .run_due_routines(at(2026, 11, 18, 9, 1))
            .await
            .unwrap(),
        0
    );
    assert!(routine_tasks(env).await.is_empty());
}

/// A routine whose owner may no longer use its bot (not in the user directory, the bot was
/// removed) is paused, and the owner is told why, rather than silently skipped.
async fn a_routine_that_can_no_longer_run_is_paused_with_a_reason(env: &Env) {
    let now = at(2026, 11, 16, 8, 0);
    let mut stale = Routine {
        id: pmos_bots::model::new_routine_id(),
        bot: "docs".into(),
        owner: "eve".into(),
        enabled: true,
        schedule: Schedule {
            cron: "0 8 * * *".into(),
            tz: "UTC".into(),
        },
        prompt: "p".into(),
        output: Output::dm(),
        approval: "approve".into(),
        next_run_at: Some(now),
        last_run_at: None,
        created_at: now,
        updated_at: now,
    };
    env.store.create_routine(&stale).await.unwrap();
    stale.id = pmos_bots::model::new_routine_id();
    stale.bot = "retired".into();
    stale.owner = "ana".into();
    env.store.create_routine(&stale).await.unwrap();

    assert_eq!(env.rt.run_due_routines(now).await.unwrap(), 2);
    for r in env.store.routines(None).await.unwrap() {
        assert!(!r.enabled);
        assert_eq!(r.next_run_at, None);
    }
    let tasks = routine_tasks(env).await;
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t.status == TaskStatus::Failed));
    let feed = env.feed().await;
    let texts: Vec<&str> = feed.iter().map(|n| n["text"].as_str().unwrap()).collect();
    assert!(texts.iter().any(
        |t| t.starts_with("Your routine is paused:") && t.contains("`eve` isn't a PM OS user")
    ));
    assert!(texts
        .iter()
        .any(|t| t.contains("its bot `retired` no longer exists")));
    // Nothing is claimable from them.
    assert!(env.rt.claim_now("w1", None, now).await.unwrap().is_none());
}

// ----- traces ---------------------------------------------------------------------------------

async fn housekeeping_redacts_slack_after_a_day_and_drops_traces_after_thirty(env: &Env) {
    let tid = queued_task(env).await;
    let now = at(2026, 12, 15, 12, 0);
    let slack = |t: &str| json!({"tool": "slack.read_thread", "result": {"text": t}});
    for (when, body) in [
        (now - Span::days(31), slack("very old")),
        (now - Span::hours(25), slack("yesterday")),
        (now - Span::hours(23), slack("today")),
        (
            now - Span::hours(25),
            json!({"tool": "confluence.read", "result": "page"}),
        ),
    ] {
        env.store
            .add_trace(&tid, &[(TraceKind::ToolResult, body)], when)
            .await
            .unwrap();
    }
    let h = env.rt.housekeeping(now).await.unwrap();
    assert_eq!(h.traces_pruned, 1);
    assert_eq!(
        h.redacted, 2,
        "the 31-day-old one was redacted, then pruned"
    );
    let trace = env.store.trace(&tid).await.unwrap();
    let bodies: Vec<_> = trace.iter().map(|e| e.body.clone()).collect();
    assert_eq!(
        bodies,
        vec![
            json!({"redacted": "slack content removed after 24 h"}),
            slack("today"),
            json!({"tool": "confluence.read", "result": "page"}),
        ]
    );
    let again = env.rt.housekeeping(now).await.unwrap();
    assert_eq!((again.redacted, again.traces_pruned), (0, 0));
}

both_envs!(
    an_interrupted_task_is_retried_once_then_failed,
    heartbeats_keep_the_lease,
    a_late_worker_cannot_finish_a_reclaimed_task,
    recovery_runs_on_start_up,
    a_due_routine_is_queued_once,
    missed_runs_during_downtime_run_once,
    paused_routines_do_not_run,
    a_routine_that_can_no_longer_run_is_paused_with_a_reason,
    housekeeping_redacts_slack_after_a_day_and_drops_traces_after_thirty,
);
