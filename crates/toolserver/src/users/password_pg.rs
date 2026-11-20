//! Password sign-in's Postgres store (migration 0020; `crate::password::store`).
//!
//! A change: one READ COMMITTED transaction; the person's own advisory lock first (one key per
//! person and schema, so two people never wait on each other and the users lock, which every
//! user write takes, is never held here); that person's rows read under it (each statement
//! sees what committed before it, so a change made while this one waited is seen); the plan
//! run; its rows, `role_changes` and audit events written; commit. The table checks (one live
//! setup code per person, a pending token's shape and lifetime, a password never without its
//! second step, never the `evals` account) refuse what a plan should never ask for.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};

use super::pg::{insert_change, insert_event};
use super::{Status, UserRole, Via};
use crate::audit::AuditEvent;
use crate::password::limits::Counter;
use crate::password::store::{
    CodeKind, Credential, PasswordStatus, PasswordStore, Pending, Person, Plan, Purpose,
    RecoveryCode, SetupCode, Snapshot, Writes,
};
use crate::pg::PgStore;
use crate::store::{StoreError, StoreResult};

/// One person's lock (the users lock's shape, another first key).
const LOCK: &str = "SELECT pg_advisory_xact_lock(hashtext('pmos.password'), \
                    hashtext(current_schema() || ':' || $1))";

fn bad(what: &str, value: &str) -> StoreError {
    StoreError(format!(
        "unexpected {what} `{value}` in the password tables"
    ))
}

