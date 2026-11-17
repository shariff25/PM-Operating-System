//! What the runtime does, apart from HTTP: create and claim tasks, follow them through to a
//! result, recover them after a restart, run routines, close approvals, and keep traces
//! within their retention. The HTTP layer (`app.rs`) decides who may call what; everything
//! here assumes the caller is allowed.
//!
//! Every method takes `now`, so tests can move the clock; the background loops pass the
//! real time.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use pmos_contract::bots::{Bot, BotScope, Role};
use pmos_contract::model::valid_handle;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::catalog::Catalog;
use crate::clients::{valid_approval_id, ApprovalState, CallError, Gateway, TaskDraft, ToolServer};
use crate::model::{
    self, ApprovalOutcome, ChecklistItem, Draft, Level, MemberSettings, NewNotification,
    Notification, NotificationKind, Routine, Schedule, Sensitivity, SlackRef, Source, Task,
    TaskStatus, TraceKind, Turn, GAVE_UP_REASON, LEASE_SECONDS, MAX_ATTEMPTS, REDACTED_MARKER,
    RETRY_EVENT,
};
use crate::people::{self, SnapshotClient};
use crate::routines::{self, Parsed};
use crate::store::{
    BotsStore, Lease, Refused, RoutinePatch, RoutineRun, StoreError, TaskFilter, Transition,
};

/// How often leases, routines and approvals are checked.
pub const TICK: Duration = Duration::from_secs(30);
/// How often traces are redacted and pruned.
pub const HOUSEKEEPING: Duration = Duration::from_secs(60 * 60);
/// The longest a claim or notification request may wait.
pub const MAX_WAIT: Duration = Duration::from_millis(25_000);
/// Slack text in traces is removed after this long (SLK-8).
pub const SLACK_TEXT_HOURS: i64 = 24;
/// Traces (and the notification feed) are kept this long (D6).
pub const RETENTION_DAYS: i64 = 30;

pub const MAX_TEXT_CHARS: usize = 20_000;
pub const MAX_RESULT_CHARS: usize = 100_000;
pub const MAX_TRACE_ENTRIES: usize = 500;
pub const NOTIFICATION_PAGE: usize = 100;

/// A claim carries at most this many earlier turns of the task's conversation.
pub const HISTORY_TURNS: usize = 6;
/// ... and at most this many bytes of their text, results and errors, the oldest dropped first.
pub const HISTORY_BYTES: usize = 16_000;
/// Each turn's text, result and error is cut to this many bytes, with [`TRUNCATED`] after.
pub const TURN_FIELD_BYTES: usize = 4_000;
/// What a cut field ends with.
pub const TRUNCATED: &str = "… [truncated]";

/// Errors as a PM or a calling service should read them. `app.rs` maps each to a status.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    /// The task isn't in a state that allows this; `status` tells a worker to stop.
    #[error("{message}")]
    Conflict {
        message: String,
        status: Option<TaskStatus>,
    },
    /// The request is unclear; ask the PM this (`422 {question}`).
    #[error("{0}")]
    Unclear(String),
    #[error("{0}")]
    Unavailable(String),
    /// A notification cursor this feed never issued: it is past the feed's head, so it came
    /// from another epoch of the feed (an in-memory runtime that restarted, or a database
    /// that was reset). `409 {"code": "cursor_ahead", "head"}`; the client should start
    /// again from `0`.
    #[error("{CURSOR_AHEAD}: the notification cursor {after} is past this feed's head {head}; it is from an earlier runtime or database. Start again from 0 (the Slack service: delete its notifications.cursor)")]
    CursorAhead { after: i64, head: i64 },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The code and the start of the message of [`Error::CursorAhead`].
pub const CURSOR_AHEAD: &str = "cursor_ahead";

pub type Result<T> = std::result::Result<T, Error>;

fn refused(id: &str, r: Refused) -> Error {
    match r {
        Refused::NotFound => Error::NotFound(format!("no task {id}")),
        Refused::Status(s) => Error::Conflict {
            message: format!("task {id} is {s}"),
            status: Some(s),
        },
        Refused::NotYourClaim => Error::Conflict {
            message: format!("another worker holds task {id}"),
            status: Some(TaskStatus::Working),
        },
        Refused::LeaseActive => Error::Conflict {
            message: format!("task {id} is still leased"),
            status: Some(TaskStatus::Working),
        },
    }
}

/// `POST /v1/tasks`.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub bot: String,
    pub requested_by: String,
    pub text: String,
    pub source: Source,
    pub slack: Option<SlackRef>,
    /// The task's own level (the evals client only; `app.rs` checks).
    pub level: Option<Level>,
    /// The conversation it continues (the web and Slack clients only; `app.rs` checks).
    pub conversation: Option<String>,
    /// The lowest level the task's session starts at, because its text already carries data
    /// of that class: the web app's revision of a draft, whose prompt holds the draft's text
    /// (R2, the W6 carry-forward for revision tasks). Kept as the task's
    /// `sources_sensitivity` until it finishes, when what its sessions reached (never lower,
    /// since they started here) replaces it. Only raises; the web client only (`app.rs`).
    pub sensitivity_floor: Option<Sensitivity>,
}

/// What the harness needs to call the gateway for this task: the bot's model role and the
/// headers that put each call in the right queue and usage line.
#[derive(Debug, Clone, Serialize)]
pub struct GatewayHints {
    pub model: String,
    pub headers: serde_json::Map<String, Value>,
}

/// `POST /v1/worker/claim`'s answer.
#[derive(Clone, Serialize)]
pub struct Claim {
    pub task: Task,
    pub bot: Bot,
    pub role: Role,
    pub tools: Vec<String>,
    pub session_token: String,
    pub lease_until: DateTime<Utc>,
    pub gateway: GatewayHints,
    /// The level to work at: the task's own, else the requester's setting, else `senior`.
    pub level: Level,
    /// The earlier turns of the task's conversation, oldest first; empty for a first message.
    pub history: Vec<Turn>,
    /// The highest data class those turns were built from, when there are any. The tool
    /// server already started the session at it (the worker can't lower it); it is here so
    /// the worker can tell the model the earlier answers drew on restricted sources.
    pub history_sensitivity: Option<Sensitivity>,
}

/// The session token never reaches a log through `Debug`.
impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Claim")
            .field("task", &self.task.id)
            .field("bot", &self.bot.name)
            .field("session_token", &"<redacted>")
            .field("lease_until", &self.lease_until)
            .field("level", &self.level)
            .field("history", &self.history.len())
            .field("history_sensitivity", &self.history_sensitivity)
            .finish()
    }
}

