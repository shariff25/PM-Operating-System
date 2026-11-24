//! [`OpenShellStore`] in Postgres: `policy_proofs` (migration 0030), and sandbox decisions as
//! `audit_events` rows with the sandbox columns, plus `sandbox_collector_runs` (migration 0031).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use super::proofs::{self, ProofRow};
use super::sandbox::{
    self, CollectorState, Fields, Heartbeat, Ingested, Plan, Prepared, RunRow, SandboxQuery,
    SandboxRow, WatchdogReport,
};
use super::OpenShellStore;
use crate::audit::{AuditEvent, Client, Outcome};
use crate::pg::PgStore;
use crate::store::{StoreError, StoreResult};
use pmos_policy::prove::Status;

const SANDBOX_COLUMNS: &str = "sandbox, sandbox_uid, decision, rule, reason, dest_host, dest_ip,
    dest_port, http_method, http_path, process, allow_count, os_event_uid, os_correlation_uid,
    os_version, policy_hash, collector_run, collector_seq";

/// Insert one sandbox row inside `tx`. `None`: a deny or finding already stored (the same
/// `os_event_uid`), so nothing was inserted.
async fn insert_sandbox(tx: &mut sqlx::PgConnection, p: &Prepared) -> StoreResult<Option<i64>> {
    let e = &p.event;
    let f = &p.fields;
    let id: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "INSERT INTO audit_events
             (at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
              output_ref, detail, client, persona, {SANDBOX_COLUMNS})
         VALUES ($1, $2, $3, NULL, $4, $5, $6, '{{}}', NULL, NULL, $7, $8, NULL,
                 $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24,
                 $25, $26)
         ON CONFLICT DO NOTHING
         RETURNING id"
    )))
    .bind(e.at)
    .bind(&e.run_id)
    .bind(&e.user)
    .bind(&e.bot)
    .bind(&e.tool)
    .bind(e.outcome.as_str())
    .bind(&e.detail)
    .bind(e.client.as_str())
    .bind(&f.sandbox)
    .bind(&f.sandbox_uid)
    .bind(&f.decision)
    .bind(&f.rule)
    .bind(&f.reason)
    .bind(&f.dest_host)
    .bind(&f.dest_ip)
    .bind(f.dest_port)
    .bind(&f.http_method)
    .bind(&f.http_path)
    .bind(&f.process)
    .bind(f.allow_count)
    .bind(&f.os_event_uid)
    .bind(&f.os_correlation_uid)
    .bind(&f.os_version)
    .bind(&f.policy_hash)
    .bind(&f.collector_run)
    .bind(f.collector_seq)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(id)
}

fn run_of(r: &PgRow) -> StoreResult<RunRow> {
    let last_seq: i64 = r.try_get("last_seq")?;
    Ok(RunRow {
        run: r.try_get("collector_run")?,
        first_at: r.try_get("first_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        last_seq: u64::try_from(last_seq).unwrap_or(0),
        down_open_since: r.try_get("down_open_since")?,
        superseded: r.try_get("superseded")?,
    })
}

const RUN_COLUMNS: &str =
    "collector_run, first_at, last_seen_at, last_seq, down_open_since, superseded";

async fn save_run(tx: &mut sqlx::PgConnection, r: &RunRow) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO sandbox_collector_runs
             (collector_run, first_at, last_seen_at, last_seq, down_open_since, superseded)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (collector_run) DO UPDATE SET
             last_seen_at = EXCLUDED.last_seen_at, last_seq = EXCLUDED.last_seq,
             down_open_since = EXCLUDED.down_open_since, superseded = EXCLUDED.superseded",
    )
    .bind(&r.run)
    .bind(r.first_at)
    .bind(r.last_seen_at)
    .bind(i64::try_from(r.last_seq).unwrap_or(i64::MAX))
    .bind(r.down_open_since)
    .bind(r.superseded)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn fields_of(r: &PgRow) -> StoreResult<Fields> {
    Ok(Fields {
        sandbox: r.try_get("sandbox")?,
        sandbox_uid: r.try_get("sandbox_uid")?,
        decision: r.try_get("decision")?,
        rule: r.try_get("rule")?,
        reason: r.try_get("reason")?,
        dest_host: r.try_get("dest_host")?,
        dest_ip: r.try_get("dest_ip")?,
        dest_port: r.try_get("dest_port")?,
        http_method: r.try_get("http_method")?,
        http_path: r.try_get("http_path")?,
        process: r.try_get("process")?,
        allow_count: r.try_get("allow_count")?,
        os_event_uid: r.try_get("os_event_uid")?,
        os_correlation_uid: r.try_get("os_correlation_uid")?,
        os_version: r.try_get("os_version")?,
        policy_hash: r.try_get("policy_hash")?,
        collector_run: r.try_get("collector_run")?,
        collector_seq: r.try_get("collector_seq")?,
    })
}

