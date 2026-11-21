//! Where tasks, events, traces, notifications and routines live: in memory (`PMOS_DEV=1`,
//! tests) or in Postgres (`pg.rs`). Both pass the same contract tests (`tests/store.rs`).
//!
//! Every change to a task goes through [`BotsStore::transition`], which checks the task's
//! current state and writes the change, its event and its notification together. That is
//! what keeps a crash from leaving a task done with no "done" notification, or two workers
//! finishing the same task.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{
    is_slack_tool, redacted_body, trace_tool, ChecklistItem, Event, MemberSettings,
    NewNotification, Notification, Routine, Schedule, Task, TaskStatus, TraceEntry, TraceKind,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("storage error: {0}")]
pub struct StoreError(pub String);

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError(e.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Which tasks to list, newest first.
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub bot: Option<String>,
    pub requested_by: Option<String>,
    pub status: Option<TaskStatus>,
    pub source: Option<crate::model::Source>,
    /// `Some(None)`: Nudge's tasks only; `Some(Some(key))`: that persona's.
    pub persona: Option<Option<String>>,
    pub limit: usize,
}

impl TaskFilter {
    fn matches(&self, t: &Task) -> bool {
        self.bot.as_ref().is_none_or(|b| &t.bot == b)
            && self
                .requested_by
                .as_ref()
                .is_none_or(|r| &t.requested_by == r)
            && self.status.is_none_or(|s| t.status == s)
            && self.source.is_none_or(|s| t.source == s)
            && self.persona.as_ref().is_none_or(|p| &t.persona == p)
    }
}

/// What a transition does to the lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lease {
    #[default]
    Keep,
    /// No claim any more (back to the queue, or finished).
    Clear,
    /// Extend the current claim to this time.
    Until(DateTime<Utc>),
}

/// A checked change to one task. It applies only if the task is in one of `from`, held by
/// `worker` (when given), and past its lease (when `lease_expired`).
#[derive(Debug, Clone, Default)]
pub struct Transition {
    pub from: Vec<TaskStatus>,
    pub worker: Option<String>,
    pub lease_expired: bool,
    pub to: Option<TaskStatus>,
    pub lease: Lease,
    pub result: Option<String>,
    pub error: Option<String>,
    pub approvals: Option<Vec<String>>,
    pub checklist: Option<Vec<ChecklistItem>>,
    /// The highest data class the task's sessions read (`Task::sources_sensitivity`).
    pub sensitivity: Option<crate::model::Sensitivity>,
    pub attempts_delta: i32,
    /// `(kind, text)` added to the task's events.
    pub event: Option<(String, String)>,
    pub notify: Option<NewNotification>,
}

impl Transition {
    pub fn from(from: &[TaskStatus]) -> Self {
        Self {
            from: from.to_vec(),
            ..Self::default()
        }
    }
}

/// Why a transition didn't apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    NotFound,
    /// The task is in this state, not one the transition starts from.
    Status(TaskStatus),
    /// Another worker holds the claim.
    NotYourClaim,
    /// The lease hasn't lapsed (a heartbeat won the race).
    LeaseActive,
}

/// The parts of a routine `PATCH /v1/routines/{id}` may change.
#[derive(Debug, Clone, Default)]
pub struct RoutinePatch {
    pub enabled: Option<bool>,
    pub schedule: Option<Schedule>,
    pub prompt: Option<String>,
    /// `Some(None)` moves the routine to Nudge; `Some(Some(key))` to that persona.
    pub persona: Option<Option<String>>,
    /// Recomputed by the caller whenever the schedule or `enabled` changes.
    pub next_run_at: Option<Option<DateTime<Utc>>>,
}

/// One scheduled run of a routine: the task it enqueues and when it runs next. It applies
/// only if the routine is still enabled and still due at `due_at`, so a run is never
/// enqueued twice.
#[derive(Debug, Clone)]
pub struct RoutineRun {
    pub routine_id: String,
    pub due_at: DateTime<Utc>,
    /// `None` pauses the routine (its bot is gone, or its owner may no longer use it).
    pub next_run_at: Option<DateTime<Utc>>,
    pub task: Task,
    /// The task's first event.
    pub event: String,
    pub notify: Option<NewNotification>,
}

#[async_trait]
pub trait BotsStore: Send + Sync {
    /// Is storage answering? For `/readyz`.
    async fn ping(&self) -> StoreResult<()>;

