//! The Postgres store. Every read-modify-write locks the rows it depends on in one
//! transaction: a user's row for the failed-attempt count, and every active admin's row (in
//! id order, so two such transactions can't deadlock) for the last-admin rule.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pmos_contract::model::Role;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::model::{normalize_roles, User, UserStatus};
use crate::password::LockoutPolicy;
use crate::session::Session;
use crate::store::{Failure, SessionStore, StoreError, StoreResult, UserStore};

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
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
        // Its own bookkeeping table, so sharing a database with the other services can't
        // clash.
        migrator.dangerous_set_table_name("_pmos_web_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))
    }
}

const USER_COLUMNS: &str = "id, handle, display_name, roles, status, password_hash, \
     failed_attempts, locked_until, must_change_password, created_at, updated_at, \
     password_changed_at, last_login_at";

fn bad(what: &str, value: &str) -> StoreError {
    StoreError::Backend(format!("unexpected {what} `{value}` in storage"))
}

fn user_from_row(r: &PgRow) -> StoreResult<User> {
    let id: String = r.try_get("id")?;
    let status: String = r.try_get("status")?;
    let role_names: Vec<String> = r.try_get("roles")?;
    let mut roles = role_names
        .iter()
        .map(|n| Role::parse(n).ok_or_else(|| bad("role", n)))
        .collect::<StoreResult<Vec<_>>>()?;
    normalize_roles(&mut roles);
    let failed: i32 = r.try_get("failed_attempts")?;
    Ok(User {
        id: Uuid::parse_str(&id).map_err(|_| bad("user id", &id))?,
        handle: r.try_get("handle")?,
        display_name: r.try_get("display_name")?,
        roles,
        status: UserStatus::parse(&status).ok_or_else(|| bad("status", &status))?,
        password_hash: r.try_get("password_hash")?,
        failed_attempts: u32::try_from(failed).unwrap_or(0),
        locked_until: r.try_get("locked_until")?,
        must_change_password: r.try_get("must_change_password")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        password_changed_at: r.try_get("password_changed_at")?,
        last_login_at: r.try_get("last_login_at")?,
    })
}

fn role_names(roles: &[Role]) -> Vec<String> {
    let mut roles = roles.to_vec();
    normalize_roles(&mut roles);
    roles.iter().map(|r| r.as_str().to_string()).collect()
}