fn event_of(r: &PgRow) -> StoreResult<AuditEvent> {
    let outcome: String = r.try_get("outcome")?;
    Ok(AuditEvent {
        at: r.try_get("at")?,
        client: Client::SandboxAudit,
        run_id: r.try_get("run_id")?,
        user: r.try_get("user_handle")?,
        skill: None,
        bot: r.try_get("bot")?,
        persona: None,
        tool: r.try_get("tool")?,
        outcome: Outcome::parse(&outcome)
            .ok_or_else(|| StoreError(format!("audit_events.outcome `{outcome}`")))?,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: None,
        detail: r.try_get("detail")?,
    })
}

impl PgStore {
    /// Apply a plan in `tx`: the superseded run, the run itself, then every row in order.
    async fn apply_plan(&self, tx: &mut sqlx::PgConnection, plan: Plan) -> StoreResult<Ingested> {
        let mut out = Ingested {
            replayed: plan.replayed,
            gaps: plan.gaps,
            ..Ingested::default()
        };
        if let Some(old) = &plan.supersede {
            sqlx::query(
                "UPDATE sandbox_collector_runs SET superseded = true, down_open_since = NULL
                 WHERE collector_run = $1",
            )
            .bind(old)
            .execute(&mut *tx)
            .await?;
        }
        save_run(tx, &plan.run).await?;
        for p in plan.inserts {
            match insert_sandbox(tx, &p).await? {
                Some(_) => out.stored.push((p.event, p.fields)),
                None => out.duplicates += 1,
            }
        }
        Ok(out)
    }

    /// The run's row (locked) and, for a new run, the newest other run that isn't superseded.
    async fn runs_for(
        &self,
        tx: &mut sqlx::PgConnection,
        run: &str,
    ) -> StoreResult<(Option<RunRow>, Option<RunRow>)> {
        // One ingest at a time: the runs table is small and batches are serial per collector.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pmos.sandbox_audit'), hashtext(current_schema()))")
            .execute(&mut *tx)
            .await?;
        let current = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {RUN_COLUMNS} FROM sandbox_collector_runs WHERE collector_run = $1"
        )))
        .bind(run)
        .fetch_optional(&mut *tx)
        .await?
        .as_ref()
        .map(run_of)
        .transpose()?;
        let previous = if current.is_none() {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {RUN_COLUMNS} FROM sandbox_collector_runs
                 WHERE collector_run <> $1 AND NOT superseded
                 ORDER BY last_seen_at DESC LIMIT 1"
            )))
            .bind(run)
            .fetch_optional(&mut *tx)
            .await?
            .as_ref()
            .map(run_of)
            .transpose()?
        } else {
            None
        };
        Ok((current, previous))
    }
}

/// Insert one audit event inside `tx` (the same columns as `PgStore::record`).
pub(crate) async fn insert_event(
    tx: &mut sqlx::PgConnection,
    event: &AuditEvent,
) -> StoreResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO audit_events
             (at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
              output_ref, detail, client, persona)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         RETURNING id",
    )
    .bind(event.at)
    .bind(&event.run_id)
    .bind(&event.user)
    .bind(&event.skill)
    .bind(&event.bot)
    .bind(&event.tool)
    .bind(event.outcome.as_str())
    .bind(&event.sources)
    .bind(&event.args_hash)
    .bind(&event.output_ref)
    .bind(&event.detail)
    .bind(event.client.as_str())
    .bind(&event.persona)
    .fetch_one(&mut *tx)
    .await?;
    Ok(id)
}

#[async_trait]
impl OpenShellStore for PgStore {
    async fn record_proofs(&self, rows: &[ProofRow]) -> StoreResult<()> {
        let mut tx = self.pool().begin().await?;
        for r in rows {
            sqlx::query(
                "INSERT INTO policy_proofs
                     (at, bot, status, reason_code, reason, coverage, counterexample,
                      prover_version, prover_sha256, boundary_sha256, candidate_sha256, host_user)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            )
            .bind(r.at)
            .bind(&r.bot)
            .bind(r.status.as_str())
            .bind(&r.reason_code)
            .bind(&r.reason)
            .bind(&r.coverage)
            .bind(&r.counterexample)
            .bind(&r.prover_version)
            .bind(&r.prover_sha256)
            .bind(&r.boundary_sha256)
            .bind(&r.candidate_sha256)
            .bind(&r.host_user)
            .execute(&mut *tx)
            .await?;
            insert_event(&mut tx, &proofs::audit_event(r)).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn latest_proofs(&self) -> StoreResult<Vec<ProofRow>> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (bot) at, bot, status, reason_code, reason, coverage,
                    counterexample, prover_version, prover_sha256, boundary_sha256,
                    candidate_sha256, host_user
             FROM policy_proofs
             ORDER BY bot, at DESC, id DESC",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let status: String = r.try_get("status")?;
            out.push(ProofRow {
                at: r.try_get("at")?,
                bot: r.try_get("bot")?,
                status: Status::parse(&status)
                    .ok_or_else(|| StoreError(format!("policy_proofs.status `{status}`")))?,
                reason_code: r.try_get("reason_code")?,
                reason: r.try_get("reason")?,
                coverage: r.try_get("coverage")?,
                counterexample: r.try_get("counterexample")?,
                prover_version: r.try_get("prover_version")?,
                prover_sha256: r.try_get("prover_sha256")?,
                boundary_sha256: r.try_get("boundary_sha256")?,
                candidate_sha256: r.try_get("candidate_sha256")?,
                host_user: r.try_get("host_user")?,
            });
        }
        Ok(out)
    }