    /// Add a new task, with its first event.
    async fn create_task(&self, task: &Task, event: &str) -> StoreResult<()>;
    async fn task(&self, id: &str) -> StoreResult<Option<Task>>;
    async fn tasks(&self, filter: &TaskFilter) -> StoreResult<Vec<Task>>;
    /// Claim the next queued task (interactive first, then oldest), optionally only for
    /// `bots`. Exactly one caller wins each task, however many ask at once.
    async fn claim(
        &self,
        bots: Option<&[String]>,
        worker: &str,
        lease_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Task>>;
    async fn transition(
        &self,
        id: &str,
        change: &Transition,
        now: DateTime<Utc>,
    ) -> StoreResult<Result<Task, Refused>>;
    /// The earlier turns of `task`'s conversation, newest first, at most `limit`: tasks of the
    /// same bot, the same requester, the same thread (`Task::thread_key`) and the same persona
    /// (`None` equals `None`), created at or after `since` and before `task`, and no longer
    /// queued or working. Empty for a task with no thread.
    async fn earlier_turns(
        &self,
        task: &Task,
        since: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Vec<Task>>;
    /// The persona of the newest task in `task`'s thread (same bot, requester and thread,
    /// any status, any age), or `None` when that task is Nudge's or the thread has no task
    /// yet. A task naming no persona inherits it (`docs/bot-marketplace-spec.md` §6,
    /// Conversations). `None` for a task with no thread.
    async fn thread_persona(&self, task: &Task) -> StoreResult<Option<String>>;
    /// `working` tasks whose lease lapsed before `now`.
    async fn expired_leases(&self, now: DateTime<Utc>) -> StoreResult<Vec<Task>>;
    async fn events(&self, task_id: &str) -> StoreResult<Vec<Event>>;

    async fn add_trace(
        &self,
        task_id: &str,
        entries: &[(TraceKind, Value)],
        now: DateTime<Utc>,
    ) -> StoreResult<()>;
    async fn trace(&self, task_id: &str) -> StoreResult<Vec<TraceEntry>>;
    /// Replace the body of every Slack tool result recorded before `before`. Returns how many.
    async fn redact_slack(&self, before: DateTime<Utc>) -> StoreResult<u64>;
    /// Delete trace entries recorded before `before`. Returns how many.
    async fn prune_traces(&self, before: DateTime<Utc>) -> StoreResult<u64>;

    /// Notifications after `after` (a sequence number), oldest first.
    async fn notifications(&self, after: i64, limit: usize) -> StoreResult<Vec<Notification>>;
    /// The last notification sequence number issued, whether or not that notification has
    /// been pruned since; `0` before the first. Never goes down while the store lives, so a
    /// cursor past it came from another store (or another process's memory).
    async fn feed_head(&self) -> StoreResult<i64>;
    /// Delete notifications created before `before`. Returns how many.
    async fn prune_notifications(&self, before: DateTime<Utc>) -> StoreResult<u64>;

    async fn create_routine(&self, routine: &Routine) -> StoreResult<()>;
    async fn routine(&self, id: &str) -> StoreResult<Option<Routine>>;
    /// All routines, or one owner's, oldest first.
    async fn routines(&self, owner: Option<&str>) -> StoreResult<Vec<Routine>>;
    async fn update_routine(
        &self,
        id: &str,
        patch: &RoutinePatch,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Routine>>;
    async fn delete_routine(&self, id: &str) -> StoreResult<bool>;
    /// Enabled routines due at or before `now`.
    async fn due_routines(&self, now: DateTime<Utc>) -> StoreResult<Vec<Routine>>;
    /// Enqueue one run of a routine. False if it was no longer due (another tick ran it, or
    /// it was paused or changed meanwhile).
    async fn run_routine(&self, run: &RoutineRun, now: DateTime<Utc>) -> StoreResult<bool>;
    /// Skip one run of a routine that is enabled and due at `due_at`: its next run becomes
    /// `next_run_at`, nothing is enqueued, and it stays enabled. False if it was no longer
    /// due, as for [`BotsStore::run_routine`].
    async fn skip_routine_run(
        &self,
        id: &str,
        due_at: DateTime<Utc>,
        next_run_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<bool>;

    /// A PM's settings, if they ever set any.
    async fn member_settings(&self, handle: &str) -> StoreResult<Option<MemberSettings>>;
    /// Save a PM's settings, replacing any they had.
    async fn set_member_settings(&self, settings: &MemberSettings) -> StoreResult<()>;
}

/// Apply `change` to `task` if its guards hold. Shared by both stores so they decide alike.
pub fn apply(task: &mut Task, change: &Transition, now: DateTime<Utc>) -> Result<(), Refused> {
    if !change.from.contains(&task.status) {
        return Err(Refused::Status(task.status));
    }
    if let Some(worker) = &change.worker {
        if task.worker.as_ref() != Some(worker) {
            return Err(Refused::NotYourClaim);
        }
    }
    if change.lease_expired && task.lease_until.is_none_or(|l| l >= now) {
        return Err(Refused::LeaseActive);
    }
    if let Some(to) = change.to {
        task.status = to;
    }
    match change.lease {
        Lease::Keep => {}
        Lease::Clear => {
            task.lease_until = None;
            task.worker = None;
        }
        Lease::Until(t) => task.lease_until = Some(t),
    }
    if let Some(r) = &change.result {
        task.result = Some(r.clone());
    }
    if let Some(e) = &change.error {
        task.error = Some(e.clone());
    }
    if let Some(a) = &change.approvals {
        task.approvals = a.clone();
    }
    if let Some(c) = &change.checklist {
        task.checklist = c.clone();
    }
    if let Some(s) = change.sensitivity {
        task.sources_sensitivity = Some(s);
    }
    task.attempts += change.attempts_delta;
    task.updated_at = now;
    Ok(())
}

/// Whether a change to `task` may put a notification in the Slack feed. An eval task never
/// does: benchmarks run as the benchmark member and must not post anywhere (spec §3).
pub fn notifies(task: &Task) -> bool {
    task.source != crate::model::Source::Eval
}

fn notification_for(
    task: &Task,
    seq: i64,
    n: &NewNotification,
    now: DateTime<Utc>,
) -> Notification {
    Notification {
        id: format!("ntf_{seq}"),
        seq,
        task_id: task.id.clone(),
        bot: task.bot.clone(),
        requested_by: task.requested_by.clone(),
        slack: task.slack.clone(),
        kind: n.kind,
        text: n.text.clone(),
        approvals: n.approvals.clone(),
        outcomes: n.outcomes.clone(),
        routine_id: task.routine_id.clone(),
        persona: task.persona.clone(),
        at: now,
    }
}

// ----- memory -----------------------------------------------------------------------------

struct StoredTrace {
    seq: u64,
    task_id: String,
    tool: Option<String>,
    redacted: bool,
    entry: TraceEntry,
}

#[derive(Default)]
struct State {
    tasks: BTreeMap<String, Task>,
    events: BTreeMap<String, Vec<Event>>,
    traces: Vec<StoredTrace>,
    trace_seq: u64,
    notifications: Vec<Notification>,
    notification_seq: i64,
    routines: BTreeMap<String, Routine>,
    member_settings: BTreeMap<String, MemberSettings>,
}

impl State {
    fn notify(&mut self, task: &Task, n: &NewNotification, now: DateTime<Utc>) {
        // A web task has no Slack thread to post in; its caller reads the task and its events.
        if task.source == crate::model::Source::Web {
            return;
        }
        self.notification_seq += 1;
        let item = notification_for(task, self.notification_seq, n, now);
        self.notifications.push(item);
    }

    fn event(&mut self, task_id: &str, kind: &str, text: &str, now: DateTime<Utc>) {
        self.events
            .entry(task_id.to_string())
            .or_default()
            .push(Event {
                at: now,
                kind: kind.to_string(),
                text: text.to_string(),
            });
    }
}

/// Everything behind one lock, so each operation is as atomic as a Postgres transaction.
#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<State>,
}

impl MemoryStore {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("bots store lock")
    }
}

#[async_trait]
impl BotsStore for MemoryStore {
    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }

    async fn create_task(&self, task: &Task, event: &str) -> StoreResult<()> {
        let mut s = self.state();
        if s.tasks.contains_key(&task.id) {
            return Err(StoreError(format!("task {} already exists", task.id)));
        }
        s.tasks.insert(task.id.clone(), task.clone());
        s.event(&task.id, "status", event, task.created_at);
        Ok(())
    }

    async fn task(&self, id: &str) -> StoreResult<Option<Task>> {
        Ok(self.state().tasks.get(id).cloned())
    }

    async fn tasks(&self, filter: &TaskFilter) -> StoreResult<Vec<Task>> {
        let s = self.state();
        let mut out: Vec<Task> = s
            .tasks
            .values()
            .filter(|t| filter.matches(t))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        out.truncate(filter.limit);
        Ok(out)
    }

    async fn claim(
        &self,
        bots: Option<&[String]>,
        worker: &str,
        lease_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Task>> {
        let mut s = self.state();
        let next = s
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Queued)
            .filter(|t| bots.is_none_or(|b| b.contains(&t.bot)))
            .min_by(|a, b| {
                (a.priority.rank(), a.created_at, &a.id).cmp(&(
                    b.priority.rank(),
                    b.created_at,
                    &b.id,
                ))
            })
            .map(|t| t.id.clone());
        let Some(id) = next else {
            return Ok(None);
        };
        let task = s.tasks.get_mut(&id).expect("task just found");
        task.status = TaskStatus::Working;
        task.attempts += 1;
        task.worker = Some(worker.to_string());
        task.lease_until = Some(lease_until);
        task.updated_at = now;
        let task = task.clone();
        s.event(&id, "status", &format!("claimed by worker {worker}"), now);
        Ok(Some(task))
    }

    async fn transition(
        &self,
        id: &str,
        change: &Transition,
        now: DateTime<Utc>,
    ) -> StoreResult<Result<Task, Refused>> {
        let mut s = self.state();
        let Some(task) = s.tasks.get_mut(id) else {
            return Ok(Err(Refused::NotFound));
        };
        let mut updated = task.clone();
        if let Err(r) = apply(&mut updated, change, now) {
            return Ok(Err(r));
        }
        *task = updated.clone();
        if let Some((kind, text)) = &change.event {
            s.event(id, kind, text, now);
        }
        if let Some(n) = change.notify.as_ref().filter(|_| notifies(&updated)) {
            s.notify(&updated, n, now);
        }
        Ok(Ok(updated))
    }

    async fn earlier_turns(
        &self,
        task: &Task,
        since: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Vec<Task>> {
        let Some(key) = task.thread_key() else {
            return Ok(Vec::new());
        };
        let s = self.state();
        let mut out: Vec<Task> = s
            .tasks
            .values()
            .filter(|t| {
                t.bot == task.bot
                    && t.requested_by == task.requested_by
                    && t.persona == task.persona
                    && t.thread_key().as_ref() == Some(&key)
                    && t.created_at >= since
                    && (&t.created_at, &t.id) < (&task.created_at, &task.id)
                    && !matches!(t.status, TaskStatus::Queued | TaskStatus::Working)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        out.truncate(limit);
        Ok(out)
    }

    async fn thread_persona(&self, task: &Task) -> StoreResult<Option<String>> {
        let Some(key) = task.thread_key() else {
            return Ok(None);
        };
        let s = self.state();
        Ok(s.tasks
            .values()
            .filter(|t| {
                t.id != task.id
                    && t.bot == task.bot
                    && t.requested_by == task.requested_by
                    && t.thread_key().as_ref() == Some(&key)
            })
            .max_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)))
            .and_then(|t| t.persona.clone()))
    }

    async fn expired_leases(&self, now: DateTime<Utc>) -> StoreResult<Vec<Task>> {
        Ok(self
            .state()
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Working && t.lease_until.is_some_and(|l| l < now))
            .cloned()
            .collect())
    }

