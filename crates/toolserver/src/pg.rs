//! Postgres implementations of every store (`store.rs`).
//!
//! Queries are checked at run time, not compile time, so building needs no database. The
//! shared contract tests in `tests/stores.rs` run against a real Postgres in CI.
//!
//! Concurrency rules the schema and queries keep:
//! - A grant is consumed by `DELETE … RETURNING` on one row picked `FOR UPDATE SKIP LOCKED`,
//!   so of two concurrent calls with one grant, exactly one runs.
//! - An approval decision locks its item (`SELECT … FOR UPDATE`) and updates it only while
//!   it is still `pending`, so of two concurrent decisions, exactly one wins. Member requests
//!   are decided with one conditional `UPDATE … WHERE status = 'pending'`, and a partial
//!   unique index keeps one pending request per Slack id.
//! - An approval item and its ledger row are written in one transaction, on create, decide
//!   and complete, so a row is never missing or behind its item.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pmos_contract::Sensitivity;
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::approvals::{
    args_hash, ApprovalItem, ApprovalRepo, DecideError, Decision, NewItem, Status, Verdict,
    GRANT_TTL_MINUTES,
};
use crate::audit::{AuditEvent, AuditSink};
use crate::context::{
    query_terms, snippet, ContextDoc, ContextIndex, Hit, ReindexReport, SearchResults,
};
use crate::ledger::{args_edit_distance, LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use crate::members::{
    Created, Decided, MemberRequest, MemberRequestStore, NewRequest, RequestStatus, MAX_PENDING,
};
use crate::memory::{MemoryResult, MemoryStore, Note};
use crate::memory_repo::{MemoryRepo, MemoryRepoStore};
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
            credentials: s.clone(),
            memory_repos: s.clone(),
            member_requests: s,
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
        ceiling: Sensitivity,
        limit: usize,
    ) -> StoreResult<SearchResults> {
        // Terms are letters and digits only, so the tsquery text can't carry operators.
        let query_terms = query_terms(query);
        if query_terms.is_empty() {
            return Ok(SearchResults::default());
        }
        let tsquery = query_terms.join(" | ");
        let allowed: Vec<String> = [
            Sensitivity::Public,
            Sensitivity::Internal,
            Sensitivity::Restricted,
        ]
        .into_iter()
        .filter(|s| *s <= ceiling)
        .map(|s| s.to_string())
        .collect();
        // Allowed matches only; the count of the others comes from a window over every match.
        let rows = sqlx::query(
            "SELECT * FROM (
                 SELECT path, domain, title, owner, last_reviewed, sensitivity, body,
                        ts_rank_cd(tsv, q) AS score,
                        sensitivity = ANY($4) AS allowed,
                        count(*) FILTER (WHERE NOT (sensitivity = ANY($4))) OVER () AS withheld
                 FROM context_docs, to_tsquery('english', $1) AS q
                 WHERE tsv @@ q AND ($2::text IS NULL OR domain = $2)
             ) matches
             ORDER BY allowed DESC, score DESC, path
             LIMIT $3",
        )
        .bind(&tsquery)
        .bind(domain)
        .bind(limit as i64)
        .bind(&allowed)
        .fetch_all(&self.pool)
        .await?;
        let withheld = rows
            .first()
            .map(|r| r.try_get::<i64, _>("withheld"))
            .transpose()?
            .unwrap_or(0) as usize;
        let hits = rows
            .iter()
            .filter(|row| row.try_get::<bool, _>("allowed").unwrap_or(false))
            .map(|row| {
                let doc = doc_from_row(row)?;
                Ok(Hit {
                    snippet: snippet(&doc.body, &query_terms),
                    score: row.try_get::<f32, _>("score")?,
                    doc,
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(SearchResults { hits, withheld })
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
    async fn read(&self, user: &str) -> MemoryResult<Vec<Note>> {
        let rows = sqlx::query(
            "SELECT key, text, updated_at FROM memory_notes WHERE user_handle = $1 ORDER BY key",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        let notes = rows
            .iter()
            .map(|r| {
                Ok(Note {
                    key: r.try_get("key")?,
                    text: r.try_get("text")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(notes)
    }

    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        _source: &str,
        now: DateTime<Utc>,
    ) -> MemoryResult<Note> {
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
        .await
        .map_err(StoreError::from)?;
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
            "INSERT INTO sessions
                 (token_hash, user_handle, skill, bot, run_id, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(token_hash(&session.token))
        .bind(&session.user)
        .bind(&session.skill)
        .bind(&session.bot)
        .bind(&session.run_id)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, token: &str, now: DateTime<Utc>) -> StoreResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT user_handle, skill, bot, run_id, created_at, expires_at
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
                bot: r.try_get("bot")?,
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
        bot: r.try_get("bot")?,
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

const ITEM_COLUMNS: &str = "id, user_handle, skill, bot, run_id, tool, args, args_hash, status, \
                            created_at, decided_at, decided_by, result";

fn ledger_from_row(r: &PgRow) -> StoreResult<LedgerRow> {
    let outcome: String = r.try_get("outcome")?;
    let reason: Option<String> = r.try_get("reason")?;
    Ok(LedgerRow {
        id: r.try_get("id")?,
        run_id: r.try_get("run_id")?,
        user: r.try_get("user_handle")?,
        bot: r.try_get("bot")?,
        skill: r.try_get("skill")?,
        tool: r.try_get("tool")?,
        content_sha: r.try_get("content_sha")?,
        model_alias: r.try_get("model_alias")?,
        outcome: LedgerOutcome::parse(&outcome).ok_or_else(|| bad_row("outcome", &outcome))?,
        edit_distance: r.try_get("edit_distance")?,
        reason: reason
            .map(|s| Reason::parse(&s).ok_or_else(|| bad_row("reason", &s)))
            .transpose()?,
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
        decided_by: r.try_get("decided_by")?,
    })
}

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
        let row = new.ledger_row(&item);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO approvals
                 (id, user_handle, skill, bot, run_id, tool, args, args_hash, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&item.id)
        .bind(&item.user)
        .bind(&item.skill)
        .bind(&item.bot)
        .bind(&item.run_id)
        .bind(&item.tool)
        .bind(&item.args)
        .bind(&item.args_hash)
        .bind(item.status.as_str())
        .bind(item.created_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO ledger
                 (id, run_id, user_handle, bot, skill, tool, content_sha, model_alias, outcome,
                  created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&row.id)
        .bind(&row.run_id)
        .bind(&row.user)
        .bind(&row.bot)
        .bind(&row.skill)
        .bind(&row.tool)
        .bind(&row.content_sha)
        .bind(&row.model_alias)
        .bind(row.outcome.as_str())
        .bind(row.created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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

    async fn decide_with(
        &self,
        id: &str,
        decided_by: &str,
        verdict: Verdict,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;
        // The lock makes a concurrent decision wait, then see this one's result.
        let sql = format!("SELECT {ITEM_COLUMNS} FROM approvals WHERE id = $1 FOR UPDATE");
        let current = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from)?
            .as_ref()
            .map(item_from_row)
            .transpose()?
            .ok_or(DecideError::NotFound)?;
        // The same checks, in the same order, as the in-memory store.
        if current.user != decided_by {
            return Err(DecideError::Forbidden);
        }
        if current.status != Status::Pending {
            return Err(DecideError::NotPending);
        }
        let reason = verdict
            .reason
            .filter(|_| verdict.decision == Decision::Reject);
        let (status, args, distance) = match verdict.decision {
            Decision::Reject => (Status::Rejected, None, None),
            Decision::Approve => (Status::Executing, None, Some(0.0)),
            Decision::Edit => {
                let args = verdict.args.ok_or(DecideError::MissingArgs)?;
                let distance = args_edit_distance(&current.args, &args);
                (Status::Executing, Some(args), Some(distance))
            }
        };
        let new_hash = args.as_ref().map(args_hash);
        let edited = new_hash.as_ref().is_some_and(|h| h != &current.args_hash);
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
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from)?
            // Held under the lock, so this can't happen; refuse rather than guess.
            .ok_or(DecideError::NotPending)?;
        let item = item_from_row(&row)?;
        sqlx::query(
            "UPDATE ledger
             SET outcome = CASE WHEN $2 THEN 'rejected' ELSE outcome END,
                 edit_distance = $3, reason = $4, edited = $5, decided_at = $6, decided_by = $7
             WHERE id = $1",
        )
        .bind(id)
        .bind(verdict.decision == Decision::Reject)
        .bind(distance)
        .bind(reason.map(Reason::as_str))
        .bind(edited)
        .bind(now)
        .bind(decided_by)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;
        tx.commit().await.map_err(StoreError::from)?;
        Ok(item)
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
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            "UPDATE approvals
             SET result = $2, status = CASE WHEN $3 THEN 'failed' ELSE 'approved' END
             WHERE id = $1 AND status = 'executing'
             RETURNING {ITEM_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(&value)
            .bind(failed)
            .fetch_optional(&mut *tx)
            .await?;
        if row.is_some() {
            sqlx::query(
                "UPDATE ledger
                 SET outcome = CASE WHEN $2 THEN 'failed'
                                    WHEN edited THEN 'edited'
                                    ELSE 'approved' END
                 WHERE id = $1",
            )
            .bind(id)
            .bind(failed)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        row.as_ref().map(item_from_row).transpose()
    }

    async fn stuck(&self, before: DateTime<Utc>) -> StoreResult<Vec<ApprovalItem>> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals
             WHERE status = 'executing' AND decided_at <= $1
             ORDER BY decided_at"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(before)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(item_from_row).collect()
    }

    async fn ledger(&self, query: &LedgerQuery) -> StoreResult<Vec<LedgerRow>> {
        // `outcome` is the reported one, with `expired` worked out; the filters match
        // `LedgerQuery::matches`.
        let outcomes: Vec<&str> = query.outcomes.iter().map(|o| o.as_str()).collect();
        let rows = sqlx::query(
            "SELECT * FROM (
                 SELECT id, run_id, user_handle, bot, skill, tool, content_sha, model_alias,
                        CASE WHEN outcome = 'pending' AND decided_at IS NULL AND created_at < $1
                             THEN 'expired' ELSE outcome END AS outcome,
                        edit_distance, reason, created_at, decided_at, decided_by,
                        COALESCE(decided_at, created_at) AS activity_at
                 FROM ledger
                 WHERE ($2::timestamptz IS NULL OR COALESCE(decided_at, created_at) >= $2)
                   AND ($3::timestamptz IS NULL OR COALESCE(decided_at, created_at) < $3)
                   AND ($4::text IS NULL OR bot = $4)
                   AND ($5::double precision IS NULL OR edit_distance IS NULL
                        OR edit_distance >= $5)
             ) l
             WHERE cardinality($6::text[]) = 0 OR outcome = ANY($6)
             ORDER BY activity_at DESC, id
             LIMIT $7",
        )
        .bind(query.expire_before)
        .bind(query.since)
        .bind(query.until)
        .bind(&query.bot)
        .bind(query.min_edit)
        .bind(&outcomes)
        .bind(query.limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX)))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(ledger_from_row).collect()
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
                 (at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
                  output_ref, detail, client)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(event.at)
        .bind(&event.run_id)
        .bind(&event.user)
        .bind(&event.skill)
        .bind(&event.bot)
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

#[async_trait]
impl MemoryRepoStore for PgStore {
    async fn get(&self, user: &str) -> StoreResult<Option<MemoryRepo>> {
        let row = sqlx::query("SELECT repo, verified_at FROM memory_repos WHERE user_handle = $1")
            .bind(user)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| {
            Ok(MemoryRepo {
                user: user.to_string(),
                repo: r.try_get("repo")?,
                verified_at: r.try_get("verified_at")?,
            })
        })
        .transpose()
    }

    async fn put(&self, record: &MemoryRepo) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO memory_repos (user_handle, repo, verified_at) VALUES ($1, $2, $3)
             ON CONFLICT (user_handle) DO UPDATE SET repo = EXCLUDED.repo,
                 verified_at = EXCLUDED.verified_at",
        )
        .bind(&record.user)
        .bind(&record.repo)
        .bind(record.verified_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, user: &str) -> StoreResult<bool> {
        let done = sqlx::query("DELETE FROM memory_repos WHERE user_handle = $1")
            .bind(user)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

fn request_from_row(r: &PgRow) -> StoreResult<MemberRequest> {
    let status: String = r.try_get("status")?;
    Ok(MemberRequest {
        id: r.try_get("id")?,
        slack_id: r.try_get("slack_id")?,
        email: r.try_get("email")?,
        name: r.try_get("name")?,
        proposed_handle: r.try_get("proposed_handle")?,
        status: RequestStatus::parse(&status).ok_or_else(|| bad_row("status", &status))?,
        requested_at: r.try_get("requested_at")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        handle: r.try_get("handle")?,
        commit: r.try_get("commit_sha")?,
        error: r.try_get("error")?,
    })
}

const REQUEST_COLUMNS: &str = "id, slack_id, email, name, proposed_handle, status, requested_at, \
                               decided_by, decided_at, handle, commit_sha, error";

#[async_trait]
impl MemberRequestStore for PgStore {
    async fn create(&self, new: NewRequest<'_>, now: DateTime<Utc>) -> StoreResult<Created> {
        if let Some(existing) = self.pending_for(new.slack_id).await? {
            return Ok(Created::Existing(existing));
        }
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM member_requests WHERE status = 'pending'")
                .fetch_one(&self.pool)
                .await?;
        if pending as usize >= MAX_PENDING {
            return Ok(Created::TooMany);
        }
        let request = new.into_request(now);
        let inserted = sqlx::query(
            "INSERT INTO member_requests
                 (id, slack_id, email, name, proposed_handle, status, requested_at)
             VALUES ($1, $2, $3, $4, $5, 'pending', $6)
             ON CONFLICT (slack_id) WHERE status = 'pending' DO NOTHING",
        )
        .bind(&request.id)
        .bind(&request.slack_id)
        .bind(&request.email)
        .bind(&request.name)
        .bind(&request.proposed_handle)
        .bind(request.requested_at)
        .execute(&self.pool)
        .await?;
        if inserted.rows_affected() == 1 {
            return Ok(Created::New(request));
        }
        // Another request for the same Slack id won the race.
        match self.pending_for(&request.slack_id).await? {
            Some(existing) => Ok(Created::Existing(existing)),
            None => Err(StoreError(
                "a member request vanished while being created".into(),
            )),
        }
    }

    async fn get(&self, id: &str) -> StoreResult<Option<MemberRequest>> {
        let sql = format!("SELECT {REQUEST_COLUMNS} FROM member_requests WHERE id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(request_from_row).transpose()
    }

    async fn pending_for(&self, slack_id: &str) -> StoreResult<Option<MemberRequest>> {
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM member_requests
             WHERE slack_id = $1 AND status = 'pending'"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(slack_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(request_from_row).transpose()
    }

    async fn list(&self, status: Option<RequestStatus>) -> StoreResult<Vec<MemberRequest>> {
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM member_requests
             WHERE ($1::text IS NULL OR status = $1)
             ORDER BY requested_at, id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(status.map(RequestStatus::as_str))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(request_from_row).collect()
    }

    async fn decide(
        &self,
        id: &str,
        decided: &Decided,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<MemberRequest>> {
        let sql = format!(
            "UPDATE member_requests
             SET status = $2, decided_by = $3, decided_at = $4, handle = $5, commit_sha = $6,
                 error = $7
             WHERE id = $1 AND status = 'pending'
             RETURNING {REQUEST_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(decided.status.as_str())
            .bind(&decided.decided_by)
            .bind(now)
            .bind(&decided.handle)
            .bind(&decided.commit)
            .bind(&decided.error)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(request_from_row).transpose()
    }
}