fn session_from_row(r: &PgRow) -> StoreResult<Session> {
    let user_id: String = r.try_get("user_id")?;
    Ok(Session {
        id_hash: r.try_get("id_hash")?,
        user_id: Uuid::parse_str(&user_id).map_err(|_| bad("user id", &user_id))?,
        provider: r.try_get("provider")?,
        created_at: r.try_get("created_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

async fn locked_user(conn: &mut PgConnection, id: Uuid) -> StoreResult<User> {
    let sql = format!("SELECT {USER_COLUMNS} FROM web_users WHERE id = $1 FOR UPDATE");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id.to_string())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(StoreError::NotFound)?;
    user_from_row(&row)
}

/// Lock every active admin's row, in id order, and count those other than `id`.
async fn lock_other_active_admins(conn: &mut PgConnection, id: Uuid) -> StoreResult<usize> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM web_users WHERE status = 'active' AND 'admin' = ANY(roles) \
         ORDER BY id FOR UPDATE",
    )
    .fetch_all(&mut *conn)
    .await?;
    let me = id.to_string();
    Ok(ids.iter().filter(|i| **i != me).count())
}

#[async_trait]
impl UserStore for PgStore {
    async fn create_user(&self, user: &User) -> StoreResult<()> {
        let done = sqlx::query(
            "INSERT INTO web_users (id, handle, display_name, roles, status, password_hash, \
             failed_attempts, locked_until, must_change_password, created_at, updated_at, \
             password_changed_at, last_login_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (handle) DO NOTHING",
        )
        .bind(user.id.to_string())
        .bind(&user.handle)
        .bind(&user.display_name)
        .bind(role_names(&user.roles))
        .bind(user.status.as_str())
        .bind(&user.password_hash)
        .bind(i32::try_from(user.failed_attempts).unwrap_or(i32::MAX))
        .bind(user.locked_until)
        .bind(user.must_change_password)
        .bind(user.created_at)
        .bind(user.updated_at)
        .bind(user.password_changed_at)
        .bind(user.last_login_at)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(StoreError::HandleTaken);
        }
        Ok(())
    }

    async fn user_by_id(&self, id: Uuid) -> StoreResult<Option<User>> {
        let sql = format!("SELECT {USER_COLUMNS} FROM web_users WHERE id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    async fn user_by_handle(&self, handle: &str) -> StoreResult<Option<User>> {
        let sql = format!("SELECT {USER_COLUMNS} FROM web_users WHERE handle = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(handle)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    async fn count_active_admins(&self) -> StoreResult<u64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM web_users WHERE status = 'active' AND 'admin' = ANY(roles)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(n.max(0) as u64)
    }

    async fn record_failure(
        &self,
        id: Uuid,
        at: DateTime<Utc>,
        policy: &LockoutPolicy,
    ) -> StoreResult<Failure> {
        let mut tx = self.pool.begin().await?;
        let user = locked_user(&mut tx, id).await?;
        let failed = user.failed_attempts.saturating_add(1);
        let locked_until = policy.lock_after(failed, at).or(user.locked_until);
        sqlx::query("UPDATE web_users SET failed_attempts = $2, locked_until = $3 WHERE id = $1")
            .bind(id.to_string())
            .bind(i32::try_from(failed).unwrap_or(i32::MAX))
            .bind(locked_until)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Failure {
            failed_attempts: failed,
            locked_until,
        })
    }

    async fn record_success(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<()> {
        let done = sqlx::query(
            "UPDATE web_users SET failed_attempts = 0, locked_until = NULL, last_login_at = $2 \
             WHERE id = $1",
        )
        .bind(id.to_string())
        .bind(at)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn set_password(
        &self,
        id: Uuid,
        hash: &str,
        must_change: bool,
        at: DateTime<Utc>,
    ) -> StoreResult<User> {
        let sql = format!(
            "UPDATE web_users SET password_hash = $2, must_change_password = $3, \
             failed_attempts = 0, locked_until = NULL, password_changed_at = $4, \
             updated_at = $4 WHERE id = $1 RETURNING {USER_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .bind(hash)
            .bind(must_change)
            .bind(at)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        user_from_row(&row)
    }

    async fn upgrade_hash(&self, id: Uuid, hash: &str) -> StoreResult<()> {
        sqlx::query("UPDATE web_users SET password_hash = $2 WHERE id = $1")
            .bind(id.to_string())
            .bind(hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_roles(&self, id: Uuid, roles: &[Role], at: DateTime<Utc>) -> StoreResult<User> {
        let mut tx = self.pool.begin().await?;
        let others = lock_other_active_admins(&mut tx, id).await?;
        let user = locked_user(&mut tx, id).await?;
        if user.is_active_admin() && !roles.contains(&Role::Admin) && others == 0 {
            return Err(StoreError::LastAdmin);
        }
        let sql = format!(
            "UPDATE web_users SET roles = $2, updated_at = $3 WHERE id = $1 \
             RETURNING {USER_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .bind(role_names(roles))
            .bind(at)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        user_from_row(&row)
    }

    async fn set_status(
        &self,
        id: Uuid,
        status: UserStatus,
        at: DateTime<Utc>,
    ) -> StoreResult<User> {
        let mut tx = self.pool.begin().await?;
        let others = lock_other_active_admins(&mut tx, id).await?;
        let user = locked_user(&mut tx, id).await?;
        if user.is_active_admin() && status != UserStatus::Active && others == 0 {
            return Err(StoreError::LastAdmin);
        }
        let sql = format!(
            "UPDATE web_users SET status = $2, updated_at = $3 WHERE id = $1 \
             RETURNING {USER_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .bind(status.as_str())
            .bind(at)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        user_from_row(&row)
    }
}

#[async_trait]
impl SessionStore for PgStore {
    async fn insert_session(&self, s: &Session) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO web_sessions (id_hash, user_id, provider, created_at, last_seen_at, \
             expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&s.id_hash)
        .bind(s.user_id.to_string())
        .bind(&s.provider)
        .bind(s.created_at)
        .bind(s.last_seen_at)
        .bind(s.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn session(&self, id_hash: &str) -> StoreResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT id_hash, user_id, provider, created_at, last_seen_at, expires_at \
             FROM web_sessions WHERE id_hash = $1",
        )
        .bind(id_hash)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(session_from_row).transpose()
    }

    async fn touch_session(&self, id_hash: &str, at: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query(
            "UPDATE web_sessions SET last_seen_at = GREATEST(last_seen_at, $2) WHERE id_hash = $1",
        )
        .bind(id_hash)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_session(&self, id_hash: &str) -> StoreResult<()> {
        sqlx::query("DELETE FROM web_sessions WHERE id_hash = $1")
            .bind(id_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_user_sessions(&self, user_id: Uuid) -> StoreResult<u64> {
        let done = sqlx::query("DELETE FROM web_sessions WHERE user_id = $1")
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    async fn purge_expired(
        &self,
        now: DateTime<Utc>,
        idle_before: DateTime<Utc>,
    ) -> StoreResult<u64> {
        let done =
            sqlx::query("DELETE FROM web_sessions WHERE expires_at <= $1 OR last_seen_at <= $2")
                .bind(now)
                .bind(idle_before)
                .execute(&self.pool)
                .await?;
        Ok(done.rows_affected())
    }
}