    async fn events(&self, task_id: &str) -> StoreResult<Vec<Event>> {
        Ok(self
            .state()
            .events
            .get(task_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_trace(
        &self,
        task_id: &str,
        entries: &[(TraceKind, Value)],
        now: DateTime<Utc>,
    ) -> StoreResult<()> {
        let mut s = self.state();
        for (kind, body) in entries {
            s.trace_seq += 1;
            let seq = s.trace_seq;
            s.traces.push(StoredTrace {
                seq,
                task_id: task_id.to_string(),
                tool: trace_tool(body),
                redacted: false,
                entry: TraceEntry {
                    at: now,
                    kind: *kind,
                    body: body.clone(),
                },
            });
        }
        Ok(())
    }

    async fn trace(&self, task_id: &str) -> StoreResult<Vec<TraceEntry>> {
        let s = self.state();
        let mut rows: Vec<&StoredTrace> =
            s.traces.iter().filter(|t| t.task_id == task_id).collect();
        rows.sort_by_key(|t| t.seq);
        Ok(rows.into_iter().map(|t| t.entry.clone()).collect())
    }

    async fn redact_slack(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let mut s = self.state();
        let mut n = 0;
        for t in s.traces.iter_mut() {
            if !t.redacted
                && t.entry.kind == TraceKind::ToolResult
                && t.entry.at < before
                && t.tool.as_deref().is_some_and(is_slack_tool)
            {
                t.entry.body = redacted_body();
                t.redacted = true;
                n += 1;
            }
        }
        Ok(n)
    }

    async fn prune_traces(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let mut s = self.state();
        let len = s.traces.len();
        s.traces.retain(|t| t.entry.at >= before);
        Ok((len - s.traces.len()) as u64)
    }

    async fn notifications(&self, after: i64, limit: usize) -> StoreResult<Vec<Notification>> {
        Ok(self
            .state()
            .notifications
            .iter()
            .filter(|n| n.seq > after)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn feed_head(&self) -> StoreResult<i64> {
        Ok(self.state().notification_seq)
    }

    async fn prune_notifications(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let mut s = self.state();
        let len = s.notifications.len();
        s.notifications.retain(|n| n.at >= before);
        Ok((len - s.notifications.len()) as u64)
    }

    async fn create_routine(&self, routine: &Routine) -> StoreResult<()> {
        let mut s = self.state();
        if s.routines.contains_key(&routine.id) {
            return Err(StoreError(format!("routine {} already exists", routine.id)));
        }
        s.routines.insert(routine.id.clone(), routine.clone());
        Ok(())
    }

    async fn routine(&self, id: &str) -> StoreResult<Option<Routine>> {
        Ok(self.state().routines.get(id).cloned())
    }

    async fn routines(&self, owner: Option<&str>) -> StoreResult<Vec<Routine>> {
        let s = self.state();
        let mut out: Vec<Routine> = s
            .routines
            .values()
            .filter(|r| owner.is_none_or(|o| r.owner == o))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    async fn update_routine(
        &self,
        id: &str,
        patch: &RoutinePatch,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Routine>> {
        let mut s = self.state();
        let Some(r) = s.routines.get_mut(id) else {
            return Ok(None);
        };
        if let Some(e) = patch.enabled {
            r.enabled = e;
        }
        if let Some(sch) = &patch.schedule {
            r.schedule = sch.clone();
        }
        if let Some(p) = &patch.prompt {
            r.prompt = p.clone();
        }
        if let Some(p) = &patch.persona {
            r.persona = p.clone();
        }
        if let Some(n) = patch.next_run_at {
            r.next_run_at = n;
        }
        r.updated_at = now;
        Ok(Some(r.clone()))
    }

    async fn delete_routine(&self, id: &str) -> StoreResult<bool> {
        Ok(self.state().routines.remove(id).is_some())
    }

    async fn due_routines(&self, now: DateTime<Utc>) -> StoreResult<Vec<Routine>> {
        let s = self.state();
        let mut out: Vec<Routine> = s
            .routines
            .values()
            .filter(|r| r.enabled && r.next_run_at.is_some_and(|n| n <= now))
            .cloned()
            .collect();
        out.sort_by_key(|r| r.next_run_at);
        Ok(out)
    }

    async fn run_routine(&self, run: &RoutineRun, now: DateTime<Utc>) -> StoreResult<bool> {
        let mut s = self.state();
        let Some(r) = s.routines.get_mut(&run.routine_id) else {
            return Ok(false);
        };
        if !r.enabled || r.next_run_at != Some(run.due_at) {
            return Ok(false);
        }
        r.last_run_at = Some(now);
        r.next_run_at = run.next_run_at;
        r.enabled = run.next_run_at.is_some();
        r.updated_at = now;
        let task = run.task.clone();
        s.tasks.insert(task.id.clone(), task.clone());
        s.event(&task.id, "status", &run.event, now);
        if let Some(n) = run.notify.as_ref().filter(|_| notifies(&task)) {
            s.notify(&task, n, now);
        }
        Ok(true)
    }

    async fn skip_routine_run(
        &self,
        id: &str,
        due_at: DateTime<Utc>,
        next_run_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let mut s = self.state();
        let Some(r) = s.routines.get_mut(id) else {
            return Ok(false);
        };
        if !r.enabled || r.next_run_at != Some(due_at) {
            return Ok(false);
        }
        r.next_run_at = Some(next_run_at);
        r.updated_at = now;
        Ok(true)
    }

    async fn member_settings(&self, handle: &str) -> StoreResult<Option<MemberSettings>> {
        Ok(self.state().member_settings.get(handle).cloned())
    }

    async fn set_member_settings(&self, settings: &MemberSettings) -> StoreResult<()> {
        self.state()
            .member_settings
            .insert(settings.handle.clone(), settings.clone());
        Ok(())
    }
}
