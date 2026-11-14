//! Running a category: create each task, follow it until it settles, grade it, and tag it.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::client::{status, Runtime, RuntimeTask};
use crate::grade::grade;
use crate::outcome::Outcome;
use crate::report::{Report, TaskReport};
use crate::spec::Category;
use crate::tag::tag_failure;

/// How to follow a task.
#[derive(Debug, Clone)]
pub struct Follow {
    pub poll: Duration,
    pub timeout: Duration,
}

/// Polls in a row that may fail before the task is given up on.
const POLL_RETRIES: u32 = 3;

/// Create a task and poll it until it settles or `follow.timeout` passes. Fills the events
/// and the timings; the trace is read later, only for failures.
pub async fn follow_task(
    rt: &Runtime,
    bot: &str,
    requested_by: &str,
    text: &str,
    follow: &Follow,
) -> Outcome {
    let created = match rt.create_task(bot, requested_by, text).await {
        Ok(t) => t,
        Err(e) => {
            return Outcome {
                runtime_error: Some(e.to_string()),
                ..Outcome::default()
            }
        }
    };
    let started = tokio::time::Instant::now();
    let mut last: RuntimeTask = created;
    let mut first_active: Option<DateTime<Utc>> = None;
    let mut misses = 0;
    let mut timed_out = false;
    loop {
        if last.status != status::QUEUED && first_active.is_none() {
            first_active = Some(Utc::now());
        }
        if last.is_settled() {
            break;
        }
        if started.elapsed() >= follow.timeout {
            timed_out = true;
            break;
        }
        tokio::time::sleep(follow.poll).await;
        match rt.task(&last.id).await {
            Ok(t) => {
                last = t;
                misses = 0;
            }
            Err(e) => {
                misses += 1;
                if misses >= POLL_RETRIES {
                    return Outcome {
                        task_id: Some(last.id.clone()),
                        status: Some(last.status.clone()),
                        runtime_error: Some(e.to_string()),
                        ..Outcome::default()
                    };
                }
            }
        }
    }
    let events = match rt.events(&last.id).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("warning: task {}: {e}; tagging without events", last.id);
            Vec::new()
        }
    };
    // A worker claimed the task before it sent its first event and before a poll saw it
    // leave `queued`, so the earlier of the two is the closest bound on the claim.
    let first_event = events.iter().map(|e| e.at).min();
    let claimed = match (first_event, first_active) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let secs =
        |to: DateTime<Utc>| ((to - last.created_at).num_milliseconds().max(0)) as f64 / 1000.0;
    Outcome {
        task_id: Some(last.id.clone()),
        status: Some(last.status.clone()),
        result: last.result.clone(),
        error: last.error.clone(),
        approvals: last.approvals.len(),
        attempts: last.attempts,
        timed_out,
        runtime_error: None,
        events,
        trace: None,
        queue_secs: claimed.map(secs),
        total_secs: (!timed_out).then(|| secs(last.updated_at)),
    }
}

/// Options for [`run_category`].
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub requested_by: String,
    pub follow: Follow,
    pub concurrency: usize,
    pub date: NaiveDate,
    pub note: Option<String>,
    pub gateway_workarounds: Option<String>,
    /// Run only these task ids (all when empty).
    pub only: Vec<String>,
}

/// Run every task of a category (at most `concurrency` at once), grade and tag each, and
/// return the report. Tasks are listed in the category's order.
pub async fn run_category(
    rt: &Runtime,
    runtime_url: &str,
    category: &Category,
    opts: &RunOptions,
) -> Report {
    let started_at = Utc::now();
    let tasks: Vec<_> = category
        .tasks
        .iter()
        .filter(|t| opts.only.is_empty() || opts.only.contains(&t.id))
        .cloned()
        .collect();
    let limit = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let mut set = JoinSet::new();
    for (i, task) in tasks.iter().cloned().enumerate() {
        let rt = rt.clone();
        let limit = limit.clone();
        let bot = category.bot.clone();
        let opts = opts.clone();
        set.spawn(async move {
            let _permit = limit
                .acquire_owned()
                .await
                .expect("the semaphore is never closed");
            let mut outcome =
                follow_task(&rt, &bot, &opts.requested_by, &task.ask, &opts.follow).await;
            let grade = grade(&task, &outcome);
            let mut trace_read = false;
            let tag = if grade.passed() {
                None
            } else {
                if let Some(id) = outcome.task_id.clone() {
                    match rt.trace(&id).await {
                        Ok(t) => {
                            trace_read = t.is_some();
                            outcome.trace = t;
                        }
                        Err(e) => eprintln!("warning: task {id}: {e}; tagging without the trace"),
                    }
                }
                Some(tag_failure(&outcome, &grade))
            };
            let line = TaskReport {
                id: task.id.clone(),
                ask: task.ask.clone(),
                task_id: outcome.task_id.clone(),
                status: outcome.status.clone(),
                passed: grade.passed(),
                failures: grade.failures,
                tag,
                queue_secs: outcome.queue_secs,
                total_secs: outcome.total_secs,
                attempts: outcome.attempts,
                trace_read,
            };
            (i, line)
        });
    }
    let mut lines: Vec<Option<TaskReport>> = vec![None; tasks.len()];
    while let Some(joined) = set.join_next().await {
        let (i, line) = joined.expect("a task runner never panics");
        lines[i] = Some(line);
    }
    Report {
        category: category.name.clone(),
        bot: category.bot.clone(),
        date: opts.date,
        started_at,
        finished_at: Utc::now(),
        pass_bar: category.pass_bar,
        runtime_url: runtime_url.to_string(),
        requested_by: opts.requested_by.clone(),
        gateway_workarounds: opts.gateway_workarounds.clone(),
        note: opts.note.clone(),
        tasks: lines.into_iter().flatten().collect(),
    }
}
