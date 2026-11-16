//! The Postgres audit sink: the hash chain, verification and pruning (see `audit_chain`).
//!
//! Every insert runs in one short transaction that locks the single `audit_chain_head` row
//! (`FOR UPDATE`), reads the previous seq and hash, seals the new record, inserts it and
//! moves the head. That lock is the serialisation point: two writers queue, and the chain
//! cannot fork or skip a seq. `lock_timeout` keeps a stuck transaction from blocking the
//! fail-closed writes behind it for long.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::audit_chain::{
    failed_report, Actor, ActorType, AuditEnvelope, Break, BreakKind, Channel, ChainRecord,
    ChainVerifier, Checkpoint, PruneReport, Recorded, RequestContext, Source, Target,
    VerifyReport, GENESIS_HASH,
};
use crate::pg::PgStore;
use crate::store::{StoreError, StoreResult};

const LOCK_TIMEOUT: &str = "SET LOCAL lock_timeout = '5s'";
/// Rows fetched per page while verifying.
const VERIFY_PAGE: i64 = 1_000;

const RECORD_COLUMNS: &str = "seq, schema_version, at, source, actor_type, user_handle, idp_oid, \
    idp_tid, session_id, client, channel, action, target_type, target_id, outcome, reason_code, \
    request_id, source_ip_hash, user_agent_hash, run_id, skill, bot, tool, sources, args_hash, \
    before_hash, after_hash, output_ref, detail, prev_hash, hash";

fn bad_row(column: &str, value: &str) -> StoreError {
    StoreError(format!("audit_events has an unexpected {column}: {value}"))
}

fn record_from_row(r: &PgRow) -> StoreResult<ChainRecord> {
    let get = |c: &str| -> StoreResult<String> {
        r.try_get::<Option<String>, _>(c)?
            .ok_or_else(|| bad_row(c, "NULL"))
    };
    let source = get("source")?;
    let actor_type = get("actor_type")?;
    let client = get("client")?;
    let channel = get("channel")?;
    let outcome = get("outcome")?;
    let target = match (
        r.try_get::<Option<String>, _>("target_type")?,
        r.try_get::<Option<String>, _>("target_id")?,
    ) {
        (Some(kind), Some(id)) => Some(Target { kind, id }),
        _ => None,
    };
    Ok(ChainRecord {
        seq: r.try_get("seq")?,
        schema_version: r.try_get("schema_version")?,
        envelope: AuditEnvelope {
            at: r.try_get("at")?,
            source: Source::parse(&source).ok_or_else(|| bad_row("source", &source))?,
            actor: Actor {
                kind: ActorType::parse(&actor_type)
                    .ok_or_else(|| bad_row("actor_type", &actor_type))?,
                handle: r.try_get("user_handle")?,
                idp_oid: r.try_get("idp_oid")?,
                idp_tid: r.try_get("idp_tid")?,
                session_id: r.try_get("session_id")?,
                client: Client::parse(&client).ok_or_else(|| bad_row("client", &client))?,
            },
            channel: Channel::parse(&channel).ok_or_else(|| bad_row("channel", &channel))?,
            action: get("action")?,
            target,
            outcome: Outcome::parse(&outcome).ok_or_else(|| bad_row("outcome", &outcome))?,
            reason_code: r.try_get("reason_code")?,
            request: RequestContext {
                request_id: r.try_get("request_id")?,
                source_ip_hash: r.try_get("source_ip_hash")?,
                user_agent_hash: r.try_get("user_agent_hash")?,
            },
            run_id: r.try_get("run_id")?,
            skill: r.try_get("skill")?,
            bot: r.try_get("bot")?,
            tool: r.try_get("tool")?,
            sources: r.try_get("sources")?,
            args_hash: r.try_get("args_hash")?,
            before_hash: r.try_get("before_hash")?,
            after_hash: r.try_get("after_hash")?,
            output_ref: r.try_get("output_ref")?,
            detail: r.try_get("detail")?,
        },
        prev_hash: get("prev_hash")?,
        hash: get("hash")?,
    })
}

