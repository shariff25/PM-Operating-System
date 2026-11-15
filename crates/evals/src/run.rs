//! Running a category: create each task (each `repeats` times), follow it until it settles,
//! read its drafts, grade it, ask the judges, and tag it.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::client::{status, Runtime, RuntimeTask, TraceEntry};
use crate::grade::{draft_text, grade};
use crate::judge::{Gateway, Judge, JudgeStatus, JudgeVerdict};
use crate::outcome::Outcome;
use crate::report::{Report, TaskReport};
use crate::spec::{resolve_bot, Category, EvalTask};
use crate::tag::tag_failure;

/// How to follow a task.
#[derive(Debug, Clone)]
pub struct Follow {
    pub poll: Duration,
    pub timeout: Duration,
}

/// Polls in a row that may fail before the task is given up on.
const POLL_RETRIES: u32 = 3;

/// Create a task (at `level`, when the category has one) and poll it until it settles or
/// `follow.timeout` passes. Fills the events and the timings; the trace is read later, only
/// for failures and judges.
pub async fn follow_task(
    rt: &Runtime,
    bot: &str,
    requested_by: &str,
    text: &str,
    level: Option<&str>,
    follow: &Follow,
) -> Outcome {
    let created = match rt.create_task(bot, requested_by, text, level).await {
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
        drafts: Vec::new(),
        drafts_error: None,
        queue_secs: claimed.map(secs),
        total_secs: (!timed_out).then(|| secs(last.updated_at)),
    }
}

/// The judges a run asks, with the gateway they are asked through.
#[derive(Clone)]
pub struct JudgeRun {
    pub gateway: Gateway,
    /// Each judge that applies to the category, and whether it blocks.
    pub judges: Vec<(Judge, JudgeStatus)>,
}

/// Options for [`run_category`].
#[derive(Clone)]
pub struct RunOptions {
    pub requested_by: String,
    pub follow: Follow,
    pub concurrency: usize,
    pub date: NaiveDate,
    pub note: Option<String>,
    pub gateway_workarounds: Option<String>,
    /// Run only these task ids (all when empty).
    pub only: Vec<String>,
    /// Runs per task: the category's `repeats` unless overridden.
    pub repeats: u32,
    /// `config.yaml`'s, for `must_decline`.
    pub decline_phrases: Vec<String>,
    pub judges: Option<JudgeRun>,
    /// Why judges that apply aren't asked, recorded in the report.
    pub judges_skipped: Option<String>,
}

/// At most this much of each tool result, and of all of them, goes to a judge.
const SOURCE_CHARS: usize = 6000;
const SOURCES_CHARS: usize = 24000;

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// What a judge sees as the input: the ask, then what the bot read (its tool results, from
/// the trace, when the runner could read it).
pub fn judge_input(ask: &str, trace: Option<&[TraceEntry]>) -> String {
    let mut s = format!("The request: {ask}");
    let results: Vec<String> = trace
        .unwrap_or_default()
        .iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| clip(&e.body.to_string(), SOURCE_CHARS))
        .collect();
    if !results.is_empty() {
        s.push_str("\n\nWhat the assistant read (tool results):\n");
        s.push_str(&clip(&results.join("\n\n"), SOURCES_CHARS));
    }
    s
}

/// What a judge sees as the output: the answer, then each draft's tool and text.
pub fn judge_output(outcome: &Outcome) -> String {
    let mut s = outcome.result_text().to_string();
    if !outcome.drafts.is_empty() {
        s.push_str("\n\nDrafts waiting for approval:");
        for d in &outcome.drafts {
            s.push_str(&format!("\n- {}: {}", d.tool, draft_text(&d.args)));
        }
    }
    s
}