async fn person(conn: &mut PgConnection, handle: &str) -> StoreResult<Option<Person>> {
    let Some(row) = sqlx::query(
        "SELECT handle, status, sign_in_name, sign_in_name_confirmed_at FROM users WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let status: String = row.try_get("status")?;
    let mut roles = std::collections::BTreeSet::new();
    for r in sqlx::query_scalar::<_, String>("SELECT role FROM user_roles WHERE handle = $1")
        .bind(handle)
        .fetch_all(&mut *conn)
        .await?
    {
        roles.insert(UserRole::parse(&r).ok_or_else(|| bad("role", &r))?);
    }
    let confirmed: Option<DateTime<Utc>> = row.try_get("sign_in_name_confirmed_at")?;
    Ok(Some(Person {
        handle: row.try_get("handle")?,
        status: Status::parse(&status).ok_or_else(|| bad("status", &status))?,
        roles,
        sign_in_name: row.try_get("sign_in_name")?,
        sign_in_name_confirmed: confirmed.is_some(),
    }))
}

fn counter(row: &PgRow, prefix: &str) -> StoreResult<Counter> {
    Ok(Counter {
        count: row.try_get(format!("{prefix}failed_count").as_str())?,
        last_at: row.try_get(format!("{prefix}failed_last_at").as_str())?,
        locked_until: row.try_get(format!("{prefix}locked_until").as_str())?,
    })
}

fn pending_of(row: &PgRow) -> StoreResult<Pending> {
    let purpose: String = row.try_get("purpose")?;
    Ok(Pending {
        token_hash: row.try_get("token_hash")?,
        purpose: Purpose::parse(&purpose).ok_or_else(|| bad("purpose", &purpose))?,
        setup_code_id: row.try_get("setup_code_id")?,
        epoch: row.try_get("epoch")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        attempts: row.try_get("attempts")?,
        staged_hash: row.try_get("staged_hash")?,
        staged_totp: row.try_get("staged_totp")?,
    })
}

async fn load(conn: &mut PgConnection, handle: &str, actor: Option<&str>) -> StoreResult<Snapshot> {
    let credential = match sqlx::query(
        "SELECT password_hash, totp_sealed, totp_last_step, epoch, failed_count, failed_last_at,
                locked_until, setup_failed_count, setup_failed_last_at, setup_locked_until, set_at
         FROM password_credentials WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?
    {
        None => None,
        Some(row) => Some(Credential {
            password_hash: row.try_get("password_hash")?,
            totp_sealed: row.try_get("totp_sealed")?,
            totp_last_step: row.try_get("totp_last_step")?,
            epoch: row.try_get("epoch")?,
            failed: counter(&row, "")?,
            setup_failed: counter(&row, "setup_")?,
            set_at: row.try_get("set_at")?,
        }),
    };
    let mut recovery = Vec::new();
    for row in sqlx::query(
        "SELECT code_hash, used_at FROM password_recovery_codes WHERE handle = $1 ORDER BY code_hash",
    )
    .bind(handle)
    .fetch_all(&mut *conn)
    .await?
    {
        recovery.push(RecoveryCode {
            hash: row.try_get("code_hash")?,
            used_at: row.try_get("used_at")?,
        });
    }
    let setup_code = match sqlx::query(
        "SELECT id, code_hash, kind, issued_at, issued_by, issued_client, expires_at
         FROM password_setup_codes
         WHERE handle = $1 AND used_at IS NULL AND revoked_at IS NULL",
    )
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?
    {
        None => None,
        Some(row) => {
            let kind: String = row.try_get("kind")?;
            let client: String = row.try_get("issued_client")?;
            Some(SetupCode {
                id: row.try_get("id")?,
                hash: row.try_get("code_hash")?,
                kind: CodeKind::parse(&kind).ok_or_else(|| bad("kind", &kind))?,
                issued_at: row.try_get("issued_at")?,
                issued_by: row.try_get("issued_by")?,
                issued_via: Via::parse(&client).ok_or_else(|| bad("client", &client))?,
                expires_at: row.try_get("expires_at")?,
            })
        }
    };
    let mut pending = Vec::new();
    for row in sqlx::query(
        "SELECT token_hash, purpose, setup_code_id, epoch, created_at, expires_at, attempts,
                staged_hash, staged_totp
         FROM password_pending WHERE handle = $1 ORDER BY created_at",
    )
    .bind(handle)
    .fetch_all(&mut *conn)
    .await?
    {
        pending.push(pending_of(&row)?);
    }
    Ok(Snapshot {
        person: person(conn, handle).await?,
        actor: match actor {
            Some(a) => person(conn, a).await?,
            None => None,
        },
        credential,
        recovery,
        setup_code,
        pending,
    })
}

async fn write(
    conn: &mut PgConnection,
    handle: &str,
    w: &Writes,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if let Some(c) = &w.credential {
        sqlx::query(
            "INSERT INTO password_credentials
                 (handle, password_hash, totp_sealed, totp_last_step, epoch, failed_count,
                  failed_last_at, locked_until, setup_failed_count, setup_failed_last_at,
                  setup_locked_until, set_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (handle) DO UPDATE SET
                 password_hash = EXCLUDED.password_hash, totp_sealed = EXCLUDED.totp_sealed,
                 totp_last_step = EXCLUDED.totp_last_step, epoch = EXCLUDED.epoch,
                 failed_count = EXCLUDED.failed_count, failed_last_at = EXCLUDED.failed_last_at,
                 locked_until = EXCLUDED.locked_until,
                 setup_failed_count = EXCLUDED.setup_failed_count,
                 setup_failed_last_at = EXCLUDED.setup_failed_last_at,
                 setup_locked_until = EXCLUDED.setup_locked_until, set_at = EXCLUDED.set_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(handle)
        .bind(&c.password_hash)
        .bind(&c.totp_sealed)
        .bind(c.totp_last_step)
        .bind(c.epoch)
        .bind(c.failed.count)
        .bind(c.failed.last_at)
        .bind(c.failed.locked_until)
        .bind(c.setup_failed.count)
        .bind(c.setup_failed.last_at)
        .bind(c.setup_failed.locked_until)
        .bind(c.set_at)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(hashes) = &w.recovery {
        sqlx::query("DELETE FROM password_recovery_codes WHERE handle = $1")
            .bind(handle)
            .execute(&mut *conn)
            .await?;
        for h in hashes {
            sqlx::query(
                "INSERT INTO password_recovery_codes (handle, code_hash, created_at)
                 VALUES ($1, $2, $3)",
            )
            .bind(handle)
            .bind(h)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        }
    }
    if let Some(used) = &w.use_recovery {
        sqlx::query(
            "UPDATE password_recovery_codes SET used_at = $3
             WHERE handle = $1 AND code_hash = $2 AND used_at IS NULL",
        )
        .bind(handle)
        .bind(used)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(code) = &w.issue_code {
        sqlx::query(
            "UPDATE password_setup_codes SET revoked_at = $2
             WHERE handle = $1 AND used_at IS NULL AND revoked_at IS NULL",
        )
        .bind(handle)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT INTO password_setup_codes
                 (handle, code_hash, kind, issued_at, issued_by, issued_client, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(handle)
        .bind(&code.hash)
        .bind(code.kind.as_str())
        .bind(now)
        .bind(&code.issued_by)
        .bind(code.issued_via.as_str())
        .bind(code.expires_at)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(id) = w.use_code {
        sqlx::query(
            "UPDATE password_setup_codes SET used_at = $3
             WHERE id = $1 AND handle = $2 AND used_at IS NULL AND revoked_at IS NULL",
        )
        .bind(id)
        .bind(handle)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        sqlx::query("DELETE FROM password_pending WHERE setup_code_id = $1")
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    if w.clear_pending {
        sqlx::query("DELETE FROM password_pending WHERE handle = $1")
            .bind(handle)
            .execute(&mut *conn)
            .await?;
    }
    for t in &w.delete_pending {
        sqlx::query("DELETE FROM password_pending WHERE token_hash = $1 AND handle = $2")
            .bind(t)
            .bind(handle)
            .execute(&mut *conn)
            .await?;
    }
    for p in &w.put_pending {
        let done = sqlx::query(
            "INSERT INTO password_pending
                 (token_hash, handle, purpose, setup_code_id, epoch, created_at, expires_at,
                  attempts, staged_hash, staged_totp)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (token_hash) DO UPDATE SET
                 attempts = EXCLUDED.attempts, staged_hash = EXCLUDED.staged_hash,
                 staged_totp = EXCLUDED.staged_totp
             WHERE password_pending.handle = EXCLUDED.handle
               AND password_pending.purpose = EXCLUDED.purpose",
        )
        .bind(&p.token_hash)
        .bind(handle)
        .bind(p.purpose.as_str())
        .bind(p.setup_code_id)
        .bind(p.epoch)
        .bind(p.created_at)
        .bind(p.expires_at)
        .bind(p.attempts)
        .bind(&p.staged_hash)
        .bind(&p.staged_totp)
        .execute(&mut *conn)
        .await?;
        if done.rows_affected() != 1 {
            return Err(sqlx::Error::Protocol(
                "a pending token belongs to someone else".into(),
            ));
        }
    }
    for c in &w.changes {
        insert_change(conn, c).await?;
    }
    for e in &w.events {
        insert_event(conn, e).await?;
    }
    Ok(())
}

const STATUS: &str = "SELECT u.handle,
        c.password_hash IS NOT NULL AS password_set,
        c.totp_sealed IS NOT NULL AS second_step_enrolled,
        coalesce(c.epoch, 0) AS epoch,
        EXISTS (SELECT 1 FROM password_setup_codes s
                WHERE s.handle = u.handle AND s.used_at IS NULL AND s.revoked_at IS NULL
                  AND s.expires_at > $1) AS setup_code_pending,
        (SELECT count(*) FROM password_recovery_codes r
         WHERE r.handle = u.handle AND r.used_at IS NULL) AS recovery_codes_left
     FROM users u LEFT JOIN password_credentials c ON c.handle = u.handle";

fn status_of(row: &PgRow) -> StoreResult<PasswordStatus> {
    Ok(PasswordStatus {
        password_set: row
            .try_get::<Option<bool>, _>("password_set")?
            .unwrap_or(false),
        second_step_enrolled: row
            .try_get::<Option<bool>, _>("second_step_enrolled")?
            .unwrap_or(false),
        setup_code_pending: row.try_get("setup_code_pending")?,
        recovery_codes_left: u32::try_from(row.try_get::<i64, _>("recovery_codes_left")?)
            .unwrap_or(0),
        epoch: row.try_get("epoch")?,
    })
}

impl PgStore {
    /// Everything about `handle`'s password goes (the deactivation grace is over), in the
    /// caller's transaction.
    pub(crate) async fn forget_password(
        conn: &mut PgConnection,
        handle: &str,
    ) -> Result<(), sqlx::Error> {
        for table in [
            "password_pending",
            "password_recovery_codes",
            "password_setup_codes",
            "password_credentials",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE handle = $1"
            )))
            .bind(handle)
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PasswordStore for PgStore {
    async fn handle_by_sign_in_name(&self, name: &str) -> StoreResult<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT handle FROM users WHERE sign_in_name = $1")
                .bind(name)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    async fn handle_by_token(&self, token_hash: &str) -> StoreResult<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT handle FROM password_pending WHERE token_hash = $1")
                .bind(token_hash)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    async fn password_tx(
        &self,
        handle: &str,
        actor: Option<&str>,
        now: DateTime<Utc>,
        plan: Plan<'_>,
    ) -> StoreResult<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(LOCK).bind(handle).execute(&mut *tx).await?;
        let snapshot = load(&mut tx, handle, actor).await?;
        let w = plan(&snapshot);
        write(&mut tx, handle, &w, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn password_events(&self, events: Vec<AuditEvent>) -> StoreResult<()> {
        let mut tx = self.pool().begin().await?;
        for e in &events {
            insert_event(&mut tx, e).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn password_statuses(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, PasswordStatus>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{STATUS} WHERE c.handle IS NOT NULL
                OR EXISTS (SELECT 1 FROM password_setup_codes s WHERE s.handle = u.handle)"
        )))
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        let mut out = BTreeMap::new();
        for row in rows {
            out.insert(row.try_get::<String, _>("handle")?, status_of(&row)?);
        }
        Ok(out)
    }

    async fn password_status(
        &self,
        handle: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<PasswordStatus> {
        match sqlx::query(sqlx::AssertSqlSafe(format!("{STATUS} WHERE u.handle = $2")))
            .bind(now)
            .bind(handle)
            .fetch_optional(self.pool())
            .await?
        {
            Some(row) => status_of(&row),
            None => Ok(PasswordStatus::default()),
        }
    }

    async fn prune_passwords(&self, now: DateTime<Utc>) -> StoreResult<u64> {
        let pending = sqlx::query("DELETE FROM password_pending WHERE expires_at <= $1")
            .bind(now)
            .execute(self.pool())
            .await?
            .rows_affected();
        let codes = sqlx::query("DELETE FROM password_setup_codes WHERE expires_at <= $1")
            .bind(now)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(pending + codes)
    }
}
