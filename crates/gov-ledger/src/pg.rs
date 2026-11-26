//! The Ledger's store in Postgres: Agent Governance's own database, never PM OS's.

use std::collections::BTreeMap;

use async_trait::async_trait;
use gov_core::receipt::ChainHead;
use gov_core::{Digest, Envelope, Id, PublicKey, Scope};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::Row;

use crate::store::{Refusal, Store, StoreError, StoreResult, StoredReceipt};

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Other(e.to_string())
    }
}

fn bad(what: &str) -> StoreError {
    StoreError::Other(format!("an unexpected {what} in storage"))
}

fn seq_in(n: u64) -> StoreResult<i64> {
    i64::try_from(n).map_err(|_| bad("sequence number"))
}

fn seq_out(n: i64) -> StoreResult<u64> {
    u64::try_from(n).map_err(|_| bad("sequence number"))
}

fn id(s: String) -> StoreResult<Id> {
    Id::try_from(s).map_err(|_| bad("identifier"))
}

fn digest(s: String) -> StoreResult<Digest> {
    Digest::try_from(s).map_err(|_| bad("digest"))
}

fn stored(r: &PgRow) -> StoreResult<StoredReceipt> {
    let envelope: Json<Envelope> = r.try_get("envelope")?;
    Ok(StoredReceipt {
        seq: seq_out(r.try_get("seq")?)?,
        hash: digest(r.try_get("hash")?)?,
        at: r.try_get("at")?,
        envelope: envelope.0,
    })
}

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        Ok(PgStore { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        PgStore { pool }
    }

    pub async fn migrate(&self) -> StoreResult<()> {
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name("_gov_ledger_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }
}

#[async_trait]
impl Store for PgStore {
    async fn add_gate_key(&self, scope: &Scope, gate: &Id, key: &PublicKey) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO gate_keys (tenant, env, kid, gate, key) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(key.kid.as_str())
        .bind(gate.as_str())
        .bind(Json(key))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn gate_key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<(Id, PublicKey)>> {
        let row = sqlx::query(
            "SELECT gate, key FROM gate_keys WHERE tenant = $1 AND env = $2 AND kid = $3",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(kid.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let key: Json<PublicKey> = r.try_get("key")?;
            Ok((id(r.try_get("gate")?)?, key.0))
        })
        .transpose()
    }

    async fn gate_keys(&self, scope: &Scope) -> StoreResult<BTreeMap<Id, Vec<PublicKey>>> {
        let rows = sqlx::query(
            "SELECT gate, key FROM gate_keys WHERE tenant = $1 AND env = $2 ORDER BY gate, kid",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_all(&self.pool)
        .await?;
        let mut out: BTreeMap<Id, Vec<PublicKey>> = BTreeMap::new();
        for r in rows {
            let key: Json<PublicKey> = r.try_get("key")?;
            out.entry(id(r.try_get("gate")?)?).or_default().push(key.0);
        }
        Ok(out)
    }

    async fn head(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<ChainHead>> {
        let row = sqlx::query(
            "SELECT seq, hash FROM chain_heads WHERE tenant = $1 AND env = $2 AND gate = $3",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(ChainHead {
                seq: seq_out(r.try_get("seq")?)?,
                hash: digest(r.try_get("hash")?)?,
            })
        })
        .transpose()
    }

    async fn heads(&self, scope: &Scope) -> StoreResult<Vec<(Id, ChainHead)>> {
        let rows = sqlx::query(
            "SELECT gate, seq, hash FROM chain_heads WHERE tenant = $1 AND env = $2 ORDER BY gate",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for r in rows {
            out.push((
                id(r.try_get("gate")?)?,
                ChainHead {
                    seq: seq_out(r.try_get("seq")?)?,
                    hash: digest(r.try_get("hash")?)?,
                },
            ));
        }
        // Postgres orders text by the database's collation; a checkpoint wants plain order.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    async fn append(
        &self,
        scope: &Scope,
        gate: &Id,
        expected_head_seq: u64,
        receipt: &StoredReceipt,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        // Move the head first: the row's own lock decides between two writers.
        let moved = if expected_head_seq == 0 {
            sqlx::query(
                "INSERT INTO chain_heads (tenant, env, gate, seq, hash) VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(scope.tenant.as_str())
            .bind(scope.env.as_str())
            .bind(gate.as_str())
            .bind(seq_in(receipt.seq)?)
            .bind(receipt.hash.as_str())
            .execute(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "UPDATE chain_heads SET seq = $4, hash = $5 \
                 WHERE tenant = $1 AND env = $2 AND gate = $3 AND seq = $6",
            )
            .bind(scope.tenant.as_str())
            .bind(scope.env.as_str())
            .bind(gate.as_str())
            .bind(seq_in(receipt.seq)?)
            .bind(receipt.hash.as_str())
            .bind(seq_in(expected_head_seq)?)
            .execute(&mut *tx)
            .await?
        };
        if moved.rows_affected() != 1 {
            return Err(StoreError::HeadMoved);
        }
        sqlx::query(
            "INSERT INTO receipts (tenant, env, gate, seq, hash, at, envelope) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .bind(seq_in(receipt.seq)?)
        .bind(receipt.hash.as_str())
        .bind(receipt.at)
        .bind(Json(&receipt.envelope))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn receipt(
        &self,
        scope: &Scope,
        gate: &Id,
        seq: u64,
    ) -> StoreResult<Option<StoredReceipt>> {
        let row = sqlx::query(
            "SELECT seq, hash, at, envelope FROM receipts WHERE tenant = $1 AND env = $2 AND gate = $3 AND seq = $4",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .bind(seq_in(seq)?)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(stored).transpose()
    }

    async fn receipts(&self, scope: &Scope, gate: &Id) -> StoreResult<Vec<StoredReceipt>> {
        let rows = sqlx::query(
            "SELECT seq, hash, at, envelope FROM receipts WHERE tenant = $1 AND env = $2 AND gate = $3 ORDER BY seq",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(stored).collect()
    }

    async fn record_refusal(&self, scope: &Scope, refusal: &Refusal) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO refusals (tenant, env, at, kid, code, envelope_hash, envelope) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(refusal.at)
        .bind(refusal.kid.as_str())
        .bind(&refusal.code)
        .bind(refusal.envelope_hash.as_str())
        .bind(refusal.envelope.as_ref().map(Json))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn refusals(&self, scope: &Scope) -> StoreResult<Vec<Refusal>> {
        let rows = sqlx::query(
            "SELECT at, kid, code, envelope_hash, envelope FROM refusals WHERE tenant = $1 AND env = $2 ORDER BY id",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for r in rows {
            let envelope: Option<Json<Envelope>> = r.try_get("envelope")?;
            out.push(Refusal {
                at: r.try_get("at")?,
                kid: id(r.try_get("kid")?)?,
                code: r.try_get("code")?,
                envelope_hash: digest(r.try_get("envelope_hash")?)?,
                envelope: envelope.map(|e| e.0),
            });
        }
        Ok(out)
    }

    async fn add_checkpoint(
        &self,
        scope: &Scope,
        seq: u64,
        at: i64,
        envelope: &Envelope,
    ) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO checkpoints (tenant, env, seq, at, envelope) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(seq_in(seq)?)
        .bind(at)
        .bind(Json(envelope))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn last_checkpoint(&self, scope: &Scope) -> StoreResult<Option<(u64, Envelope)>> {
        let row = sqlx::query(
            "SELECT seq, envelope FROM checkpoints WHERE tenant = $1 AND env = $2 ORDER BY seq DESC LIMIT 1",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let envelope: Json<Envelope> = r.try_get("envelope")?;
            Ok((seq_out(r.try_get("seq")?)?, envelope.0))
        })
        .transpose()
    }

    async fn prunable_through(
        &self,
        scope: &Scope,
        gate: &Id,
        cutoff: i64,
        max_seq: u64,
    ) -> StoreResult<Option<(u64, Digest)>> {
        // The last receipt before the first one that is too new or past `max_seq`.
        let row = sqlx::query(
            "SELECT seq, hash FROM receipts r WHERE tenant = $1 AND env = $2 AND gate = $3 AND seq <= $5 \
             AND NOT EXISTS (SELECT 1 FROM receipts n WHERE n.tenant = r.tenant AND n.env = r.env \
                             AND n.gate = r.gate AND n.seq <= r.seq AND n.at >= $4) \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .bind(cutoff)
        .bind(seq_in(max_seq)?)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Ok((seq_out(r.try_get("seq")?)?, digest(r.try_get("hash")?)?)))
            .transpose()
    }

    async fn prune(
        &self,
        scope: &Scope,
        gate: &Id,
        through_seq: u64,
        record: &Envelope,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM receipts WHERE tenant = $1 AND env = $2 AND gate = $3 AND seq <= $4",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .bind(seq_in(through_seq)?)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO pruned (tenant, env, gate, through_seq, envelope) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant, env, gate) DO UPDATE SET through_seq = EXCLUDED.through_seq, envelope = EXCLUDED.envelope \
             WHERE pruned.through_seq < EXCLUDED.through_seq",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .bind(seq_in(through_seq)?)
        .bind(Json(record))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn pruned(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<(u64, Envelope)>> {
        let row = sqlx::query(
            "SELECT through_seq, envelope FROM pruned WHERE tenant = $1 AND env = $2 AND gate = $3",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(gate.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let envelope: Json<Envelope> = r.try_get("envelope")?;
            Ok((seq_out(r.try_get("through_seq")?)?, envelope.0))
        })
        .transpose()
    }
}