/// What a worker reports with `POST /v1/worker/tasks/{id}/events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    Status(String),
    Checklist(Vec<ChecklistItem>, Option<String>),
    Milestone(String),
}

/// What one recovery pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Recovered {
    pub requeued: usize,
    pub failed: usize,
}

/// What one housekeeping pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Housekeeping {
    pub redacted: u64,
    pub traces_pruned: u64,
    pub notifications_pruned: u64,
}

/// `PATCH /v1/routines/{id}`.
#[derive(Debug, Clone, Default)]
pub struct RoutineChange {
    pub enabled: Option<bool>,
    pub schedule: Option<Schedule>,
    pub prompt: Option<String>,
}

pub struct Runtime {
    pub store: Arc<dyn BotsStore>,
    /// The bots, roles and tools, swapped whole when the content changes
    /// (`catalog::Reloader`). Each use takes its own snapshot, so a reload never changes a
    /// catalog mid-call.
    catalog: RwLock<Arc<Catalog>>,
    /// Who may ask: the user directory from the tool server (`crate::people`).
    people: Arc<SnapshotClient>,
    toolserver: ToolServer,
    gateway: Gateway,
    /// Bumped on every change a long-poll may be waiting for (a new task, a notification).
    changed: watch::Sender<u64>,
    /// Cancelled on shutdown: long-polls return at once and the loops stop.
    pub stop: CancellationToken,
}

fn trimmed(s: &str) -> String {
    s.trim().to_string()
}

fn too_long(what: &str, s: &str, max: usize) -> Option<Error> {
    (s.chars().count() > max)
        .then(|| Error::BadRequest(format!("{what} is longer than {max} characters")))
}

impl Runtime {
    pub fn new(
        store: Arc<dyn BotsStore>,
        catalog: Arc<Catalog>,
        people: Arc<SnapshotClient>,
        toolserver: ToolServer,
        gateway: Gateway,
        stop: CancellationToken,
    ) -> Self {
        Self {
            store,
            catalog: RwLock::new(catalog),
            people,
            toolserver,
            gateway,
            changed: watch::channel(0).0,
            stop,
        }
    }