fn checkpoint_from_row(r: &PgRow) -> StoreResult<Checkpoint> {
    Ok(Checkpoint {
        through_seq: r.try_get("through_seq")?,
        through_hash: r.try_get("through_hash")?,
        rows_pruned: r.try_get("rows_pruned")?,
        cutoff: r.try_get("cutoff")?,
        pruned_at: r.try_get("pruned_at")?,
        prev_hash: r.try_get("prev_hash")?,
        hash: r.try_get("hash")?,
    })
}

impl PgStore {
    async fn append_envelope(&self, envelope: AuditEnvelope) -> StoreResult<Recorded> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(LOCK_TIMEOUT).execute(&mut *tx).await?;
        let head = sqlx::query("SELECT seq, hash FROM audit_chain_head WHERE id = 1 FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
        let prev_seq: i64 = head.try_get("seq")?;
        let prev_hash: String = head.try_get("hash")?;
        let rec = ChainRecord::seal(prev_seq + 1, envelope, &prev_hash);
        let e = &rec.envelope;
        sqlx::query(
            "INSERT INTO audit_events
                 (seq, schema_version, at, source, actor_type, user_handle, idp_oid, idp_tid,
                  session_id, client, channel, action, target_type, target_id, outcome,
                  reason_code, request_id, source_ip_hash, user_agent_hash, run_id, skill, bot,
                  tool, sources, args_hash, before_hash, after_hash, output_ref, detail,
                  prev_hash, hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31)",
        )
        .bind(rec.seq)
        .bind(rec.schema_version)
        .bind(e.at)
        .bind(e.source.as_str())
        .bind(e.actor.kind.as_str())
        .bind(&e.actor.handle)
        .bind(&e.actor.idp_oid)
        .bind(&e.actor.idp_tid)
        .bind(&e.actor.session_id)
        .bind(e.actor.client.as_str())
        .bind(e.channel.as_str())
        .bind(&e.action)
        .bind(e.target.as_ref().map(|t| t.kind.as_str()))
        .bind(e.target.as_ref().map(|t| t.id.as_str()))
        .bind(crate::audit::outcome_str(e.outcome))
        .bind(&e.reason_code)
        .bind(&e.request.request_id)
        .bind(&e.request.source_ip_hash)
        .bind(&e.request.user_agent_hash)
        .bind(&e.run_id)
        .bind(&e.skill)
        .bind(&e.bot)
        .bind(&e.tool)
        .bind(&e.sources)
        .bind(&e.args_hash)
        .bind(&e.before_hash)
        .bind(&e.after_hash)
        .bind(&e.output_ref)
        .bind(&e.detail)
        .bind(&rec.prev_hash)
        .bind(&rec.hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE audit_chain_head SET seq = $1, hash = $2 WHERE id = 1")
            .bind(rec.seq)
            .bind(&rec.hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Recorded {
            seq: rec.seq,
            hash: rec.hash,
        })
    }
}

#[async_trait]
impl AuditSink for PgStore {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        self.append_envelope(AuditEnvelope::from(&event)).await?;
        Ok(())
    }

    fn chained(&self) -> bool {
        true
    }

    async fn record_envelope(&self, envelope: AuditEnvelope) -> StoreResult<Recorded> {
        self.append_envelope(envelope).await
    }

