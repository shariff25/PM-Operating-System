//! Postgres implementations of every store (`store.rs`).
//!
//! Queries are checked at run time, not compile time, so building needs no database. The
//! shared contract tests in `tests/stores.rs` run against a real Postgres in CI.
//!
//! Concurrency rules the schema and queries keep:
//! - A grant is consumed by `DELETE … RETURNING` on one row picked `FOR UPDATE SKIP LOCKED`,
//!   so of two concurrent calls with one grant, exactly one runs.
//! - A decision is one conditional `UPDATE … WHERE status = 'pending' AND user_handle = …`,
//!   so of two concurrent decisions, exactly one wins.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pmos_contract::Sensitivity;
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::approvals::{
    args_hash, ApprovalItem, ApprovalRepo, DecideError, Decision, NewItem, Status,
    GRANT_TTL_MINUTES,
};
use crate::audit::{AuditEvent, AuditSink};
use crate::context::{snippet, terms, ContextDoc, ContextIndex, Hit, ReindexReport};
use crate::memory::{MemoryStore, Note};
use crate::session::{token_hash, Session, SessionRepo};
use crate::store::{StoreError, StoreResult, Stores};
use crate::vault::{CredentialStore, Provider, SealedCredential};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        StoreError(e.to_string())
    }
}