async fn attempt(
    rt: &Runtime,
    bot: &str,
    level: Option<&str>,
    task: &EvalTask,
    repeat: u32,
    opts: &RunOptions,
) -> TaskReport {
    let mut outcome =
        follow_task(rt, bot, &opts.requested_by, &task.ask, level, &opts.follow).await;
    let settled = outcome.runtime_error.is_none() && !outcome.timed_out;
    let judging = opts.judges.as_ref().filter(|j| !j.judges.is_empty());
    let wants_drafts =
        !task.expect.drafts.is_empty() || (judging.is_some() && outcome.approvals > 0);
    if let (true, true, Some(id)) = (settled, wants_drafts, outcome.task_id.clone()) {
        match rt.drafts(&id).await {
            Ok(d) => outcome.drafts = d,
            Err(e) => outcome.drafts_error = Some(e.to_string()),
        }
    }
    let grade = grade(task, &outcome, &opts.decline_phrases);
    let answered = settled && !outcome.result_text().trim().is_empty();
    let mut trace_read = false;
    if !grade.passed() || (judging.is_some() && answered) {
        if let Some(id) = outcome.task_id.clone() {
            match rt.trace(&id).await {
                Ok(t) => {
                    trace_read = t.is_some();
                    outcome.trace = t;
                }
                Err(e) => eprintln!("warning: task {id}: {e}; going on without the trace"),
            }
        }
    }
    let mut verdicts = Vec::new();
    if let (Some(j), true) = (judging, answered) {
        let input = judge_input(&task.ask, outcome.trace.as_deref());
        let output = judge_output(&outcome);
        let has_sources = outcome
            .trace
            .as_deref()
            .is_some_and(|t| t.iter().any(|e| e.kind == "tool_result"));
        for (judge, status) in &j.judges {
            let (verdict, error) = if judge.needs_sources && !has_sources {
                (
                    None,
                    Some("no tool results in the trace to judge against".to_string()),
                )
            } else {
                match j.gateway.ask(judge, &input, &output).await {
                    Ok(Some(v)) => (Some(v), None),
                    Ok(None) => (None, Some("the reply was neither yes nor no".to_string())),
                    Err(e) => (None, Some(e)),
                }
            };
            verdicts.push(JudgeVerdict {
                name: judge.name.clone(),
                verdict,
                blocking: status.blocking,
                error,
            });
        }
    }
    let tag = (!grade.passed()).then(|| tag_failure(&outcome, &grade));
    TaskReport {
        id: task.id.clone(),
        ask: task.ask.clone(),
        repeat,
        cluster: Some(task.cluster()),
        regression: task.regression,
        task_id: outcome.task_id.clone(),
        status: outcome.status.clone(),
        passed: grade.passed(),
        failures: grade.failures,
        tag,
        queue_secs: outcome.queue_secs,
        total_secs: outcome.total_secs,
        attempts: outcome.attempts,
        trace_read,
        judges: verdicts,
    }
}

/// Run every task of a category `opts.repeats` times (at most `concurrency` at once), grade
/// and tag each attempt, and return the report. Lines are in the category's order, each
/// task's repeats together.
pub async fn run_category(
    rt: &Runtime,
    runtime_url: &str,
    category: &Category,
    opts: &RunOptions,
) -> Report {
    let started_at = Utc::now();
    let bot = resolve_bot(&category.bot, &opts.requested_by);
    let repeats = opts.repeats.max(1);
    let tasks: Vec<_> = category
        .tasks
        .iter()
        .filter(|t| opts.only.is_empty() || opts.only.contains(&t.id))
        .cloned()
        .collect();
    let limit = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let mut set = JoinSet::new();
    // The first run of every task goes first, so a short timeout still covers each task once.
    for repeat in 1..=repeats {
        for (i, task) in tasks.iter().cloned().enumerate() {
            let rt = rt.clone();
            let limit = limit.clone();
            let bot = bot.clone();
            let level = category.level.clone();
            let opts = opts.clone();
            set.spawn(async move {
                let _permit = limit
                    .acquire_owned()
                    .await
                    .expect("the semaphore is never closed");
                let line = attempt(&rt, &bot, level.as_deref(), &task, repeat, &opts).await;
                (i * repeats as usize + (repeat as usize - 1), line)
            });
        }
    }
    let mut lines: Vec<Option<TaskReport>> = vec![None; tasks.len() * repeats as usize];
    while let Some(joined) = set.join_next().await {
        let (i, line) = joined.expect("a task runner never panics");
        lines[i] = Some(line);
    }
    Report {
        category: category.name.clone(),
        bot,
        date: opts.date,
        started_at,
        finished_at: Utc::now(),
        pass_bar: category.pass_bar,
        runtime_url: runtime_url.to_string(),
        requested_by: opts.requested_by.clone(),
        gateway_workarounds: opts.gateway_workarounds.clone(),
        note: opts.note.clone(),
        repeats,
        judges: opts
            .judges
            .as_ref()
            .map(|j| j.judges.iter().map(|(_, s)| s.clone()).collect())
            .unwrap_or_default(),
        judges_skipped: opts.judges_skipped.clone(),
        tasks: lines.into_iter().flatten().collect(),
    }
}