    async fn verify_chain(&self) -> StoreResult<VerifyReport> {
        // One snapshot, so rows written while we walk don't look like a head mismatch.
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let checkpoints = sqlx::query("SELECT * FROM audit_checkpoints ORDER BY through_seq")
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(checkpoint_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        let head = sqlx::query("SELECT seq, hash FROM audit_chain_head WHERE id = 1")
            .fetch_one(&mut *tx)
            .await?;
        let head = (head.try_get::<i64, _>("seq")?, head.try_get::<String, _>("hash")?);

        let mut verifier = match ChainVerifier::new(&checkpoints) {
            Ok(v) => v,
            Err(b) => return Ok(failed_report(0, checkpoints.len(), b)),
        };
        let sql = format!(
            "SELECT {RECORD_COLUMNS} FROM audit_events WHERE seq > $1 ORDER BY seq LIMIT $2"
        );
        let mut after = checkpoints.last().map_or(0, |c| c.through_seq);
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(after)
                .bind(VERIFY_PAGE)
                .fetch_all(&mut *tx)
                .await?;
            for row in &rows {
                let rec = match record_from_row(row) {
                    Ok(rec) => rec,
                    // A row that can't be read back is itself a break.
                    Err(e) => {
                        let seq: i64 = row.try_get("seq")?;
                        let b = Break {
                            seq,
                            kind: BreakKind::HashMismatch,
                            message: e.to_string(),
                        };
                        return Ok(verifier.report(Some(b)));
                    }
                };
                if let Err(b) = verifier.push(&rec) {
                    return Ok(verifier.report(Some(b)));
                }
                after = rec.seq;
            }
            if (rows.len() as i64) < VERIFY_PAGE {
                break;
            }
        }
        Ok(verifier.finish(Some(head)))
    }

    async fn prune_before(&self, cutoff: DateTime<Utc>, batch: usize) -> StoreResult<PruneReport> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(LOCK_TIMEOUT).execute(&mut *tx).await?;
        // Hold the head so no insert interleaves with the prune.
        sqlx::query("SELECT seq FROM audit_chain_head WHERE id = 1 FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
        // The oldest chained rows that are all before the cutoff: stop at the first newer one.
        let rows = sqlx::query(
            "SELECT seq, hash FROM audit_events
             WHERE seq IS NOT NULL
               AND seq < COALESCE((SELECT min(seq) FROM audit_events
                                   WHERE seq IS NOT NULL AND at >= $1), 9223372036854775807)
             ORDER BY seq
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(batch as i64)
        .fetch_all(&mut *tx)
        .await?;
        let Some(last) = rows.last() else {
            return Ok(PruneReport {
                pruned: 0,
                through_seq: None,
                cutoff,
            });
        };
        let through_seq: i64 = last.try_get("seq")?;
        let through_hash: String = last.try_get("hash")?;
        let prev: Option<String> =
            sqlx::query_scalar("SELECT hash FROM audit_checkpoints ORDER BY through_seq DESC LIMIT 1")
                .fetch_optional(&mut *tx)
                .await?;
        let checkpoint = Checkpoint::seal(
            through_seq,
            &through_hash,
            rows.len() as i64,
            cutoff,
            Utc::now(),
            prev.as_deref().unwrap_or(GENESIS_HASH),
        );
        sqlx::query(
            "INSERT INTO audit_checkpoints
                 (through_seq, through_hash, rows_pruned, cutoff, pruned_at, prev_hash, hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(checkpoint.through_seq)
        .bind(&checkpoint.through_hash)
        .bind(checkpoint.rows_pruned)
        .bind(checkpoint.cutoff)
        .bind(checkpoint.pruned_at)
        .bind(&checkpoint.prev_hash)
        .bind(&checkpoint.hash)
        .execute(&mut *tx)
        .await?;
        // The delete trigger allows only rows at or below the checkpoint just written.
        let deleted = sqlx::query("DELETE FROM audit_events WHERE seq IS NOT NULL AND seq <= $1")
            .bind(through_seq)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(PruneReport {
            pruned: deleted,
            through_seq: Some(through_seq),
            cutoff,
        })
    }

    async fn read_chain(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<ChainRecord>> {
        let sql = format!(
            "SELECT {RECORD_COLUMNS} FROM audit_events WHERE seq > $1 ORDER BY seq LIMIT $2"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(after_seq)
            .bind(limit.min(1_000) as i64)
            .fetch_all(self.pool())
            .await?
            .iter()
            .map(record_from_row)
            .collect()
    }
}