    async fn ingest(
        &self,
        run: &str,
        events: Vec<Prepared>,
        now: DateTime<Utc>,
    ) -> StoreResult<Ingested> {
        let mut tx = self.pool().begin().await?;
        let (current, previous) = self.runs_for(&mut tx, run).await?;
        let plan = sandbox::plan(run, current, previous.as_ref(), events, None, now);
        let out = self.apply_plan(&mut tx, plan).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn heartbeat(&self, hb: &Heartbeat, now: DateTime<Utc>) -> StoreResult<Ingested> {
        let mut tx = self.pool().begin().await?;
        let (current, previous) = self.runs_for(&mut tx, &hb.collector_run).await?;
        let plan = sandbox::plan(
            &hb.collector_run,
            current,
            previous.as_ref(),
            Vec::new(),
            Some(hb.acked_seq),
            now,
        );
        let out = self.apply_plan(&mut tx, plan).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn watchdog(&self, now: DateTime<Utc>) -> StoreResult<WatchdogReport> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pmos.sandbox_audit'), hashtext(current_schema()))")
            .execute(&mut *tx)
            .await?;
        let latest = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {RUN_COLUMNS} FROM sandbox_collector_runs
             WHERE NOT superseded ORDER BY last_seen_at DESC LIMIT 1"
        )))
        .fetch_optional(&mut *tx)
        .await?
        .as_ref()
        .map(run_of)
        .transpose()?;
        let Some((event, row)) = sandbox::plan_watchdog(latest.as_ref(), now) else {
            return Ok(WatchdogReport::default());
        };
        save_run(&mut tx, &row).await?;
        insert_sandbox(&mut tx, &event).await?;
        tx.commit().await?;
        Ok(WatchdogReport {
            stored: vec![(event.event, event.fields)],
        })
    }

    async fn sandbox_rows(&self, q: &SandboxQuery) -> StoreResult<Vec<SandboxRow>> {
        let tools: Vec<String> = if q.tools.is_empty() {
            sandbox::TOOLS.iter().map(|s| s.to_string()).collect()
        } else {
            q.tools.clone()
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT id, at, run_id, user_handle, bot, tool, outcome, detail, {SANDBOX_COLUMNS}
             FROM audit_events
             WHERE client = 'sandbox_audit' AND tool = ANY($1)
               AND ($2::text IS NULL OR bot = $2)
               AND ($3::timestamptz IS NULL OR at >= $3)
             ORDER BY at DESC, id DESC
             LIMIT $4"
        )))
        .bind(&tools)
        .bind(&q.bot)
        .bind(q.since)
        .bind(i64::try_from(q.bounded_limit()).unwrap_or(1000))
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SandboxRow {
                    id: r.try_get("id")?,
                    event: event_of(r)?,
                    fields: fields_of(r)?,
                })
            })
            .collect()
    }

    async fn sandbox_fields(&self, ids: &[i64]) -> StoreResult<Vec<(i64, Fields)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT id, {SANDBOX_COLUMNS} FROM audit_events
             WHERE id = ANY($1) AND tool LIKE 'sandbox.%'"
        )))
        .bind(ids)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, fields_of(r)?)))
            .collect()
    }

    async fn collector_state(&self, now: DateTime<Utc>) -> StoreResult<CollectorState> {
        let latest = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {RUN_COLUMNS} FROM sandbox_collector_runs
             WHERE NOT superseded ORDER BY last_seen_at DESC LIMIT 1"
        )))
        .fetch_optional(self.pool())
        .await?
        .as_ref()
        .map(run_of)
        .transpose()?;
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM sandbox_collector_runs")
            .fetch_one(self.pool())
            .await?;
        let last_gap_at: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT max(at) FROM audit_events
             WHERE client = 'sandbox_audit'
               AND tool IN ('sandbox.collector_gap', 'sandbox.collector_down', 'sandbox.source_gap')
               AND bot IS NULL",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(sandbox::state_of(
            latest.as_ref(),
            last_gap_at,
            usize::try_from(runs).unwrap_or(0),
            now,
        ))
    }
}
