//! The Postgres store. Claims use `FOR UPDATE SKIP LOCKED`, so any number of workers can
//! poll at once and each queued task goes to exactly one of them; every other change locks
//! the task's row and writes the change, its event and its notification in one transaction.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::{PgConnection, Row};

use crate::model::{
    redacted_body, trace_tool, ApprovalOutcome, ChecklistItem, Event, Level, MemberSettings,
    NewNotification, Notification, NotificationKind, Output, Priority, Routine, Schedule,
    Sensitivity, SlackRef, Source, Task, TaskStatus, TraceEntry, TraceKind,
};
use crate::store::{
    apply, notifies, BotsStore, Refused, RoutinePatch, RoutineRun, StoreError, StoreResult,
    TaskFilter, Transition,
};

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> StoreResult<()> {
        let mut migrator = sqlx::migrate!("./migrations");
        // Its own bookkeeping table, so sharing a database with the tool server and the docs
        // server (each with their own) can't clash.
        migrator.dangerous_set_table_name("_pmos_bots_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError(e.to_string()))
    }
}

const TASK_COLUMNS: &str = "id, bot, requested_by, source, priority, text, status, checklist, \
     result, error, approvals, slack, routine_id, level, conversation, sources_sensitivity, \
     persona, attempts, worker, lease_until, created_at, updated_at";

fn bad(what: &str, value: &str) -> StoreError {
    StoreError(format!("unexpected {what} `{value}` in storage"))
}

