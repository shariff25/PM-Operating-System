//! The store contract: every case runs against the memory store and Postgres, so the two
//! can't drift apart.

#[macro_use]
mod common;

use std::sync::Arc;

use chrono::Duration;
use common::at;
use pmos_bots::model::{
    ChecklistItem, Level, MemberSettings, NewNotification, NotificationKind, Output, Routine,
    Schedule, SlackRef, Source, Task, TaskStatus, TraceKind,
};
use pmos_bots::store::{
    BotsStore, Lease, Refused, RoutinePatch, RoutineRun, TaskFilter, Transition,
};
use serde_json::json;

fn task(bot: &str, user: &str, source: Source, minute: u32) -> Task {
    Task::queued(
        bot,
        user,
        source,
        &format!("question at {minute}"),
        Some(SlackRef {
            channel: "D0ANA".into(),
            thread_ts: "1731.1".into(),
        }),
        None,
        at(2026, 11, 14, 9, minute),
    )
}

fn all() -> TaskFilter {
    TaskFilter {
        limit: 100,
        ..TaskFilter::default()
    }
}

async fn tasks_round_trip_and_filter(store: Arc<dyn BotsStore>) {
    let a = task("docs", "ana", Source::Slack, 1);
    let b = task("pm-ana", "ana", Source::Routine, 2);
    let c = task("docs", "ben", Source::Eval, 3);
    for t in [&a, &b, &c] {
        store.create_task(t, "queued").await.unwrap();
    }
    assert!(
        store.create_task(&a, "queued").await.is_err(),
        "ids are unique"
    );
    assert_eq!(store.task(&a.id).await.unwrap().as_ref(), Some(&a));
    assert_eq!(store.task("tsk_missing").await.unwrap(), None);

    let ids = |v: Vec<Task>| v.into_iter().map(|t| t.id).collect::<Vec<_>>();
    assert_eq!(
        ids(store.tasks(&all()).await.unwrap()),
        vec![c.id.clone(), b.id.clone(), a.id.clone()],
        "newest first"
    );
    let f = TaskFilter {
        bot: Some("docs".into()),
        ..all()
    };
    assert_eq!(
        ids(store.tasks(&f).await.unwrap()),
        vec![c.id.clone(), a.id.clone()]
    );
    let f = TaskFilter {
        requested_by: Some("ana".into()),
        source: Some(Source::Routine),
        ..all()
    };
    assert_eq!(ids(store.tasks(&f).await.unwrap()), vec![b.id.clone()]);
    let f = TaskFilter {
        status: Some(TaskStatus::Done),
        ..all()
    };
    assert!(store.tasks(&f).await.unwrap().is_empty());
    let f = TaskFilter { limit: 1, ..all() };
    assert_eq!(ids(store.tasks(&f).await.unwrap()), vec![c.id.clone()]);

    let events = store.events(&a.id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].kind.as_str(), events[0].text.as_str()),
        ("status", "queued")
    );
}

async fn claims_go_interactive_first_then_oldest(store: Arc<dyn BotsStore>) {
    let eval = task("docs", "ben", Source::Eval, 1);
    let routine = task("docs", "ana", Source::Routine, 2);
    let slack_late = task("docs", "ana", Source::Slack, 4);
    let slack_early = task("docs", "ana", Source::Slack, 3);
    let personal = task("pm-ana", "ana", Source::Slack, 0);
    for t in [&eval, &routine, &slack_late, &slack_early, &personal] {
        store.create_task(t, "queued").await.unwrap();
    }
    let now = at(2026, 11, 14, 10, 0);
    let lease = now + Duration::seconds(120);
    let docs = vec!["docs".to_string()];
    let mut order = Vec::new();
    while let Some(t) = store.claim(Some(&docs), "w1", lease, now).await.unwrap() {
        assert_eq!(t.status, TaskStatus::Working);
        assert_eq!(t.attempts, 1);
        assert_eq!(t.worker.as_deref(), Some("w1"));
        assert_eq!(t.lease_until, Some(lease));
        assert_eq!(t.updated_at, now);
        order.push(t.id);
    }
    assert_eq!(
        order,
        vec![
            slack_early.id.clone(),
            slack_late.id.clone(),
            routine.id.clone(),
            eval.id.clone()
        ]
    );
    // The other bot's task is still there for an unfiltered worker.
    let t = store.claim(None, "w2", lease, now).await.unwrap().unwrap();
    assert_eq!(t.id, personal.id);
    assert!(store.claim(None, "w2", lease, now).await.unwrap().is_none());
    let stored = store.task(&slack_early.id).await.unwrap().unwrap();
    assert_eq!(stored.status, TaskStatus::Working);
    assert_eq!(stored.worker.as_deref(), Some("w1"));
    let events = store.events(&slack_early.id).await.unwrap();
    assert_eq!(events.last().unwrap().text, "claimed by worker w1");
}

