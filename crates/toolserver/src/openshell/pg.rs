//! [`OpenShellStore`] in Postgres: `policy_proofs` (migration 0030).

use async_trait::async_trait;
use sqlx::Row;

use super::proofs::{self, ProofRow};
use super::OpenShellStore;
use crate::audit::AuditEvent;
use crate::pg::PgStore;
use crate::store::{StoreError, StoreResult};
use pmos_policy::prove::Status;

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
}
