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
    Notification, NotificationKind, Routine, Schedule, SlackRef, Source, Task, TaskStatus,
    TraceKind, GAVE_UP_REASON, LEASE_SECONDS, MAX_ATTEMPTS, RETRY_EVENT,
};
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
    #[error(transparent)]
    Store(#[from] StoreError),
}

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
    /// The bots, roles and team, swapped whole when the content changes (`catalog::Reloader`).
    /// Each use takes its own snapshot, so a reload never changes a catalog mid-call.
    catalog: RwLock<Arc<Catalog>>,
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
        toolserver: ToolServer,
        gateway: Gateway,
        stop: CancellationToken,
    ) -> Self {
        Self {
            store,
            catalog: RwLock::new(catalog),
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
        let bot = self.bot(&new.bot)?;
        self.catalog()
            .may_use(&new.requested_by, &bot)
            .map_err(Error::Forbidden)?;
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
            match self
                .toolserver
                .create_session(&task.requested_by, &bot.name, &task.id)
                .await
            {
                Ok(session_token) => {
                    let level = self.claim_level(&task).await;
                    tracing::info!(task = %task.id, bot = %bot.name, %worker, attempt = task.attempts, %level, "task claimed");
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
    /// cursor (the last item's, or `after` if none) and the items in order.
    pub async fn notifications(
        &self,
        after: i64,
        wait: Duration,
    ) -> Result<(i64, Vec<Notification>)> {
        let until = Instant::now() + wait.min(MAX_WAIT);
        let mut rx = self.changed.subscribe();
        loop {
            rx.borrow_and_update();
            let items = self.store.notifications(after, NOTIFICATION_PAGE).await?;
            if let Some(last) = items.last() {
                return Ok((last.seq, items));
            }
            if !self.wait_for_change(&mut rx, until).await {
                return Ok((after, items));
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

    /// Set a PM's level. Only members of `team.yaml` have settings.
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
        if !self.catalog().is_member(handle) {
            return Err(Error::Forbidden(format!(
                "`{handle}` isn't listed in team.yaml, so has no settings to change"
            )));
        }
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

    fn bot_for(&self, name: &str, owner: &str) -> Result<Bot> {
        let bot = self.bot(name)?;
        self.catalog()
            .may_use(owner, &bot)
            .map_err(Error::Forbidden)?;
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
    pub async fn run_due_routines(&self, now: DateTime<Utc>) -> Result<usize> {
        let mut queued = 0;
        for r in self.store.due_routines(now).await? {
            let Some(due_at) = r.next_run_at else {
                continue;
            };
            // Why this run can't go ahead, if it can't; the routine is then paused.
            let catalog = self.catalog();
            let problem = match catalog.bot(&r.bot) {
                None => Some(format!("its bot `{}` no longer exists", r.bot)),
                Some(bot) => catalog.may_use(&r.owner, bot).err(),
            };
            let next = routines::next_run(&r.schedule, now);
            let problem = problem.or_else(|| next.as_ref().err().cloned());
            let mut task = Task::queued(
                &r.bot,
                &r.owner,
                Source::Routine,
                &r.prompt,
                None,
                Some(r.id.clone()),
                now,
            );
            let run = match problem {
                None => RoutineRun {
                    routine_id: r.id.clone(),
                    due_at,
                    next_run_at: next.ok(),
                    task,
                    event: "queued by its routine".into(),
                    notify: None,
                },
                Some(problem) => {
                    let reason = format!("Your routine is paused: {problem}.");
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

    // ----- readiness and loops -------------------------------------------------------------

    /// `(storage, tool server, gateway)`, each up or not.
    pub async fn readiness(&self) -> (bool, bool, bool) {
        let storage = async {
            matches!(
                tokio::time::timeout(Duration::from_secs(2), self.store.ping()).await,
                Ok(Ok(()))
            )
        };
        tokio::join!(storage, self.toolserver.ready(), self.gateway.ready())
    }

    /// One pass of the 30-second loop.
    pub async fn tick(&self, now: DateTime<Utc>) {
        if let Err(e) = self.recover_leases(now).await {
            tracing::error!(error = %e, "lease recovery failed");
        }
        if let Err(e) = self.run_due_routines(now).await {
            tracing::error!(error = %e, "routine scheduling failed");
        }
        if let Err(e) = self.check_approvals(now).await {
            tracing::error!(error = %e, "approval check failed");
        }
    }

    /// The background loops: every 30 seconds recover leases, run due routines and close
    /// approvals; every hour redact and prune traces. Both run once straight away, which is
    /// the start-up recovery and the one catch-up run of a missed routine.
    pub fn spawn_loops(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
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
        vec![ticker, housekeeper]
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

#[cfg(test)]
mod tests {
    use super::*;

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