/// Two workers asking at once for one task: exactly one gets it, every time.
async fn two_workers_never_claim_the_same_task(store: Arc<dyn BotsStore>) {
    let now = at(2026, 11, 14, 10, 0);
    let lease = now + Duration::seconds(120);
    for round in 0..20 {
        let t = task("docs", "ana", Source::Slack, round % 60);
        store.create_task(&t, "queued").await.unwrap();
        let (a, b) = tokio::join!(
            store.claim(None, "w1", lease, now),
            store.claim(None, "w2", lease, now)
        );
        let won: Vec<Task> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
        assert_eq!(won.len(), 1, "round {round}: exactly one claim");
        assert_eq!(won[0].id, t.id);
        assert_eq!(won[0].attempts, 1);
    }
}

/// A refused change must not hold the task's lock afterwards: a claim right after it has to
/// find the task. (A Postgres transaction that is dropped rather than ended keeps its row lock
/// until its connection is next used, and claims skip locked rows.)
async fn a_refused_change_leaves_the_task_claimable(store: Arc<dyn BotsStore>) {
    let now = at(2026, 11, 14, 10, 0);
    let working = Transition::from(&[TaskStatus::Working]);
    for round in 0..30 {
        let t = task("docs", "ana", Source::Slack, round % 60);
        store.create_task(&t, "queued").await.unwrap();
        assert_eq!(
            store.transition(&t.id, &working, now).await.unwrap(),
            Err(Refused::Status(TaskStatus::Queued))
        );
        let claimed = store
            .claim(None, "w1", now + Duration::seconds(120), now)
            .await
            .unwrap();
        assert_eq!(
            claimed.map(|c| c.id),
            Some(t.id.clone()),
            "round {round}: the task is claimable straight after a refused change"
        );
    }
}

async fn transitions_check_their_guards(store: Arc<dyn BotsStore>) {
    let t = task("docs", "ana", Source::Slack, 1);
    store.create_task(&t, "queued").await.unwrap();
    let now = at(2026, 11, 14, 10, 0);
    let working = Transition::from(&[TaskStatus::Working]);

    assert_eq!(
        store.transition("tsk_nope", &working, now).await.unwrap(),
        Err(Refused::NotFound)
    );
    assert_eq!(
        store.transition(&t.id, &working, now).await.unwrap(),
        Err(Refused::Status(TaskStatus::Queued))
    );
    store
        .claim(None, "w1", now + Duration::seconds(120), now)
        .await
        .unwrap()
        .unwrap();
    let other = Transition {
        worker: Some("w2".into()),
        ..working.clone()
    };
    assert_eq!(
        store.transition(&t.id, &other, now).await.unwrap(),
        Err(Refused::NotYourClaim)
    );
    let expired = Transition {
        lease_expired: true,
        ..working.clone()
    };
    assert_eq!(
        store.transition(&t.id, &expired, now).await.unwrap(),
        Err(Refused::LeaseActive)
    );

    // A full change: fields, event and notification together.
    let later = now + Duration::seconds(5);
    let change = Transition {
        worker: Some("w1".into()),
        to: Some(TaskStatus::NeedsApproval),
        lease: Lease::Clear,
        result: Some("Drafted the page.".into()),
        approvals: Some(vec!["apr_1".into(), "apr_2".into()]),
        checklist: Some(vec![ChecklistItem {
            item: "Draft".into(),
            done: true,
        }]),
        event: Some(("status".into(), "waiting for approval of 2 items".into())),
        notify: Some(NewNotification {
            approvals: vec!["apr_1".into(), "apr_2".into()],
            ..NewNotification::new(NotificationKind::NeedsApproval, "Drafted the page.")
        }),
        ..working.clone()
    };
    let done = store
        .transition(&t.id, &change, later)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, TaskStatus::NeedsApproval);
    assert_eq!(done.worker, None);
    assert_eq!(done.lease_until, None);
    assert_eq!(done.updated_at, later);
    assert_eq!(store.task(&t.id).await.unwrap().unwrap(), done);
    let events = store.events(&t.id).await.unwrap();
    assert_eq!(
        events.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        vec![
            "queued",
            "claimed by worker w1",
            "waiting for approval of 2 items"
        ]
    );
    let feed = store.notifications(0, 10).await.unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].kind, NotificationKind::NeedsApproval);
    assert_eq!(feed[0].task_id, t.id);
    assert_eq!(feed[0].bot, "docs");
    assert_eq!(feed[0].requested_by, "ana");
    assert_eq!(feed[0].slack, t.slack);
    assert_eq!(feed[0].approvals, vec!["apr_1", "apr_2"]);
    assert_eq!(feed[0].at, later);
    // A refused change writes nothing.
    assert!(store
        .transition(&t.id, &change, later)
        .await
        .unwrap()
        .is_err());
    assert_eq!(store.notifications(0, 10).await.unwrap().len(), 1);
    assert_eq!(store.events(&t.id).await.unwrap().len(), 3);
}