    /// The catalog as it is now.
    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.read().expect("catalog lock").clone()
    }

    /// Replace the catalog. Tasks already claimed keep the bot, role and tools they were
    /// claimed with; new claims, tasks and routines use the new one.
    pub fn set_catalog(&self, catalog: Arc<Catalog>) {
        *self.catalog.write().expect("catalog lock") = catalog;
    }

    /// The user directory this runtime decides from.
    pub fn people(&self) -> &Arc<SnapshotClient> {
        &self.people
    }

    /// Is `handle` an active admin, on a snapshot at most 30 seconds old? Without one,
    /// nobody is (the console then shows a viewer their own tasks only).
    pub fn is_admin(&self, handle: &str) -> bool {
        self.people.fresh().is_some_and(|p| p.is_admin(handle))
    }

    /// May `handle` use `bot`, on `people` (`None`: no snapshot recent enough, `503`)?
    fn may_ask(
        &self,
        people: Option<Arc<people::Snapshot>>,
        handle: &str,
        bot: &Bot,
    ) -> Result<()> {
        let people = people.ok_or_else(|| Error::Unavailable(people::UNAVAILABLE.into()))?;
        people::may_ask(&people, &self.catalog(), handle, bot).map_err(Error::Forbidden)
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Wait until something changes, a second passes (another process may have written),
    /// `until` arrives or the runtime stops. False once it is time to give up.
    async fn wait_for_change(&self, rx: &mut watch::Receiver<u64>, until: Instant) -> bool {
        let now = Instant::now();
        if now >= until {
            return false;
        }
        let nap = (until - now).min(Duration::from_secs(1));
        tokio::select! {
            _ = rx.changed() => {}
            _ = tokio::time::sleep(nap) => {}
            _ = self.stop.cancelled() => return false,
        }
        Instant::now() < until && !self.stop.is_cancelled()
    }

    // ----- tasks ---------------------------------------------------------------------------

    pub fn bot(&self, name: &str) -> Result<Bot> {
        self.catalog()
            .bot(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("there's no bot called `{name}`")))
    }

    pub async fn create_task(&self, new: NewTask, now: DateTime<Utc>) -> Result<Task> {
        if !valid_handle(&new.requested_by) {
            return Err(Error::BadRequest(format!(
                "`{}` isn't a PM OS handle",
                new.requested_by
            )));
        }
        let text = trimmed(&new.text);
        if text.is_empty() {
            return Err(Error::BadRequest("the task has no text".into()));
        }
        if let Some(e) = too_long("the task text", &text, MAX_TEXT_CHARS) {
            return Err(e);
        }
        if let Some(c) = &new.conversation {
            if !model::valid_conversation(c) {
                return Err(Error::BadRequest(
                    "a conversation id is 1 to 128 letters, digits, `.`, `_`, `:` or `-`".into(),
                ));
            }
        }
        let bot = self.bot(&new.bot)?;
        // Queued on a snapshot up to 5 minutes old (spec §2): the task can't run without a
        // tool server session, which decides afresh and fails closed.
        self.may_ask(self.people.for_queueing_ask(), &new.requested_by, &bot)?;
        let mut task = Task::queued(
            &bot.name,
            &new.requested_by,
            new.source,
            &text,
            new.slack,
            None,
            now,
        );
        task.level = new.level;
        task.conversation = new.conversation;
        task.sources_sensitivity = new.sensitivity_floor;
        self.store.create_task(&task, "queued").await?;
        tracing::info!(task = %task.id, bot = %task.bot, source = %task.source, "task queued");
        self.bump();
        Ok(task)
    }

    pub async fn task(&self, id: &str) -> Result<Task> {
        self.store
            .task(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no task {id}")))
    }

    pub async fn cancel(&self, id: &str, by: &str, now: DateTime<Utc>) -> Result<Task> {
        let change = Transition {
            to: Some(TaskStatus::Cancelled),
            lease: Lease::Clear,
            event: Some(("status".into(), format!("cancelled by {by}"))),
            ..Transition::from(&[TaskStatus::Queued, TaskStatus::Working])
        };
        let task = self
            .store
            .transition(id, &change, now)
            .await?
            .map_err(|r| refused(id, r))?;
        self.bump();
        Ok(task)
    }

    // ----- workers -------------------------------------------------------------------------

    /// Claim the next task for `worker`, waiting up to `wait` for one. `Ok(None)` when
    /// nothing was queued in time.
    pub async fn claim(
        &self,
        worker: &str,
        bots: Option<&[String]>,
        wait: Duration,
    ) -> Result<Option<Claim>> {
        let until = Instant::now() + wait.min(MAX_WAIT);
        let mut rx = self.changed.subscribe();
        loop {
            rx.borrow_and_update();
            if let Some(claim) = self.claim_now(worker, bots, model::now()).await? {
                return Ok(Some(claim));
            }
            if !self.wait_for_change(&mut rx, until).await {
                return Ok(None);
            }
        }
    }

    /// One claim attempt. A task whose bot is gone, or whose session the tool server
    /// refuses, fails with the reason and the next one is tried.
    pub async fn claim_now(
        &self,
        worker: &str,
        bots: Option<&[String]>,
        now: DateTime<Utc>,
    ) -> Result<Option<Claim>> {
        loop {
            let lease_until = now + chrono::Duration::seconds(LEASE_SECONDS);
            let Some(task) = self.store.claim(bots, worker, lease_until, now).await? else {
                return Ok(None);
            };
            let catalog = self.catalog();
            let found = catalog
                .bot(&task.bot)
                .and_then(|b| catalog.role(b).map(|r| (b, r)));
            let Some((bot, role)) = found else {
                let reason = format!("the bot `{}` no longer exists", task.bot);
                self.fail_claimed(&task, worker, &reason, now).await?;
                continue;
            };
            // The history first: its level is where the session starts (W6).
            let (history, history_sensitivity) = self.history(&task, now).await;
            // A task not yet finished carries in `sources_sensitivity` only its own floor
            // (`NewTask::sensitivity_floor`): the session starts at the higher of the two.
            let floor = history_sensitivity.max(task.sources_sensitivity);
            match self
                .toolserver
                .create_session(&task.requested_by, &bot.name, &task.id, floor)
                .await
            {
                Ok(session_token) => {
                    let level = self.claim_level(&task).await;
                    tracing::info!(task = %task.id, bot = %bot.name, %worker, attempt = task.attempts, %level, turns = history.len(), "task claimed");
                    let mut headers = serde_json::Map::new();
                    headers.insert("X-Pmos-User".into(), json!(task.requested_by));
                    headers.insert("X-Pmos-Run".into(), json!(task.id));
                    headers.insert("X-Pmos-Priority".into(), json!(task.priority.as_str()));
                    let model_role = serde_json::to_value(role.model)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "default".into());
                    return Ok(Some(Claim {
                        tools: catalog.tools(bot).into_iter().collect(),
                        bot: bot.clone(),
                        role: role.clone(),
                        session_token,
                        lease_until,
                        gateway: GatewayHints {
                            model: format!("pmos/{model_role}"),
                            headers,
                        },
                        level,
                        history,
                        history_sensitivity,
                        task,
                    }));
                }
                Err(CallError::Refused(why)) => {
                    let reason =
                        format!("the tool server wouldn't start this bot's session: {why}");
                    self.fail_claimed(&task, worker, &reason, now).await?;
                }
                Err(CallError::Unavailable(what)) => {
                    // Not the task's fault: give it back without counting the attempt.
                    let change = Transition {
                        worker: Some(worker.to_string()),
                        to: Some(TaskStatus::Queued),
                        lease: Lease::Clear,
                        attempts_delta: -1,
                        event: Some((
                            "status".into(),
                            format!("{what} was unavailable; back in the queue"),
                        )),
                        ..Transition::from(&[TaskStatus::Working])
                    };
                    let _ = self.store.transition(&task.id, &change, now).await?;
                    return Err(Error::Unavailable(format!(
                        "{what} is unavailable right now; try the claim again shortly"
                    )));
                }
            }
        }
    }

    /// The level a claimed task runs at: its own, else its requester's setting, else
    /// `senior`. A settings read that fails right after the claim falls back to `senior`
    /// rather than strand the task.
    async fn claim_level(&self, task: &Task) -> Level {
        if let Some(level) = task.level {
            return level;
        }
        match self.store.member_settings(&task.requested_by).await {
            Ok(settings) => settings.map(|s| s.level).unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, task = %task.id, "couldn't read the requester's level; using senior");
                Level::default()
            }
        }
    }

    /// The earlier turns of a claimed task's conversation, oldest first (spec §3,
    /// Conversations). Only the requester's own tasks for the same bot in the same thread,
    /// asked within the last [`SLACK_TEXT_HOURS`] (the retention window for Slack text) and
    /// never redacted. A read that fails gives no history rather than strand the task.
    ///
    /// Also the highest data class those turns were built from (`None` without history):
    /// each turn's recorded level, an unrecorded one counting as `restricted` (W6).
    async fn history(&self, task: &Task, now: DateTime<Utc>) -> (Vec<Turn>, Option<Sensitivity>) {
        let since = now - chrono::Duration::hours(SLACK_TEXT_HOURS);
        let earlier = match self.store.earlier_turns(task, since, HISTORY_TURNS).await {
            Ok(earlier) => earlier,
            Err(e) => {
                tracing::warn!(error = %e, task = %task.id, "couldn't read the conversation; starting fresh");
                return (Vec::new(), None);
            }
        };
        bounded_history_with_level(earlier)
    }

    /// The level a finished task's sessions reached, as the tool server reports it. When it
    /// can't say (no session stored, or unreachable), `restricted`: a follow-up is then
    /// checked as if the answer drew on restricted sources, never the other way round.
    async fn finished_level(&self, id: &str) -> Sensitivity {
        match self.toolserver.run_sensitivity(id).await {
            Ok(Some(level)) => level,
            Ok(None) => {
                tracing::warn!(task = %id, "the tool server has no session for this task; recording it as restricted");
                Sensitivity::Restricted
            }
            Err(e) => {
                tracing::warn!(error = %e, task = %id, "couldn't read the task's level; recording it as restricted");
                Sensitivity::Restricted
            }
        }
    }

    async fn fail_claimed(
        &self,
        task: &Task,
        worker: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        tracing::warn!(task = %task.id, %reason, "task failed at claim");
        let change = Transition {
            worker: Some(worker.to_string()),
            to: Some(TaskStatus::Failed),
            lease: Lease::Clear,
            error: Some(reason.to_string()),
            event: Some(("status".into(), format!("failed: {reason}"))),
            notify: Some(NewNotification::new(NotificationKind::Failed, reason)),
            // No session ran for this claim: what a follow-up replays is the PM's text and
            // this reason, at the task's own floor if its text carries one.
            sensitivity: Some(task.sources_sensitivity.unwrap_or(Sensitivity::Public)),
            ..Transition::from(&[TaskStatus::Working])
        };
        let _ = self.store.transition(&task.id, &change, now).await?;
        self.bump();
        Ok(())
    }

    async fn worker_change(
        &self,
        id: &str,
        change: Transition,
        now: DateTime<Utc>,
    ) -> Result<Task> {
        let task = self
            .store
            .transition(id, &change, now)
            .await?
            .map_err(|r| refused(id, r))?;
        if change.notify.is_some() {
            self.bump();
        }
        Ok(task)
    }

    fn working(worker: Option<&str>) -> Transition {
        Transition {
            worker: worker.map(str::to_string),
            ..Transition::from(&[TaskStatus::Working])
        }
    }

    /// Extend the lease. A task that was cancelled or recovered answers `409` with its
    /// status, which tells the worker to stop.
    pub async fn heartbeat(
        &self,
        id: &str,
        worker: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let until = now + chrono::Duration::seconds(LEASE_SECONDS);
        let change = Transition {
            lease: Lease::Until(until),
            ..Self::working(worker)
        };
        self.worker_change(id, change, now).await?;
        Ok(until)
    }

    pub async fn worker_event(
        &self,
        id: &str,
        worker: Option<&str>,
        event: WorkerEvent,
        now: DateTime<Utc>,
    ) -> Result<Task> {
        let mut change = Self::working(worker);
        match event {
            WorkerEvent::Status(text) => {
                change.event = Some(("status".into(), text));
            }
            WorkerEvent::Checklist(items, text) => {
                let done = items.iter().filter(|i| i.done).count();
                let text = text.unwrap_or_else(|| format!("{done} of {} done", items.len()));
                change.checklist = Some(items);
                change.event = Some(("checklist".into(), text));
            }
            WorkerEvent::Milestone(text) => {
                change.event = Some(("milestone".into(), text.clone()));
                change.notify = Some(NewNotification::new(NotificationKind::Milestone, text));
            }
        }
        self.worker_change(id, change, now).await
    }

    pub async fn add_trace(
        &self,
        id: &str,
        entries: Vec<(TraceKind, Value)>,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        if entries.len() > MAX_TRACE_ENTRIES {
            return Err(Error::BadRequest(format!(
                "send at most {MAX_TRACE_ENTRIES} trace entries at a time"
            )));
        }
        let task = self.task(id).await?;
        if task.status == TaskStatus::Queued {
            return Err(Error::Conflict {
                message: format!("task {id} hasn't been claimed"),
                status: Some(task.status),
            });
        }
        self.store.add_trace(id, &entries, now).await?;
        Ok(entries.len())
    }

    /// The worker's result. With approvals pending it waits as `needs_approval`; else done.
    pub async fn complete(
        &self,
        id: &str,
        worker: Option<&str>,
        result: &str,
        approvals: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<Task> {
        if let Some(e) = too_long("the result", result, MAX_RESULT_CHARS) {
            return Err(e);
        }
        if let Some(bad) = approvals.iter().find(|a| !valid_approval_id(a)) {
            return Err(Error::BadRequest(format!("`{bad}` isn't an approval id")));
        }
        let mut seen = std::collections::HashSet::new();
        let approvals: Vec<String> = approvals
            .into_iter()
            .filter(|a| seen.insert(a.clone()))
            .collect();
        let mut change = Self::working(worker);
        change.lease = Lease::Clear;
        change.result = Some(result.to_string());
        change.sensitivity = Some(self.finished_level(id).await);
        if approvals.is_empty() {
            change.to = Some(TaskStatus::Done);
            change.event = Some(("status".into(), "done".into()));
            change.notify = Some(NewNotification::new(NotificationKind::Done, result));
        } else {
            let n = approvals.len();
            change.to = Some(TaskStatus::NeedsApproval);
            change.event = Some((
                "status".into(),
                format!(
                    "waiting for approval of {n} item{}",
                    if n == 1 { "" } else { "s" }
                ),
            ));
            change.notify = Some(NewNotification {
                approvals: approvals.clone(),
                ..NewNotification::new(NotificationKind::NeedsApproval, result)
            });
            change.approvals = Some(approvals);
        }
        let task = self.worker_change(id, change, now).await?;
        tracing::info!(task = %id, status = %task.status, "task finished");
        Ok(task)
    }

    pub async fn fail(
        &self,
        id: &str,
        worker: Option<&str>,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Task> {
        let reason = trimmed(reason);
        if reason.is_empty() {
            return Err(Error::BadRequest("say why the task failed".into()));
        }
        if let Some(e) = too_long("the reason", &reason, 2_000) {
            return Err(e);
        }
        let mut change = Self::working(worker);
        change.to = Some(TaskStatus::Failed);
        change.lease = Lease::Clear;
        change.error = Some(reason.clone());
        change.sensitivity = Some(self.finished_level(id).await);
        change.event = Some(("status".into(), format!("failed: {reason}")));
        change.notify = Some(NewNotification::new(NotificationKind::Failed, &reason));
        let task = self.worker_change(id, change, now).await?;
        tracing::info!(task = %id, "task failed");
        Ok(task)
    }

    // ----- durability (D2) -----------------------------------------------------------------

    /// Tasks whose worker stopped heartbeating: back to the queue once, then failed with a
    /// stated reason, so nothing is lost silently.
    pub async fn recover_leases(&self, now: DateTime<Utc>) -> Result<Recovered> {
        let mut out = Recovered::default();
        for task in self.store.expired_leases(now).await? {
            let retry = task.attempts < MAX_ATTEMPTS;
            let mut change = Transition {
                lease_expired: true,
                lease: Lease::Clear,
                ..Transition::from(&[TaskStatus::Working])
            };
            if retry {
                change.to = Some(TaskStatus::Queued);
                change.event = Some(("status".into(), RETRY_EVENT.into()));
            } else {
                change.to = Some(TaskStatus::Failed);
                change.error = Some(GAVE_UP_REASON.into());
                change.event = Some(("status".into(), format!("failed: {GAVE_UP_REASON}")));
                change.notify = Some(NewNotification::new(
                    NotificationKind::Failed,
                    GAVE_UP_REASON,
                ));
            }
            // A heartbeat that lands first wins; the task is simply left alone.
            if self.store.transition(&task.id, &change, now).await?.is_ok() {
                tracing::warn!(task = %task.id, attempts = task.attempts, retry, "lease expired");
                if retry {
                    out.requeued += 1;
                } else {
                    out.failed += 1;
                }
            }
        }
        if out != Recovered::default() {
            self.bump();
        }
        Ok(out)
    }

    // ----- approvals -----------------------------------------------------------------------

    /// Close every `needs_approval` task whose approval items are all closed: decided,
    /// failed, `expired` or gone. Returns how many closed. A tool server that can't answer
    /// just means "check again next tick".
    pub async fn check_approvals(&self, now: DateTime<Utc>) -> Result<usize> {
        let waiting = self
            .store
            .tasks(&TaskFilter {
                status: Some(TaskStatus::NeedsApproval),
                limit: 1_000,
                ..TaskFilter::default()
            })
            .await?;
        let mut closed = 0;
        'tasks: for task in waiting {
            let mut outcomes = Vec::new();
            for id in &task.approvals {
                match self.toolserver.approval(id).await {
                    Ok(Some(state)) if state.is_open() => continue 'tasks,
                    Ok(Some(ApprovalState {
                        tool,
                        status,
                        decided_by,
                        ..
                    })) => outcomes.push(ApprovalOutcome {
                        id: id.clone(),
                        tool,
                        status,
                        decided_by,
                    }),
                    Ok(None) => outcomes.push(ApprovalOutcome {
                        id: id.clone(),
                        tool: None,
                        status: "missing".into(),
                        decided_by: None,
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, task = %task.id, "couldn't check approvals");
                        continue 'tasks;
                    }
                }
            }
            let text = describe_outcomes(&outcomes);
            let change = Transition {
                to: Some(TaskStatus::Done),
                event: Some(("status".into(), text.clone())),
                notify: Some(NewNotification {
                    approvals: task.approvals.clone(),
                    outcomes,
                    ..NewNotification::new(NotificationKind::ApprovalClosed, text)
                }),
                ..Transition::from(&[TaskStatus::NeedsApproval])
            };
            if self.store.transition(&task.id, &change, now).await?.is_ok() {
                tracing::info!(task = %task.id, "approvals closed");
                closed += 1;
            }
        }
        if closed > 0 {
            self.bump();
        }
        Ok(closed)
    }

    /// The task's drafted outbound actions, one per approval item, in the order the worker
    /// reported them, read from the tool server with the service token
    /// (`docs/improvement-loop-spec.md` §3).
    pub async fn drafts(&self, task: &Task) -> Result<Vec<TaskDraft>> {
        let mut drafts = Vec::with_capacity(task.approvals.len());
        for id in &task.approvals {
            match self.toolserver.draft(id).await {
                Ok(Some(draft)) => drafts.push(draft),
                Ok(None) => drafts.push(TaskDraft::missing(id)),
                Err(e) => {
                    tracing::warn!(error = %e, task = %task.id, "couldn't read drafts");
                    return Err(Error::Unavailable(format!(
                        "{e}; the task's drafts can't be read, try again shortly"
                    )));
                }
            }
        }
        Ok(drafts)
    }

    // ----- notifications -------------------------------------------------------------------

    /// Notifications after `after`, waiting up to `wait` for the first. Returns the new
    /// cursor (the last item's, or `after` if none), the items in order, and the feed's head
    /// (the last sequence number it has issued, pruned or not).
    ///
    /// A cursor past the head was never issued by this feed: it is refused with
    /// [`Error::CursorAhead`] instead of answering "nothing new" forever, which is how replies
    /// used to vanish after an in-memory runtime restarted (its sequence starts again at 1
    /// while the Slack service kept its saved cursor).
    pub async fn notifications(
        &self,
        after: i64,
        wait: Duration,
    ) -> Result<(i64, Vec<Notification>, i64)> {
        let until = Instant::now() + wait.min(MAX_WAIT);
        let mut rx = self.changed.subscribe();
        loop {
            rx.borrow_and_update();
            let head = self.store.feed_head().await?;
            if after > head {
                tracing::warn!(
                    after,
                    head,
                    "a notification cursor from another epoch of the feed was refused"
                );
                return Err(Error::CursorAhead { after, head });
            }
            let items = self.store.notifications(after, NOTIFICATION_PAGE).await?;
            if let Some(last) = items.last().map(|n| n.seq) {
                return Ok((last, items, head.max(last)));
            }
            if !self.wait_for_change(&mut rx, until).await {
                return Ok((after, items, head));
            }
        }
    }

    // ----- traces --------------------------------------------------------------------------

    /// Remove Slack text older than 24 hours and anything older than 30 days.
    pub async fn housekeeping(&self, now: DateTime<Utc>) -> Result<Housekeeping> {
        let out = Housekeeping {
            redacted: self
                .store
                .redact_slack(now - chrono::Duration::hours(SLACK_TEXT_HOURS))
                .await?,
            traces_pruned: self
                .store
                .prune_traces(now - chrono::Duration::days(RETENTION_DAYS))
                .await?,
            notifications_pruned: self
                .store
                .prune_notifications(now - chrono::Duration::days(RETENTION_DAYS))
                .await?,
        };
        if out != Housekeeping::default() {
            tracing::info!(
                redacted = out.redacted,
                traces_pruned = out.traces_pruned,
                notifications_pruned = out.notifications_pruned,
                "trace housekeeping"
            );
        }
        Ok(out)
    }

    // ----- member settings -----------------------------------------------------------------

    /// A PM's settings; an unset level reads as `senior`.
    pub async fn member_settings(&self, handle: &str) -> Result<MemberSettings> {
        if !valid_handle(handle) {
            return Err(Error::BadRequest(format!(
                "`{handle}` isn't a PM OS handle"
            )));
        }
        Ok(self
            .store
            .member_settings(handle)
            .await?
            .unwrap_or_else(|| MemberSettings {
                handle: handle.to_string(),
                level: Level::default(),
                updated_at: DateTime::<Utc>::UNIX_EPOCH,
            }))
    }

    /// Set a PM's level. Only people who may ask bots have settings (on a snapshot at most
    /// 30 seconds old).
    pub async fn set_member_level(
        &self,
        handle: &str,
        level: Level,
        now: DateTime<Utc>,
    ) -> Result<MemberSettings> {
        if !valid_handle(handle) {
            return Err(Error::BadRequest(format!(
                "`{handle}` isn't a PM OS handle"
            )));
        }
        let people = self
            .people
            .fresh()
            .ok_or_else(|| Error::Unavailable(people::UNAVAILABLE.into()))?;
        people::may_ask_bots(&people, handle)
            .map_err(|why| Error::Forbidden(format!("{why}, so has no settings to change")))?;
        let settings = MemberSettings {
            handle: handle.to_string(),
            level,
            updated_at: now,
        };
        self.store.set_member_settings(&settings).await?;
        tracing::info!(%handle, %level, "level set");
        Ok(settings)
    }

    // ----- routines (D8) -------------------------------------------------------------------

    /// The bot, if `owner` may use it now (a snapshot at most 30 seconds old: parsing calls
    /// the model as them, and a routine runs as them).
    fn bot_for(&self, name: &str, owner: &str) -> Result<Bot> {
        let bot = self.bot(name)?;
        self.may_ask(self.people.fresh(), owner, &bot)?;
        Ok(bot)
    }

    /// Ask the model to turn `text` into a routine draft, and check what it says.
    pub async fn parse_routine(
        &self,
        bot: &str,
        owner: &str,
        text: &str,
        tz: &str,
        now: DateTime<Utc>,
    ) -> Result<(Draft, String)> {
        routines::parse_tz(tz).map_err(Error::BadRequest)?;
        let bot = self.bot_for(bot, owner)?;
        let text = trimmed(text);
        if text.is_empty() {
            return Err(Error::Unclear(routines::ASK_AGAIN.into()));
        }
        if let Some(e) = too_long("the request", &text, 4_000) {
            return Err(e);
        }
        let reply = self
            .gateway
            .extract_json(&routines::system_prompt(&bot.name, tz, now), &text, owner)
            .await
            .map_err(|e| Error::Unavailable(format!("{e}; try again in a minute")))?;
        match routines::read_reply(&reply, &bot.name, owner, tz, now) {
            Parsed::Draft(draft) => {
                let summary = routines::summary(&draft, bot.scope == BotScope::Personal);
                Ok((draft, summary))
            }
            Parsed::Question(q) => Err(Error::Unclear(q)),
        }
    }

    pub async fn create_routine(&self, draft: Draft, now: DateTime<Utc>) -> Result<Routine> {
        self.bot_for(&draft.bot, &draft.owner)?;
        if draft.approval != routines::APPROVAL {
            return Err(Error::BadRequest(
                "a routine's approval setting is `approve` in v1: every outbound action waits for you".into(),
            ));
        }
        if draft.output.kind != "dm" {
            return Err(Error::BadRequest(
                "a routine's output is a DM to its owner (`{\"kind\": \"dm\"}`) in v1".into(),
            ));
        }
        let prompt = trimmed(&draft.prompt);
        if prompt.is_empty() {
            return Err(Error::BadRequest("the routine has no prompt".into()));
        }
        if let Some(e) = too_long("the prompt", &prompt, MAX_TEXT_CHARS) {
            return Err(e);
        }
        let next = routines::next_run(&draft.schedule, now).map_err(Error::BadRequest)?;
        let routine = Routine {
            id: model::new_routine_id(),
            bot: draft.bot,
            owner: draft.owner,
            enabled: true,
            schedule: Schedule {
                cron: draft
                    .schedule
                    .cron
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                tz: draft.schedule.tz,
            },
            prompt,
            output: draft.output,
            approval: draft.approval,
            next_run_at: Some(next),
            last_run_at: None,
            created_at: now,
            updated_at: now,
        };
        self.store.create_routine(&routine).await?;
        tracing::info!(routine = %routine.id, bot = %routine.bot, "routine created");
        Ok(routine)
    }

    pub async fn routine(&self, id: &str) -> Result<Routine> {
        self.store
            .routine(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no routine {id}")))
    }

    /// Pause, resume, reschedule or reword a routine. Resuming or rescheduling counts from
    /// now, so a slot that passed while paused doesn't run on resume.
    pub async fn update_routine(
        &self,
        id: &str,
        change: RoutineChange,
        now: DateTime<Utc>,
    ) -> Result<Routine> {
        let current = self.routine(id).await?;
        let mut patch = RoutinePatch {
            enabled: change.enabled,
            ..RoutinePatch::default()
        };
        if let Some(p) = change.prompt {
            let p = trimmed(&p);
            if p.is_empty() {
                return Err(Error::BadRequest("the routine has no prompt".into()));
            }
            if let Some(e) = too_long("the prompt", &p, MAX_TEXT_CHARS) {
                return Err(e);
            }
            patch.prompt = Some(p);
        }
        let schedule = match change.schedule {
            Some(s) => {
                let s = Schedule {
                    cron: s.cron.split_whitespace().collect::<Vec<_>>().join(" "),
                    tz: s.tz,
                };
                routines::next_run(&s, now).map_err(Error::BadRequest)?;
                patch.schedule = Some(s.clone());
                Some(s)
            }
            None => None,
        };
        let enabled = change.enabled.unwrap_or(current.enabled);
        if !enabled {
            patch.next_run_at = Some(None);
        } else if schedule.is_some() || (change.enabled == Some(true) && !current.enabled) {
            let s = schedule.as_ref().unwrap_or(&current.schedule);
            patch.next_run_at = Some(Some(routines::next_run(s, now).map_err(Error::BadRequest)?));
        }
        self.store
            .update_routine(id, &patch, now)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no routine {id}")))
    }

    pub async fn delete_routine(&self, id: &str) -> Result<()> {
        if self.store.delete_routine(id).await? {
            Ok(())
        } else {
            Err(Error::NotFound(format!("no routine {id}")))
        }
    }

    /// Enqueue every due routine once, and schedule its next run from now. A routine that
    /// was due several times while the runtime was down runs once, not once per missed slot.
    ///
    /// Who may run it comes from a directory snapshot at most 5 minutes old. Without one,
    /// each due run is skipped (the routine stays enabled, its next run is scheduled) and
    /// logged: nothing runs, and nothing is paused, on data that old (spec §2).
    pub async fn run_due_routines(&self, now: DateTime<Utc>) -> Result<usize> {
        let due = self.store.due_routines(now).await?;
        if due.is_empty() {
            return Ok(0);
        }
        let Some(people) = self.people.for_due_routine() else {
            for r in due {
                self.skip_routine_run(&r, now).await?;
            }
            return Ok(0);
        };
        let mut queued = 0;
        for r in due {
            let Some(due_at) = r.next_run_at else {
                continue;
            };
            // Why this run can't go ahead, if it can't; the routine is then paused.
            let catalog = self.catalog();
            let problem = match catalog.bot(&r.bot) {
                None => Some(format!("its bot `{}` no longer exists", r.bot)),
                Some(bot) => people::may_ask(&people, &catalog, &r.owner, bot).err(),
            };
            let next = routines::next_run(&r.schedule, now);
            let problem = problem.or_else(|| next.as_ref().err().cloned());
            let run = match problem {
                None => RoutineRun {
                    routine_id: r.id.clone(),
                    due_at,
                    next_run_at: next.ok(),
                    task: Task::queued(
                        &r.bot,
                        &r.owner,
                        Source::Routine,
                        &r.prompt,
                        None,
                        Some(r.id.clone()),
                        now,
                    ),
                    event: "queued by its routine".into(),
                    notify: None,
                },
                Some(problem) => paused_run(&r, due_at, &problem, now),
            };
            if self.store.run_routine(&run, now).await? {
                tracing::info!(routine = %r.id, task = %run.task.id, status = %run.task.status, "routine ran");
                queued += 1;
            }
        }
        if queued > 0 {
            self.bump();
        }
        Ok(queued)
    }

    /// Skip one due run of `r` because the directory is too old to decide on: schedule the
    /// next run, keep the routine enabled, queue nothing, and log it.
    async fn skip_routine_run(&self, r: &Routine, now: DateTime<Utc>) -> Result<()> {
        let Some(due_at) = r.next_run_at else {
            return Ok(());
        };
        let age = self.people.age().map(|a| a.as_secs());
        match routines::next_run(&r.schedule, now) {
            Ok(next) => {
                if self
                    .store
                    .skip_routine_run(&r.id, due_at, next, now)
                    .await?
                {
                    tracing::warn!(routine = %r.id, ?age, next = %next, "routine run skipped: the user directory is more than 5 minutes old, so PM OS can't check its owner");
                }
            }
            // A schedule that no longer parses pauses it when the directory is back.
            Err(e) => {
                tracing::warn!(routine = %r.id, ?age, error = %e, "routine run skipped: the user directory is more than 5 minutes old")
            }
        }
        Ok(())
    }

    /// Pause the enabled routines whose owner may no longer ask bots (deactivated, removed,
    /// or holding no role that may ask), with the usual "paused" note, as soon as a snapshot
    /// at most 30 seconds old shows it. A routine whose bot is gone is left to its next due
    /// run. Returns how many were paused.
    pub async fn pause_routines_of_inactive_owners(&self, now: DateTime<Utc>) -> Result<usize> {
        let Some(people) = self.people.fresh() else {
            return Ok(0);
        };
        let mut paused = 0;
        for r in self.store.routines(None).await? {
            let (true, Some(due_at)) = (r.enabled, r.next_run_at) else {
                continue;
            };
            let Err(problem) = people::may_ask_bots(&people, &r.owner) else {
                continue;
            };
            let run = paused_run(&r, due_at, &problem, now);
            // Guarded like a due run: a routine changed meanwhile is left for the next pass.
            if self.store.run_routine(&run, now).await? {
                tracing::info!(routine = %r.id, owner = %r.owner, "routine paused: its owner may no longer ask bots");
                paused += 1;
            }
        }
        if paused > 0 {
            self.bump();
        }
        Ok(paused)
    }

    // ----- readiness and loops -------------------------------------------------------------

    /// Each of storage, the tool server, the gateway and the user directory (a snapshot at
    /// most 30 seconds old) is up or not.
    pub async fn readiness(&self) -> Readiness {
        let storage = async {
            matches!(
                tokio::time::timeout(Duration::from_secs(2), self.store.ping()).await,
                Ok(Ok(()))
            )
        };
        let (storage, toolserver, gateway) =
            tokio::join!(storage, self.toolserver.ready(), self.gateway.ready());
        Readiness {
            storage,
            toolserver,
            gateway,
            directory: self.people.fresh().is_some(),
        }
    }

    /// One pass of the 30-second loop.
    pub async fn tick(&self, now: DateTime<Utc>) {
        if let Err(e) = self.recover_leases(now).await {
            tracing::error!(error = %e, "lease recovery failed");
        }
        if let Err(e) = self.pause_routines_of_inactive_owners(now).await {
            tracing::error!(error = %e, "pausing routines of inactive owners failed");
        }
        if let Err(e) = self.run_due_routines(now).await {
            tracing::error!(error = %e, "routine scheduling failed");
        }
        if let Err(e) = self.check_approvals(now).await {
            tracing::error!(error = %e, "approval check failed");
        }
    }

    /// The background loops: every 10 seconds poll the user directory, and pause the
    /// routines of anyone it newly shows may no longer ask; every 30 seconds recover leases,
    /// run due routines and close approvals; every hour redact and prune traces. They run
    /// once straight away, which is the start-up recovery and the one catch-up run of a
    /// missed routine.
    pub fn spawn_loops(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let poller = self
            .people
            .spawn(pmos_directory::snapshot::POLL_EVERY, self.stop.clone());
        let pauser = {
            let rt = self.clone();
            let mut changes = self.people.changes();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rt.stop.cancelled() => break,
                        changed = changes.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            if let Err(e) = rt.pause_routines_of_inactive_owners(model::now()).await {
                                tracing::error!(error = %e, "pausing routines of inactive owners failed");
                            }
                        }
                    }
                }
            })
        };
        let ticker = {
            let rt = self.clone();
            tokio::spawn(async move {
                let mut every = tokio::time::interval(TICK);
                every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = rt.stop.cancelled() => break,
                        _ = every.tick() => rt.tick(model::now()).await,
                    }
                }
            })
        };
        let housekeeper = {
            let rt = self.clone();
            tokio::spawn(async move {
                let mut every = tokio::time::interval(HOUSEKEEPING);
                every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = rt.stop.cancelled() => break,
                        _ = every.tick() => {
                            if let Err(e) = rt.housekeeping(model::now()).await {
                                tracing::error!(error = %e, "trace housekeeping failed");
                            }
                        }
                    }
                }
            })
        };
        vec![poller, pauser, ticker, housekeeper]
    }
}