fn task_from_row(r: &PgRow) -> StoreResult<Task> {
    let source: String = r.try_get("source")?;
    let priority: String = r.try_get("priority")?;
    let status: String = r.try_get("status")?;
    let checklist: Json<Vec<ChecklistItem>> = r.try_get("checklist")?;
    let approvals: Json<Vec<String>> = r.try_get("approvals")?;
    let slack: Option<Json<SlackRef>> = r.try_get("slack")?;
    let level: Option<String> = r.try_get("level")?;
    let level = match level {
        Some(l) => Some(Level::parse(&l).ok_or_else(|| bad("level", &l))?),
        None => None,
    };
    let read: Option<String> = r.try_get("sources_sensitivity")?;
    let sources_sensitivity = match read {
        Some(s) => Some(Sensitivity::parse(&s).ok_or_else(|| bad("sensitivity", &s))?),
        None => None,
    };
    Ok(Task {
        id: r.try_get("id")?,
        bot: r.try_get("bot")?,
        requested_by: r.try_get("requested_by")?,
        source: Source::parse(&source).ok_or_else(|| bad("source", &source))?,
        priority: Priority::parse(&priority).ok_or_else(|| bad("priority", &priority))?,
        text: r.try_get("text")?,
        status: TaskStatus::parse(&status).ok_or_else(|| bad("status", &status))?,
        checklist: checklist.0,
        result: r.try_get("result")?,
        error: r.try_get("error")?,
        approvals: approvals.0,
        slack: slack.map(|s| s.0),
        routine_id: r.try_get("routine_id")?,
        level,
        conversation: r.try_get("conversation")?,
        sources_sensitivity,
        persona: r.try_get("persona")?,
        attempts: r.try_get("attempts")?,
        worker: r.try_get("worker")?,
        lease_until: r.try_get("lease_until")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const ROUTINE_COLUMNS: &str = "id, bot, owner, enabled, cron, tz, prompt, output, approval, \
     persona, next_run_at, last_run_at, created_at, updated_at";

fn routine_from_row(r: &PgRow) -> StoreResult<Routine> {
    let output: Json<Output> = r.try_get("output")?;
    Ok(Routine {
        id: r.try_get("id")?,
        bot: r.try_get("bot")?,
        owner: r.try_get("owner")?,
        enabled: r.try_get("enabled")?,
        schedule: Schedule {
            cron: r.try_get("cron")?,
            tz: r.try_get("tz")?,
        },
        prompt: r.try_get("prompt")?,
        output: output.0,
        approval: r.try_get("approval")?,
        persona: r.try_get("persona")?,
        next_run_at: r.try_get("next_run_at")?,
        last_run_at: r.try_get("last_run_at")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn insert_task(conn: &mut PgConnection, t: &Task) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO bots_tasks (id, bot, requested_by, source, priority, priority_rank, text,
             status, checklist, result, error, approvals, slack, routine_id, attempts, worker,
             lease_until, created_at, updated_at, level, conversation, thread_key,
             sources_sensitivity, persona)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
             $18, $19, $20, $21, $22, $23, $24)",
    )
    .bind(&t.id)
    .bind(&t.bot)
    .bind(&t.requested_by)
    .bind(t.source.as_str())
    .bind(t.priority.as_str())
    .bind(t.priority.rank())
    .bind(&t.text)
    .bind(t.status.as_str())
    .bind(Json(&t.checklist))
    .bind(&t.result)
    .bind(&t.error)
    .bind(Json(&t.approvals))
    .bind(t.slack.as_ref().map(Json))
    .bind(&t.routine_id)
    .bind(t.attempts)
    .bind(&t.worker)
    .bind(t.lease_until)
    .bind(t.created_at)
    .bind(t.updated_at)
    .bind(t.level.map(Level::as_str))
    .bind(&t.conversation)
    .bind(t.thread_key())
    .bind(t.sources_sensitivity.map(|s| s.to_string()))
    .bind(&t.persona)
    .execute(conn)
    .await?;
    Ok(())
}

async fn insert_event(
    conn: &mut PgConnection,
    task_id: &str,
    kind: &str,
    text: &str,
    now: DateTime<Utc>,
) -> StoreResult<()> {
    sqlx::query("INSERT INTO bots_task_events (task_id, at, kind, text) VALUES ($1, $2, $3, $4)")
        .bind(task_id)
        .bind(now)
        .bind(kind)
        .bind(text)
        .execute(conn)
        .await?;
    Ok(())
}

async fn insert_notification(
    conn: &mut PgConnection,
    task: &Task,
    n: &NewNotification,
    now: DateTime<Utc>,
) -> StoreResult<()> {
    // A web task has no Slack thread to post in; its caller reads the task and its events.
    if task.source == Source::Web {
        return Ok(());
    }
    // The counter row stays locked until this transaction commits, so a later number can't
    // become visible before an earlier one and a reader's cursor never skips an item.
    let seq: i64 = sqlx::query_scalar(
        "UPDATE bots_counters SET value = value + 1 WHERE name = 'notifications' RETURNING value",
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO bots_notifications (seq, task_id, bot, requested_by, slack, kind, text,
             approvals, outcomes, routine_id, at, persona)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(seq)
    .bind(&task.id)
    .bind(&task.bot)
    .bind(&task.requested_by)
    .bind(task.slack.as_ref().map(Json))
    .bind(n.kind.as_str())
    .bind(&n.text)
    .bind(Json(&n.approvals))
    .bind(Json(&n.outcomes))
    .bind(&task.routine_id)
    .bind(now)
    .bind(&task.persona)
    .execute(conn)
    .await?;
    Ok(())
}

#[async_trait]
impl BotsStore for PgStore {
    async fn ping(&self) -> StoreResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    async fn create_task(&self, task: &Task, event: &str) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        insert_task(&mut tx, task).await?;
        insert_event(&mut tx, &task.id, "status", event, task.created_at).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn task(&self, id: &str) -> StoreResult<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM bots_tasks WHERE id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(task_from_row).transpose()
    }

    async fn tasks(&self, f: &TaskFilter) -> StoreResult<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM bots_tasks
             WHERE ($1::text IS NULL OR bot = $1)
               AND ($2::text IS NULL OR requested_by = $2)
               AND ($3::text IS NULL OR status = $3)
               AND ($4::text IS NULL OR source = $4)
               AND (NOT $6 OR persona IS NOT DISTINCT FROM $7)
             ORDER BY created_at DESC, id DESC
             LIMIT $5"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&f.bot)
            .bind(&f.requested_by)
            .bind(f.status.map(TaskStatus::as_str))
            .bind(f.source.map(Source::as_str))
            .bind(f.limit as i64)
            .bind(f.persona.is_some())
            .bind(f.persona.clone().flatten())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(task_from_row).collect()
    }

    async fn claim(
        &self,
        bots: Option<&[String]>,
        worker: &str,
        lease_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Task>> {
        let mut tx = self.pool.begin().await?;
        // A row another claimer has locked is skipped, not waited on: two workers never get
        // the same task, and neither blocks the other.
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM bots_tasks
             WHERE status = 'queued' AND ($1::text[] IS NULL OR bot = ANY($1))
             ORDER BY priority_rank, created_at, id
             LIMIT 1
             FOR UPDATE SKIP LOCKED"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(bots.map(<[String]>::to_vec))
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let mut task = task_from_row(&row)?;
        task.status = TaskStatus::Working;
        task.attempts += 1;
        task.worker = Some(worker.to_string());
        task.lease_until = Some(lease_until);
        task.updated_at = now;
        sqlx::query(
            "UPDATE bots_tasks SET status = 'working', attempts = $2, worker = $3,
                 lease_until = $4, updated_at = $5
             WHERE id = $1",
        )
        .bind(&task.id)
        .bind(task.attempts)
        .bind(worker)
        .bind(lease_until)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_event(
            &mut tx,
            &task.id,
            "status",
            &format!("claimed by worker {worker}"),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Some(task))
    }

    async fn transition(
        &self,
        id: &str,
        change: &Transition,
        now: DateTime<Utc>,
    ) -> StoreResult<Result<Task, Refused>> {
        let mut tx = self.pool.begin().await?;
        let sql = format!("SELECT {TASK_COLUMNS} FROM bots_tasks WHERE id = $1 FOR UPDATE");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(Err(Refused::NotFound));
        };
        let mut task = task_from_row(&row)?;
        if let Err(r) = apply(&mut task, change, now) {
            // End the transaction now, so the row lock goes with it. A dropped transaction is
            // rolled back only when its connection is next used, and until then a claim
            // (`FOR UPDATE SKIP LOCKED`) would skip this task as if it weren't queued.
            tx.rollback().await?;
            return Ok(Err(r));
        }
        sqlx::query(
            "UPDATE bots_tasks SET status = $2, checklist = $3, result = $4, error = $5,
                 approvals = $6, attempts = $7, worker = $8, lease_until = $9, updated_at = $10,
                 sources_sensitivity = $11
             WHERE id = $1",
        )
        .bind(id)
        .bind(task.status.as_str())
        .bind(Json(&task.checklist))
        .bind(&task.result)
        .bind(&task.error)
        .bind(Json(&task.approvals))
        .bind(task.attempts)
        .bind(&task.worker)
        .bind(task.lease_until)
        .bind(now)
        .bind(task.sources_sensitivity.map(|s| s.to_string()))
        .execute(&mut *tx)
        .await?;
        if let Some((kind, text)) = &change.event {
            insert_event(&mut tx, id, kind, text, now).await?;
        }
        if let Some(n) = change.notify.as_ref().filter(|_| notifies(&task)) {
            insert_notification(&mut tx, &task, n, now).await?;
        }
        tx.commit().await?;
        Ok(Ok(task))
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
        // Ids compare bytewise (`COLLATE "C"`), as the memory store compares them.
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM bots_tasks
             WHERE bot = $1 AND requested_by = $2 AND thread_key = $3
               AND persona IS NOT DISTINCT FROM $8
               AND created_at >= $4
               AND (created_at < $5 OR (created_at = $5 AND id COLLATE \"C\" < $6))
               AND status NOT IN ('queued', 'working')
             ORDER BY created_at DESC, id COLLATE \"C\" DESC
             LIMIT $7"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&task.bot)
            .bind(&task.requested_by)
            .bind(key)
            .bind(since)
            .bind(task.created_at)
            .bind(&task.id)
            .bind(limit as i64)
            .bind(&task.persona)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(task_from_row).collect()
    }

    async fn thread_persona(&self, task: &Task) -> StoreResult<Option<String>> {
        let Some(key) = task.thread_key() else {
            return Ok(None);
        };
        let persona: Option<Option<String>> = sqlx::query_scalar(
            "SELECT persona FROM bots_tasks
             WHERE bot = $1 AND requested_by = $2 AND thread_key = $3 AND id <> $4
             ORDER BY created_at DESC, id COLLATE \"C\" DESC
             LIMIT 1",
        )
        .bind(&task.bot)
        .bind(&task.requested_by)
        .bind(key)
        .bind(&task.id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(persona.flatten())
    }

    async fn expired_leases(&self, now: DateTime<Utc>) -> StoreResult<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM bots_tasks
             WHERE status = 'working' AND lease_until < $1
             ORDER BY lease_until"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(task_from_row).collect()
    }

    async fn events(&self, task_id: &str) -> StoreResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT at, kind, text FROM bots_task_events WHERE task_id = $1 ORDER BY id",
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Event {
                    at: r.try_get("at")?,
                    kind: r.try_get("kind")?,
                    text: r.try_get("text")?,
                })
            })
            .collect()
    }

    async fn add_trace(
        &self,
        task_id: &str,
        entries: &[(TraceKind, Value)],
        now: DateTime<Utc>,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        for (kind, body) in entries {
            sqlx::query(
                "INSERT INTO bots_traces (task_id, at, kind, tool, body) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(task_id)
            .bind(now)
            .bind(kind.as_str())
            .bind(trace_tool(body))
            .bind(Json(body))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn trace(&self, task_id: &str) -> StoreResult<Vec<TraceEntry>> {
        let rows =
            sqlx::query("SELECT at, kind, body FROM bots_traces WHERE task_id = $1 ORDER BY id")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await?;
        rows.iter()
            .map(|r| {
                let kind: String = r.try_get("kind")?;
                let body: Json<Value> = r.try_get("body")?;
                Ok(TraceEntry {
                    at: r.try_get("at")?,
                    kind: TraceKind::parse(&kind).ok_or_else(|| bad("trace kind", &kind))?,
                    body: body.0,
                })
            })
            .collect()
    }

    async fn redact_slack(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let done = sqlx::query(
            "UPDATE bots_traces SET body = $2, redacted = true
             WHERE kind = 'tool_result' AND NOT redacted AND at < $1
               AND (tool LIKE 'slack.%' OR tool LIKE 'slack\\_%')",
        )
        .bind(before)
        .bind(Json(redacted_body()))
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    async fn prune_traces(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let done = sqlx::query("DELETE FROM bots_traces WHERE at < $1")
            .bind(before)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    async fn notifications(&self, after: i64, limit: usize) -> StoreResult<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT seq, task_id, bot, requested_by, slack, kind, text, approvals, outcomes,
                    routine_id, at, persona
             FROM bots_notifications WHERE seq > $1 ORDER BY seq LIMIT $2",
        )
        .bind(after)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let seq: i64 = r.try_get("seq")?;
                let kind: String = r.try_get("kind")?;
                let slack: Option<Json<SlackRef>> = r.try_get("slack")?;
                let approvals: Json<Vec<String>> = r.try_get("approvals")?;
                let outcomes: Json<Vec<ApprovalOutcome>> = r.try_get("outcomes")?;
                Ok(Notification {
                    id: format!("ntf_{seq}"),
                    seq,
                    task_id: r.try_get("task_id")?,
                    bot: r.try_get("bot")?,
                    requested_by: r.try_get("requested_by")?,
                    slack: slack.map(|s| s.0),
                    kind: NotificationKind::parse(&kind)
                        .ok_or_else(|| bad("notification kind", &kind))?,
                    text: r.try_get("text")?,
                    approvals: approvals.0,
                    outcomes: outcomes.0,
                    routine_id: r.try_get("routine_id")?,
                    persona: r.try_get("persona")?,
                    at: r.try_get("at")?,
                })
            })
            .collect()
    }

    async fn feed_head(&self) -> StoreResult<i64> {
        // The counter the sequence numbers come from: committed values only, and pruning
        // never lowers it.
        let head: Option<i64> =
            sqlx::query_scalar("SELECT value FROM bots_counters WHERE name = 'notifications'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(head.unwrap_or(0))
    }

    async fn prune_notifications(&self, before: DateTime<Utc>) -> StoreResult<u64> {
        let done = sqlx::query("DELETE FROM bots_notifications WHERE at < $1")
            .bind(before)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    async fn create_routine(&self, r: &Routine) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO bots_routines (id, bot, owner, enabled, cron, tz, prompt, output,
                 approval, next_run_at, last_run_at, created_at, updated_at, persona)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(&r.id)
        .bind(&r.bot)
        .bind(&r.owner)
        .bind(r.enabled)
        .bind(&r.schedule.cron)
        .bind(&r.schedule.tz)
        .bind(&r.prompt)
        .bind(Json(&r.output))
        .bind(&r.approval)
        .bind(r.next_run_at)
        .bind(r.last_run_at)
        .bind(r.created_at)
        .bind(r.updated_at)
        .bind(&r.persona)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn routine(&self, id: &str) -> StoreResult<Option<Routine>> {
        let sql = format!("SELECT {ROUTINE_COLUMNS} FROM bots_routines WHERE id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(routine_from_row).transpose()
    }

    async fn routines(&self, owner: Option<&str>) -> StoreResult<Vec<Routine>> {
        let sql = format!(
            "SELECT {ROUTINE_COLUMNS} FROM bots_routines
             WHERE ($1::text IS NULL OR owner = $1)
             ORDER BY created_at, id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(owner)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(routine_from_row).collect()
    }

    async fn update_routine(
        &self,
        id: &str,
        patch: &RoutinePatch,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<Routine>> {
        let sql = format!(
            "UPDATE bots_routines SET
                 enabled = coalesce($2, enabled),
                 cron = coalesce($3, cron),
                 tz = coalesce($4, tz),
                 prompt = coalesce($5, prompt),
                 next_run_at = CASE WHEN $6 THEN $7 ELSE next_run_at END,
                 updated_at = $8,
                 persona = CASE WHEN $9 THEN $10 ELSE persona END
             WHERE id = $1
             RETURNING {ROUTINE_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(patch.enabled)
            .bind(patch.schedule.as_ref().map(|s| s.cron.clone()))
            .bind(patch.schedule.as_ref().map(|s| s.tz.clone()))
            .bind(&patch.prompt)
            .bind(patch.next_run_at.is_some())
            .bind(patch.next_run_at.flatten())
            .bind(now)
            .bind(patch.persona.is_some())
            .bind(patch.persona.clone().flatten())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(routine_from_row).transpose()
    }

    async fn delete_routine(&self, id: &str) -> StoreResult<bool> {
        let done = sqlx::query("DELETE FROM bots_routines WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn due_routines(&self, now: DateTime<Utc>) -> StoreResult<Vec<Routine>> {
        let sql = format!(
            "SELECT {ROUTINE_COLUMNS} FROM bots_routines
             WHERE enabled AND next_run_at <= $1
             ORDER BY next_run_at, id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(routine_from_row).collect()
    }

    async fn run_routine(&self, run: &RoutineRun, now: DateTime<Utc>) -> StoreResult<bool> {
        let mut tx = self.pool.begin().await?;
        let row =
            sqlx::query("SELECT enabled, next_run_at FROM bots_routines WHERE id = $1 FOR UPDATE")
                .bind(&run.routine_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(false);
        };
        let enabled: bool = row.try_get("enabled")?;
        let next: Option<DateTime<Utc>> = row.try_get("next_run_at")?;
        if !enabled || next != Some(run.due_at) {
            // Release the routine's row lock at once (see `transition`).
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE bots_routines SET last_run_at = $2, next_run_at = $3, enabled = $4,
                 updated_at = $2
             WHERE id = $1",
        )
        .bind(&run.routine_id)
        .bind(now)
        .bind(run.next_run_at)
        .bind(run.next_run_at.is_some())
        .execute(&mut *tx)
        .await?;
        insert_task(&mut tx, &run.task).await?;
        insert_event(&mut tx, &run.task.id, "status", &run.event, now).await?;
        if let Some(n) = run.notify.as_ref().filter(|_| notifies(&run.task)) {
            insert_notification(&mut tx, &run.task, n, now).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn skip_routine_run(
        &self,
        id: &str,
        due_at: DateTime<Utc>,
        next_run_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        // One statement: the guard and the change can't be split by another tick.
        let done = sqlx::query(
            "UPDATE bots_routines SET next_run_at = $3, updated_at = $4
             WHERE id = $1 AND enabled AND next_run_at = $2",
        )
        .bind(id)
        .bind(due_at)
        .bind(next_run_at)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn member_settings(&self, handle: &str) -> StoreResult<Option<MemberSettings>> {
        let row = sqlx::query(
            "SELECT handle, level, updated_at FROM bots_member_settings WHERE handle = $1",
        )
        .bind(handle)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let level: String = r.try_get("level")?;
            Ok(MemberSettings {
                handle: r.try_get("handle")?,
                level: Level::parse(&level).ok_or_else(|| bad("level", &level))?,
                updated_at: r.try_get("updated_at")?,
            })
        })
        .transpose()
    }

    async fn set_member_settings(&self, s: &MemberSettings) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO bots_member_settings (handle, level, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (handle) DO UPDATE SET level = $2, updated_at = $3",
        )
        .bind(&s.handle)
        .bind(s.level.as_str())
        .bind(s.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn held_real_data(&self) -> StoreResult<bool> {
        let row = sqlx::query("SELECT EXISTS (SELECT 1 FROM bots_held_real_data) AS held")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("held")?)
    }

    async fn mark_held_real_data(&self) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO bots_held_real_data (id) VALUES (true) ON CONFLICT (id) DO NOTHING",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