async fn web_tasks_round_trip_and_write_no_notifications(store: Arc<dyn BotsStore>) {
    let mut t = task("docs", "ana", Source::Web, 1);
    t.slack = None;
    assert_eq!(t.priority, pmos_bots::model::Priority::Interactive);
    store.create_task(&t, "queued").await.unwrap();
    assert_eq!(store.task(&t.id).await.unwrap().as_ref(), Some(&t));
    let f = TaskFilter {
        source: Some(Source::Web),
        ..all()
    };
    assert_eq!(store.tasks(&f).await.unwrap().len(), 1);
    let now = at(2026, 11, 14, 10, 0);
    store
        .claim(None, "w1", now + Duration::seconds(120), now)
        .await
        .unwrap()
        .unwrap();
    let done = Transition {
        worker: Some("w1".into()),
        to: Some(TaskStatus::Done),
        lease: Lease::Clear,
        result: Some("Answer.".into()),
        event: Some(("status".into(), "done".into())),
        notify: Some(NewNotification::new(NotificationKind::Done, "Answer.")),
        ..Transition::from(&[TaskStatus::Working])
    };
    store.transition(&t.id, &done, now).await.unwrap().unwrap();
    assert!(store.notifications(0, 10).await.unwrap().is_empty());
    assert_eq!(store.events(&t.id).await.unwrap().len(), 3);
}

async fn expired_leases_are_found(store: Arc<dyn BotsStore>) {
    let now = at(2026, 11, 14, 10, 0);
    let a = task("docs", "ana", Source::Slack, 1);
    let b = task("docs", "ana", Source::Slack, 2);
    let c = task("docs", "ana", Source::Slack, 3);
    for t in [&a, &b, &c] {
        store.create_task(t, "queued").await.unwrap();
    }
    store
        .claim(None, "w1", now + Duration::seconds(10), now)
        .await
        .unwrap();
    store
        .claim(None, "w1", now + Duration::seconds(200), now)
        .await
        .unwrap();
    let later = now + Duration::seconds(60);
    let expired = store.expired_leases(later).await.unwrap();
    assert_eq!(
        expired.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&a.id],
        "only working tasks past their lease; queued ones never"
    );
}