/// [`Runtime::readiness`]: each dependency up or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub storage: bool,
    pub toolserver: bool,
    pub gateway: bool,
    /// A directory snapshot at most 30 seconds old.
    pub directory: bool,
}

impl Readiness {
    pub fn ok(&self) -> bool {
        self.storage && self.toolserver && self.gateway && self.directory
    }
}

/// The run that pauses `r` with the reason: a failed task saying "Your routine is paused: …"
/// and a notification to its owner; the routine stops (no next run).
fn paused_run(r: &Routine, due_at: DateTime<Utc>, problem: &str, now: DateTime<Utc>) -> RoutineRun {
    let reason = format!("Your routine is paused: {problem}.");
    let mut task = Task::queued(
        &r.bot,
        &r.owner,
        Source::Routine,
        &r.prompt,
        None,
        Some(r.id.clone()),
        now,
    );
    task.status = TaskStatus::Failed;
    task.error = Some(reason.clone());
    RoutineRun {
        routine_id: r.id.clone(),
        due_at,
        next_run_at: None,
        task,
        event: format!("failed: {reason}"),
        notify: Some(NewNotification::new(NotificationKind::Failed, reason)),
    }
}

/// "confluence.create_draft was approved by ana; slack.send_message was rejected by ana."
pub fn describe_outcomes(outcomes: &[ApprovalOutcome]) -> String {
    let parts: Vec<String> = outcomes
        .iter()
        .map(|o| {
            let what = o.tool.clone().unwrap_or_else(|| o.id.clone());
            let by = o
                .decided_by
                .as_ref()
                .map(|b| format!(" by {b}"))
                .unwrap_or_default();
            match o.status.as_str() {
                "approved" => format!("{what} was approved{by}"),
                "rejected" => format!("{what} was rejected{by}"),
                "failed" => format!("{what} was approved{by}, but the write failed"),
                "expired" => format!("{what} expired undecided; nothing was sent"),
                "missing" => format!("{what} is no longer in the approval queue"),
                other => format!("{what} is {other}"),
            }
        })
        .collect();
    if parts.is_empty() {
        "Nothing was waiting for approval.".into()
    } else {
        format!("{}.", parts.join("; "))
    }
}