/// One Postgres pool serving every store.
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(StdDuration::from_secs(5))
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

    /// Apply pending migrations.
    pub async fn migrate(&self) -> StoreResult<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Every store, backed by this database.
    pub fn stores(&self) -> Stores {
        let s = std::sync::Arc::new(self.clone());
        Stores {
            context: s.clone(),
            memory: s.clone(),
            sessions: s.clone(),
            approvals: s.clone(),
            audit: s.clone(),
            credentials: s,
        }
    }

    /// Make the context index match `docs` exactly, in one transaction: upsert every doc and
    /// remove rows for files that no longer exist.
    pub async fn reindex(&self, docs: &[ContextDoc]) -> StoreResult<ReindexReport> {
        let mut tx = self.pool.begin().await?;
        for doc in docs {
            sqlx::query(
                "INSERT INTO context_docs
                     (path, domain, title, owner, last_reviewed, sensitivity, body, indexed_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, now())
                 ON CONFLICT (path) DO UPDATE SET
                     domain = EXCLUDED.domain, title = EXCLUDED.title, owner = EXCLUDED.owner,
                     last_reviewed = EXCLUDED.last_reviewed,
                     sensitivity = EXCLUDED.sensitivity, body = EXCLUDED.body, indexed_at = now()",
            )
            .bind(&doc.path)
            .bind(&doc.domain)
            .bind(&doc.title)
            .bind(&doc.owner)
            .bind(doc.last_reviewed)
            .bind(doc.sensitivity.to_string())
            .bind(&doc.body)
            .execute(&mut *tx)
            .await?;
        }
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        let removed = sqlx::query("DELETE FROM context_docs WHERE NOT (path = ANY($1))")
            .bind(&paths)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(ReindexReport {
            indexed: docs.len(),
            removed,
        })
    }

    /// Delete expired sessions and grants. Safe to run at any time.
    pub async fn prune(&self, now: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM grants WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn bad_row(what: &str, value: &str) -> StoreError {
    StoreError(format!("unexpected {what} `{value}` in database"))
}

fn doc_from_row(row: &PgRow) -> StoreResult<ContextDoc> {
    let sensitivity: String = row.try_get("sensitivity")?;
    Ok(ContextDoc {
        path: row.try_get("path")?,
        domain: row.try_get("domain")?,
        title: row.try_get("title")?,
        owner: row.try_get("owner")?,
        last_reviewed: row.try_get::<NaiveDate, _>("last_reviewed")?,
        sensitivity: Sensitivity::parse(&sensitivity)
            .ok_or_else(|| bad_row("sensitivity", &sensitivity))?,
        body: row.try_get("body")?,
    })
}

const DOC_COLUMNS: &str = "path, domain, title, owner, last_reviewed, sensitivity, body";

#[async_trait]
impl ContextIndex for PgStore {
    async fn search(
        &self,
        query: &str,
        domain: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Hit>> {
        // Terms are letters and digits only, so the tsquery text can't carry operators.
        let query_terms = terms(query);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let tsquery = query_terms.join(" | ");
        let rows = sqlx::query(
            "SELECT path, domain, title, owner, last_reviewed, sensitivity, body,
                    ts_rank_cd(tsv, q) AS score
             FROM context_docs, to_tsquery('english', $1) AS q
             WHERE tsv @@ q AND ($2::text IS NULL OR domain = $2)
             ORDER BY score DESC, path
             LIMIT $3",
        )
        .bind(&tsquery)
        .bind(domain)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let doc = doc_from_row(row)?;
                Ok(Hit {
                    snippet: snippet(&doc.body, &query_terms),
                    score: row.try_get::<f32, _>("score")?,
                    doc,
                })
            })
            .collect()
    }

    async fn get(&self, path: &str) -> StoreResult<Option<ContextDoc>> {
        let sql = format!("SELECT {DOC_COLUMNS} FROM context_docs WHERE path = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(path)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(doc_from_row).transpose()
    }

    async fn reindex(&self, docs: Vec<ContextDoc>) -> StoreResult<ReindexReport> {
        PgStore::reindex(self, &docs).await
    }
}

#[async_trait]
impl MemoryStore for PgStore {
    async fn read(&self, user: &str) -> StoreResult<Vec<Note>> {
        let rows = sqlx::query(
            "SELECT key, text, updated_at FROM memory_notes WHERE user_handle = $1 ORDER BY key",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Note {
                    key: r.try_get("key")?,
                    text: r.try_get("text")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect()
    }

    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<Note> {
        sqlx::query(
            "INSERT INTO memory_notes (user_handle, key, text, updated_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_handle, key) DO UPDATE SET text = EXCLUDED.text,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(user)
        .bind(key)
        .bind(text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Note {
            key: key.to_string(),
            text: text.to_string(),
            updated_at: now,
        })
    }
}

#[async_trait]
impl SessionRepo for PgStore {
    async fn insert(&self, session: &Session, _now: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_handle, skill, run_id, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(token_hash(&session.token))
        .bind(&session.user)
        .bind(&session.skill)
        .bind(&session.run_id)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, token: &str, now: DateTime<Utc>) -> StoreResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT user_handle, skill, run_id, created_at, expires_at
             FROM sessions WHERE token_hash = $1 AND expires_at > $2",
        )
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(Session {
                token: token.to_string(),
                user: r.try_get("user_handle")?,
                skill: r.try_get("skill")?,
                run_id: r.try_get("run_id")?,
                created_at: r.try_get("created_at")?,
                expires_at: r.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    async fn revoke(&self, token: &str) -> StoreResult<bool> {
        let done = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(token))
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

fn item_from_row(r: &PgRow) -> StoreResult<ApprovalItem> {
    let status: String = r.try_get("status")?;
    Ok(ApprovalItem {
        id: r.try_get("id")?,
        user: r.try_get("user_handle")?,
        skill: r.try_get("skill")?,
        run_id: r.try_get("run_id")?,
        tool: r.try_get("tool")?,
        args: r.try_get("args")?,
        args_hash: r.try_get("args_hash")?,
        status: Status::parse(&status).ok_or_else(|| bad_row("status", &status))?,
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
        decided_by: r.try_get("decided_by")?,
        result: r.try_get("result")?,
    })
}

const ITEM_COLUMNS: &str = "id, user_handle, skill, run_id, tool, args, args_hash, status, \
                            created_at, decided_at, decided_by, result";

#[async_trait]
impl ApprovalRepo for PgStore {
    async fn add_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO grants (session_key, tool, args_hash, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(session_key)
        .bind(tool)
        .bind(args_hash(args))
        .bind(now + Duration::minutes(GRANT_TTL_MINUTES))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn take_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let row = sqlx::query(
            "DELETE FROM grants WHERE id = (
                 SELECT id FROM grants
                 WHERE session_key = $1 AND tool = $2 AND args_hash = $3 AND expires_at > $4
                 ORDER BY id LIMIT 1
                 FOR UPDATE SKIP LOCKED
             ) RETURNING id",
        )
        .bind(session_key)
        .bind(tool)
        .bind(args_hash(args))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    async fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> StoreResult<ApprovalItem> {
        let item = new.into_item(now);
        sqlx::query(
            "INSERT INTO approvals
                 (id, user_handle, skill, run_id, tool, args, args_hash, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&item.id)
        .bind(&item.user)
        .bind(&item.skill)
        .bind(&item.run_id)
        .bind(&item.tool)
        .bind(&item.args)
        .bind(&item.args_hash)
        .bind(item.status.as_str())
        .bind(item.created_at)
        .execute(&self.pool)
        .await?;
        Ok(item)
    }

    async fn get(&self, id: &str) -> StoreResult<Option<ApprovalItem>> {
        let sql = format!("SELECT {ITEM_COLUMNS} FROM approvals WHERE id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(item_from_row).transpose()
    }

    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals
             WHERE user_handle = $1 AND ($2::text IS NULL OR status = $2)
             ORDER BY created_at DESC, id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .bind(status.map(Status::as_str))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(item_from_row).collect()
    }

    async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let (status, args) = match decision {
            Decision::Reject => (Status::Rejected, None),
            Decision::Approve => (Status::Approved, None),
            Decision::Edit => (Status::Approved, edited_args),
        };
        let missing_args = decision == Decision::Edit && args.is_none();
        if !missing_args {
            let new_hash = args.as_ref().map(args_hash);
            let sql = format!(
                "UPDATE approvals
                 SET status = $3, args = COALESCE($4, args), args_hash = COALESCE($5, args_hash),
                     decided_at = $6, decided_by = $2
                 WHERE id = $1 AND user_handle = $2 AND status = 'pending'
                 RETURNING {ITEM_COLUMNS}"
            );
            let row = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(id)
                .bind(decided_by)
                .bind(status.as_str())
                .bind(&args)
                .bind(&new_hash)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::from)?;
            if let Some(row) = row {
                return Ok(item_from_row(&row)?);
            }
        }
        // Nothing changed: work out why, in the same order the in-memory store checks.
        let current = ApprovalRepo::get(self, id)
            .await?
            .ok_or(DecideError::NotFound)?;
        if current.user != decided_by {
            Err(DecideError::Forbidden)
        } else if current.status != Status::Pending {
            Err(DecideError::NotPending)
        } else if missing_args {
            Err(DecideError::MissingArgs)
        } else {
            // Pending, owned, and the update still matched nothing: it was decided between the
            // two queries.
            Err(DecideError::NotPending)
        }
    }

    async fn complete(
        &self,
        id: &str,
        result: Result<Value, String>,
    ) -> StoreResult<Option<ApprovalItem>> {
        let (failed, value) = match result {
            Ok(value) => (false, value),
            Err(message) => (true, serde_json::json!({ "error": message })),
        };
        let sql = format!(
            "UPDATE approvals
             SET result = $2, status = CASE WHEN $3 THEN 'failed' ELSE status END
             WHERE id = $1
             RETURNING {ITEM_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(&value)
            .bind(failed)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(item_from_row).transpose()
    }
}

fn outcome_str(outcome: crate::audit::Outcome) -> String {
    // The serde form, so the database and the JSON-lines log say the same thing.
    serde_json::to_value(outcome)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[async_trait]
impl AuditSink for PgStore {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO audit_events
                 (at, run_id, user_handle, skill, tool, outcome, sources, args_hash, output_ref,
                  detail, client)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(event.at)
        .bind(&event.run_id)
        .bind(&event.user)
        .bind(&event.skill)
        .bind(&event.tool)
        .bind(outcome_str(event.outcome))
        .bind(&event.sources)
        .bind(&event.args_hash)
        .bind(&event.output_ref)
        .bind(&event.detail)
        .bind(event.client.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn sealed_from_row(r: &PgRow) -> StoreResult<SealedCredential> {
    let provider: String = r.try_get("provider")?;
    Ok(SealedCredential {
        user: r.try_get("user_handle")?,
        provider: Provider::parse(&provider).ok_or_else(|| bad_row("provider", &provider))?,
        key_id: r.try_get("key_id")?,
        access_token: r.try_get("access_token")?,
        refresh_token: r.try_get("refresh_token")?,
        expires_at: r.try_get("expires_at")?,
        scopes: r.try_get("scopes")?,
        cloud_id: r.try_get("cloud_id")?,
        site_url: r.try_get("site_url")?,
        external_id: r.try_get("external_id")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const CREDENTIAL_COLUMNS: &str = "user_handle, provider, key_id, access_token, refresh_token, \
                                  expires_at, scopes, cloud_id, site_url, external_id, updated_at";

#[async_trait]
impl CredentialStore for PgStore {
    async fn get(&self, user: &str, provider: Provider) -> StoreResult<Option<SealedCredential>> {
        let sql = format!(
            "SELECT {CREDENTIAL_COLUMNS} FROM credentials WHERE user_handle = $1 AND provider = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .bind(provider.as_str())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(sealed_from_row).transpose()
    }

    async fn put(&self, c: &SealedCredential) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO credentials
                 (user_handle, provider, key_id, access_token, refresh_token, expires_at, scopes,
                  cloud_id, site_url, external_id, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (user_handle, provider) DO UPDATE SET
                 key_id = EXCLUDED.key_id, access_token = EXCLUDED.access_token,
                 refresh_token = EXCLUDED.refresh_token, expires_at = EXCLUDED.expires_at,
                 scopes = EXCLUDED.scopes, cloud_id = EXCLUDED.cloud_id,
                 site_url = EXCLUDED.site_url, external_id = EXCLUDED.external_id,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&c.user)
        .bind(c.provider.as_str())
        .bind(&c.key_id)
        .bind(&c.access_token)
        .bind(&c.refresh_token)
        .bind(c.expires_at)
        .bind(&c.scopes)
        .bind(&c.cloud_id)
        .bind(&c.site_url)
        .bind(&c.external_id)
        .bind(c.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn replace(&self, c: &SealedCredential, expected: DateTime<Utc>) -> StoreResult<bool> {
        let done = sqlx::query(
            "UPDATE credentials SET
                 key_id = $3, access_token = $4, refresh_token = $5, expires_at = $6,
                 scopes = $7, cloud_id = $8, site_url = $9, external_id = $10, updated_at = $11
             WHERE user_handle = $1 AND provider = $2 AND updated_at = $12",
        )
        .bind(&c.user)
        .bind(c.provider.as_str())
        .bind(&c.key_id)
        .bind(&c.access_token)
        .bind(&c.refresh_token)
        .bind(c.expires_at)
        .bind(&c.scopes)
        .bind(&c.cloud_id)
        .bind(&c.site_url)
        .bind(&c.external_id)
        .bind(c.updated_at)
        .bind(expected)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn delete(&self, user: &str, provider: Provider) -> StoreResult<bool> {
        let done = sqlx::query("DELETE FROM credentials WHERE user_handle = $1 AND provider = $2")
            .bind(user)
            .bind(provider.as_str())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn list(&self, user: &str) -> StoreResult<Vec<SealedCredential>> {
        let sql = format!(
            "SELECT {CREDENTIAL_COLUMNS} FROM credentials WHERE user_handle = $1 ORDER BY provider"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(sealed_from_row).collect()
    }
}