async fn notifications_are_ordered_and_paged(store: Arc<dyn BotsStore>) {
    let now = at(2026, 11, 14, 10, 0);
    let mut ids = Vec::new();
    for i in 0..5 {
        let t = task("docs", "ana", Source::Slack, i);
        store.create_task(&t, "queued").await.unwrap();
        ids.push(t.id);
    }
    for (i, id) in ids.iter().enumerate() {
        store
            .claim(None, "w1", now + Duration::seconds(120), now)
            .await
            .unwrap();
        let change = Transition {
            event: Some(("milestone".into(), format!("step {i}"))),
            notify: Some(NewNotification::new(
                NotificationKind::Milestone,
                format!("step {i}"),
            )),
            ..Transition::from(&[TaskStatus::Working])
        };
        store
            .transition(id, &change, now + Duration::seconds(i as i64))
            .await
            .unwrap()
            .unwrap();
    }
    let all = store.notifications(0, 100).await.unwrap();
    assert_eq!(all.len(), 5);
    let seqs: Vec<i64> = all.iter().map(|n| n.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order: {seqs:?}");
    assert_eq!(
        all.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
        vec!["step 0", "step 1", "step 2", "step 3", "step 4"]
    );
    assert_eq!(all[0].id, format!("ntf_{}", all[0].seq));
    let page = store.notifications(all[1].seq, 2).await.unwrap();
    assert_eq!(
        page.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
        vec!["step 2", "step 3"]
    );
    assert!(store
        .notifications(all[4].seq, 10)
        .await
        .unwrap()
        .is_empty());
    // Pruned by age only.
    assert_eq!(
        store
            .prune_notifications(now + Duration::seconds(2))
            .await
            .unwrap(),
        2
    );
    assert_eq!(store.notifications(0, 100).await.unwrap().len(), 3);
}

/// Notifications written at the same time from many tasks still get distinct, increasing
/// numbers with no gaps a reader could skip.
async fn concurrent_notifications_have_distinct_numbers(store: Arc<dyn BotsStore>) {
    let now = at(2026, 11, 14, 10, 0);
    let mut ids = Vec::new();
    for i in 0..12 {
        let t = task("docs", "ana", Source::Slack, i);
        store.create_task(&t, "queued").await.unwrap();
        store
            .claim(None, "w1", now + Duration::seconds(120), now)
            .await
            .unwrap();
        ids.push(t.id);
    }
    let mut handles = Vec::new();
    for id in ids {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let change = Transition {
                notify: Some(NewNotification::new(NotificationKind::Milestone, "m")),
                ..Transition::from(&[TaskStatus::Working])
            };
            store.transition(&id, &change, now).await.unwrap().unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let seqs: Vec<i64> = store
        .notifications(0, 100)
        .await
        .unwrap()
        .iter()
        .map(|n| n.seq)
        .collect();
    assert_eq!(seqs, (1..=12).collect::<Vec<i64>>());
}

async fn traces_redact_slack_and_expire(store: Arc<dyn BotsStore>) {
    let t = task("pm-ana", "ana", Source::Slack, 1);
    store.create_task(&t, "queued").await.unwrap();
    let day1 = at(2026, 11, 1, 9, 0);
    let day2 = at(2026, 11, 2, 12, 0);
    store
        .add_trace(
            &t.id,
            &[
                (
                    TraceKind::Prompt,
                    json!({"messages": ["what happened in #launch?"]}),
                ),
                (
                    TraceKind::ToolCall,
                    json!({"tool": "slack.search", "args": {"query": "launch"}}),
                ),
                (
                    TraceKind::ToolResult,
                    json!({"tool": "slack.search", "result": [{"text": "ship it"}]}),
                ),
                (
                    TraceKind::ToolResult,
                    json!({"name": "slack_read_thread", "result": {"text": "lgtm"}}),
                ),
                (
                    TraceKind::ToolResult,
                    json!({"tool": "jira.search", "result": []}),
                ),
            ],
            day1,
        )
        .await
        .unwrap();
    store
        .add_trace(
            &t.id,
            &[(
                TraceKind::ToolResult,
                json!({"tool": "slack.search", "result": "fresh"}),
            )],
            day2,
        )
        .await
        .unwrap();
    let trace = store.trace(&t.id).await.unwrap();
    assert_eq!(trace.len(), 6);
    assert_eq!(trace[0].kind, TraceKind::Prompt);
    assert_eq!(trace[5].body["result"], "fresh");

    // 24 hours after day 1: day 1's Slack results go, nothing else.
    let n = store
        .redact_slack(day1 + Duration::hours(24) + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(n, 2);
    let trace = store.trace(&t.id).await.unwrap();
    let redacted = json!({"redacted": "slack content removed after 24 h"});
    assert_eq!(trace[2].body, redacted);
    assert_eq!(trace[3].body, redacted);
    assert_eq!(
        trace[1].body["tool"], "slack.search",
        "calls keep their arguments"
    );
    assert_eq!(trace[4].body["tool"], "jira.search");
    assert_eq!(trace[5].body["result"], "fresh");
    assert_eq!(trace[2].at, day1, "entries keep their time");
    // Running again finds nothing new.
    assert_eq!(
        store
            .redact_slack(day1 + Duration::hours(25))
            .await
            .unwrap(),
        0
    );

    // Just over 30 days after day 1, the cut-off falls between day 1 and day 2.
    let now = day1 + Duration::days(30) + Duration::seconds(1);
    let n = store.prune_traces(now - Duration::days(30)).await.unwrap();
    assert_eq!(n, 5);
    assert_eq!(store.trace(&t.id).await.unwrap().len(), 1);
}

fn routine(owner: &str, minute: u32, next: Option<chrono::DateTime<chrono::Utc>>) -> Routine {
    let created = at(2026, 11, 1, 9, minute);
    Routine {
        id: pmos_bots::model::new_routine_id(),
        bot: "pm-ana".into(),
        owner: owner.into(),
        enabled: true,
        schedule: Schedule {
            cron: "0 8 * * 1-5".into(),
            tz: "Europe/London".into(),
        },
        prompt: "Summarize #launch".into(),
        output: Output::dm(),
        approval: "approve".into(),
        next_run_at: next,
        last_run_at: None,
        created_at: created,
        updated_at: created,
    }
}

async fn routines_crud(store: Arc<dyn BotsStore>) {
    let a = routine("ana", 1, Some(at(2026, 11, 16, 8, 0)));
    let b = routine("ben", 2, Some(at(2026, 11, 17, 8, 0)));
    store.create_routine(&a).await.unwrap();
    store.create_routine(&b).await.unwrap();
    assert_eq!(store.routine(&a.id).await.unwrap().as_ref(), Some(&a));
    assert_eq!(
        store.routines(None).await.unwrap(),
        vec![a.clone(), b.clone()]
    );
    assert_eq!(store.routines(Some("ben")).await.unwrap(), vec![b.clone()]);

    let now = at(2026, 11, 14, 12, 0);
    let patched = store
        .update_routine(
            &a.id,
            &RoutinePatch {
                enabled: Some(false),
                schedule: Some(Schedule {
                    cron: "30 9 * * 1".into(),
                    tz: "America/New_York".into(),
                }),
                prompt: Some("New prompt".into()),
                next_run_at: Some(None),
            },
            now,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!patched.enabled);
    assert_eq!(patched.schedule.cron, "30 9 * * 1");
    assert_eq!(patched.schedule.tz, "America/New_York");
    assert_eq!(patched.prompt, "New prompt");
    assert_eq!(patched.next_run_at, None);
    assert_eq!(patched.updated_at, now);
    // An empty patch changes nothing but the time.
    let same = store
        .update_routine(&a.id, &RoutinePatch::default(), now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(same, patched);
    assert_eq!(store.routine(&a.id).await.unwrap().unwrap(), patched);
    assert!(store
        .update_routine("rtn_nope", &RoutinePatch::default(), now)
        .await
        .unwrap()
        .is_none());
    assert!(store.delete_routine(&a.id).await.unwrap());
    assert!(!store.delete_routine(&a.id).await.unwrap());
    assert_eq!(store.routines(None).await.unwrap(), vec![b]);
}

async fn routine_runs_happen_once(store: Arc<dyn BotsStore>) {
    let due_at = at(2026, 11, 16, 8, 0);
    let r = routine("ana", 1, Some(due_at));
    let paused = Routine {
        enabled: false,
        ..routine("ana", 2, Some(due_at))
    };
    let later = routine("ana", 3, Some(at(2026, 11, 20, 8, 0)));
    for x in [&r, &paused, &later] {
        store.create_routine(x).await.unwrap();
    }
    let now = at(2026, 11, 16, 8, 0);
    let due = store.due_routines(now).await.unwrap();
    assert_eq!(due.iter().map(|d| &d.id).collect::<Vec<_>>(), vec![&r.id]);

    let mk = |next| RoutineRun {
        routine_id: r.id.clone(),
        due_at,
        next_run_at: next,
        task: Task::queued(
            "pm-ana",
            "ana",
            Source::Routine,
            &r.prompt,
            None,
            Some(r.id.clone()),
            now,
        ),
        event: "queued by its routine".into(),
        notify: None,
    };
    let next = at(2026, 11, 17, 8, 0);
    let run = mk(Some(next));
    assert!(store.run_routine(&run, now).await.unwrap());
    let stored = store.routine(&r.id).await.unwrap().unwrap();
    assert_eq!(stored.next_run_at, Some(next));
    assert_eq!(stored.last_run_at, Some(now));
    let t = store.task(&run.task.id).await.unwrap().unwrap();
    assert_eq!(t.routine_id.as_deref(), Some(r.id.as_str()));
    assert_eq!(t.priority, pmos_bots::model::Priority::Scheduled);
    assert_eq!(
        store.events(&t.id).await.unwrap()[0].text,
        "queued by its routine"
    );
    // The same slot again (a second tick, another process) enqueues nothing.
    assert!(!store.run_routine(&mk(Some(next)), now).await.unwrap());
    assert!(store.due_routines(now).await.unwrap().is_empty());
    // A paused routine never runs.
    let run_paused = RoutineRun {
        routine_id: paused.id.clone(),
        ..mk(Some(next))
    };
    assert!(!store.run_routine(&run_paused, now).await.unwrap());

    // A run with no next time pauses the routine.
    let r2 = routine("ana", 4, Some(due_at));
    store.create_routine(&r2).await.unwrap();
    let last = RoutineRun {
        routine_id: r2.id.clone(),
        notify: Some(NewNotification::new(NotificationKind::Failed, "paused")),
        ..mk(None)
    };
    assert!(store.run_routine(&last, now).await.unwrap());
    let stored = store.routine(&r2.id).await.unwrap().unwrap();
    assert!(!stored.enabled);
    assert_eq!(stored.next_run_at, None);
    let feed = store.notifications(0, 10).await.unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].task_id, last.task.id);
}

async fn member_settings_round_trip(store: Arc<dyn BotsStore>) {
    assert_eq!(store.member_settings("ana").await.unwrap(), None);
    let first = MemberSettings {
        handle: "ana".into(),
        level: Level::Staff,
        updated_at: at(2026, 11, 14, 9, 0),
    };
    store.set_member_settings(&first).await.unwrap();
    assert_eq!(store.member_settings("ana").await.unwrap(), Some(first));
    let second = MemberSettings {
        handle: "ana".into(),
        level: Level::Principal,
        updated_at: at(2026, 11, 14, 9, 5),
    };
    store.set_member_settings(&second).await.unwrap();
    assert_eq!(
        store.member_settings("ana").await.unwrap(),
        Some(second),
        "a second save replaces the first"
    );
    assert_eq!(store.member_settings("ben").await.unwrap(), None);

    // A task keeps its own level; one without reads back without.
    let mut t = task("docs", "ben", Source::Eval, 7);
    t.level = Some(Level::Principal);
    store.create_task(&t, "queued").await.unwrap();
    assert_eq!(
        store.task(&t.id).await.unwrap().unwrap().level,
        Some(Level::Principal)
    );
    let plain = task("docs", "ben", Source::Eval, 8);
    store.create_task(&plain, "queued").await.unwrap();
    assert_eq!(store.task(&plain.id).await.unwrap().unwrap().level, None);
    let now = at(2026, 11, 14, 10, 0);
    let claimed = store
        .claim(None, "w1", now + Duration::seconds(120), now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, t.id);
    assert_eq!(
        claimed.level,
        Some(Level::Principal),
        "a claim keeps the level"
    );
}

both_stores!(
    member_settings_round_trip,
    tasks_round_trip_and_filter,
    claims_go_interactive_first_then_oldest,
    two_workers_never_claim_the_same_task,
    transitions_check_their_guards,
    a_refused_change_leaves_the_task_claimable,
    web_tasks_round_trip_and_write_no_notifications,
    expired_leases_are_found,
    notifications_are_ordered_and_paged,
    concurrent_notifications_have_distinct_numbers,
    traces_redact_slack_and_expire,
    routines_crud,
    routine_runs_happen_once,
);