/// `s` cut to at most [`TURN_FIELD_BYTES`] bytes on a character boundary, with [`TRUNCATED`]
/// after when it was cut.
pub fn truncate_field(s: &str) -> String {
    if s.len() <= TURN_FIELD_BYTES {
        return s.to_string();
    }
    let mut end = TURN_FIELD_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &s[..end])
}

/// Earlier tasks (newest first, as the store returns them) as a claim's history, oldest
/// first: redacted turns left out, long fields cut, and older turns dropped once the total
/// passes [`HISTORY_BYTES`].
pub fn bounded_history(newest_first: Vec<Task>) -> Vec<Turn> {
    bounded_history_with_level(newest_first).0
}

/// [`bounded_history`], and the highest data class among the turns it kept (`None` when it
/// kept none). A turn with no recorded level counts as `restricted` (W6).
pub fn bounded_history_with_level(newest_first: Vec<Task>) -> (Vec<Turn>, Option<Sensitivity>) {
    let redacted = |s: &Option<String>| s.as_deref().is_some_and(|s| s.contains(REDACTED_MARKER));
    let mut turns = Vec::new();
    let mut level: Option<Sensitivity> = None;
    let mut bytes = 0;
    for t in newest_first.into_iter().take(HISTORY_TURNS) {
        if t.text.contains(REDACTED_MARKER) || redacted(&t.result) || redacted(&t.error) {
            continue;
        }
        let turn = Turn {
            text: truncate_field(&t.text),
            result: t.result.as_deref().map(truncate_field),
            error: t.error.as_deref().map(truncate_field),
            status: t.status,
            at: t.created_at,
        };
        let size = turn.text.len()
            + turn.result.as_ref().map_or(0, String::len)
            + turn.error.as_ref().map_or(0, String::len);
        if bytes + size > HISTORY_BYTES {
            break;
        }
        bytes += size;
        level = level.max(Some(
            t.sources_sensitivity.unwrap_or(Sensitivity::Restricted),
        ));
        turns.push(turn);
    }
    turns.reverse();
    (turns, level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earlier(text: &str, result: Option<&str>, minute: u32) -> Task {
        use chrono::TimeZone;
        let mut t = Task::queued(
            "docs",
            "ana",
            Source::Slack,
            text,
            None,
            None,
            Utc.with_ymd_and_hms(2026, 11, 14, 9, minute, 0).unwrap(),
        );
        t.status = TaskStatus::Done;
        t.result = result.map(String::from);
        t
    }

    #[test]
    fn history_is_oldest_first_and_bounded() {
        // Newest first, as the store returns them: eight turns, so the two oldest go.
        let tasks: Vec<Task> = (0..8)
            .rev()
            .map(|m| earlier(&format!("q{m}"), Some(&format!("a{m}")), m))
            .collect();
        let turns = bounded_history(tasks);
        assert_eq!(
            turns.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["q2", "q3", "q4", "q5", "q6", "q7"]
        );
        assert_eq!(turns[5].result.as_deref(), Some("a7"));
        assert!(bounded_history(Vec::new()).is_empty());
    }

    #[test]
    fn long_results_are_cut_and_the_oldest_dropped_past_the_byte_cap() {
        let long = "é".repeat(TURN_FIELD_BYTES); // two bytes each
        let tasks: Vec<Task> = (0..6)
            .rev()
            .map(|m| earlier(&format!("q{m}"), Some(&long), m))
            .collect();
        let turns = bounded_history(tasks);
        let cut = turns[0].result.as_deref().unwrap();
        assert!(cut.ends_with(TRUNCATED));
        assert!(cut.len() <= TURN_FIELD_BYTES + TRUNCATED.len());
        let total: usize = turns
            .iter()
            .map(|t| t.text.len() + t.result.as_ref().map_or(0, String::len))
            .sum();
        assert!(total <= HISTORY_BYTES, "{total}");
        assert_eq!(turns.len(), 3, "each turn is about 4 KB, so three fit");
        assert_eq!(turns.last().unwrap().text, "q5", "the newest are kept");
        assert_eq!(truncate_field("short"), "short");
    }

    #[test]
    fn redacted_turns_are_left_out() {
        let tasks = vec![
            earlier("fine", Some("ok"), 3),
            earlier(&format!("{{\"redacted\": \"{REDACTED_MARKER}\"}}"), None, 2),
            earlier("asked", Some(REDACTED_MARKER), 1),
        ];
        let turns = bounded_history(tasks);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "fine");
    }

    #[test]
    fn outcomes_read_plainly() {
        let o = |tool: &str, status: &str, by: Option<&str>| ApprovalOutcome {
            id: "apr_1".into(),
            tool: Some(tool.into()),
            status: status.into(),
            decided_by: by.map(String::from),
        };
        assert_eq!(
            describe_outcomes(&[
                o("confluence.create_draft", "approved", Some("ana")),
                o("slack.send_message", "rejected", Some("ana")),
                o("jira.create_issue", "failed", None),
            ]),
            "confluence.create_draft was approved by ana; slack.send_message was rejected by ana; \
             jira.create_issue was approved, but the write failed."
        );
        assert_eq!(
            describe_outcomes(&[ApprovalOutcome {
                id: "apr_9".into(),
                tool: None,
                status: "missing".into(),
                decided_by: None
            }]),
            "apr_9 is no longer in the approval queue."
        );
        assert_eq!(
            describe_outcomes(&[o("slack.post_reply", "expired", None)]),
            "slack.post_reply expired undecided; nothing was sent."
        );
    }
}
