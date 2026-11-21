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
//! - An approval item and its ledger row are written in one transaction, on create, decide,
//!   complete and expire, so a row is never missing or behind its item.
//! - A Slack reply is drafted under a transaction-scoped advisory lock on its user and thread,
//!   so of two concurrent drafts for one thread, exactly one is created.
//! - The Slack draft sweep picks items `FOR UPDATE SKIP LOCKED`: one a decision holds is
//!   left to the decision.

use std::collections::BTreeMap;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pmos_contract::Sensitivity;
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::approvals::{
    args_hash, redact_args, reply_message, reply_thread, ApprovalItem, ApprovalRepo, DecideError,
    Decision, Drafted, NewItem, Status, TargetConversation, Verdict, GRANT_TTL_MINUTES, REPLY_TOOL,
};
use crate::audit::{
    check_purge, parse_purge_detail, AuditEvent, AuditQuery, AuditSink, Client as AuditClient,
    LastPurge, Outcome, PurgeBatch, Purged, ReadEvent, RetentionFacts, StoredEvent, PURGE_TOOL,
};
use crate::context::{
    query_terms, snippet, ContextDoc, ContextIndex, Hit, ReindexReport, SearchResults,
};
use crate::ledger::{args_edit_distance, LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use crate::members::{
    Created, Decided, MemberRequest, MemberRequestStore, NewRequest, RequestStatus, MAX_PENDING,
};
use crate::memory::{MemoryResult, MemoryStore, Note, DEFAULT_NOTE_SENSITIVITY};
use crate::memory_repo::{MemoryRepo, MemoryRepoStore};
use crate::reviews::{NewRecommendation, RecVerdict, RecommendError, Recommendation};
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
    /// What the user store plans with (`users/pg.rs`).
    users_rules: crate::users::Rules,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(StdDuration::from_secs(5))
            .connect(url)
            .await?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            users_rules: crate::users::Rules::default(),
        }
    }

    /// The deactivation cap for the web and Slack clients (`users::Rules`); 1 to 10.
    pub fn with_deactivation_cap(mut self, per_hour: u32) -> Self {
        self.users_rules.deactivations_per_hour = per_hour.clamp(1, 10);
        self
    }

    /// What the user store plans with.
    pub fn users_rules(&self) -> crate::users::Rules {
        self.users_rules
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
            member_requests: s.clone(),
            users: s.clone(),
            teams: s,
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
        // Password sign-in's pending tokens and setup codes past their expiry.
        crate::password::store::PasswordStore::prune_passwords(self, now).await?;
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
            "SELECT key, text, updated_at, sensitivity FROM memory_notes
             WHERE user_handle = $1 ORDER BY key",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from)?;
        let notes = rows
            .iter()
            .map(|r| {
                // Null for notes from before notes carried one.
                let sensitivity: Option<String> = r.try_get("sensitivity")?;
                Ok(Note {
                    key: r.try_get("key")?,
                    text: r.try_get("text")?,
                    updated_at: r.try_get("updated_at")?,
                    sensitivity: match sensitivity {
                        None => DEFAULT_NOTE_SENSITIVITY,
                        Some(s) => {
                            Sensitivity::parse(&s).ok_or_else(|| bad_row("sensitivity", &s))?
                        }
                    },
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
        sensitivity: Sensitivity,
        now: DateTime<Utc>,
    ) -> MemoryResult<Note> {
        sqlx::query(
            "INSERT INTO memory_notes (user_handle, key, text, updated_at, sensitivity)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_handle, key) DO UPDATE SET text = EXCLUDED.text,
                 updated_at = EXCLUDED.updated_at, sensitivity = EXCLUDED.sensitivity",
        )
        .bind(user)
        .bind(key)
        .bind(text)
        .bind(now)
        .bind(sensitivity.to_string())
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;
        Ok(Note {
            key: key.to_string(),
            text: text.to_string(),
            updated_at: now,
            sensitivity,
        })
    }
}

#[async_trait]
impl SessionRepo for PgStore {
    async fn insert(&self, session: &Session, _now: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO sessions
                 (token_hash, user_handle, skill, bot, run_id, created_at, expires_at,
                  sources_sensitivity, persona)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(token_hash(&session.token))
        .bind(&session.user)
        .bind(&session.skill)
        .bind(&session.bot)
        .bind(&session.run_id)
        .bind(session.created_at)
        .bind(session.expires_at)
        .bind(session.sources_sensitivity.to_string())
        .bind(&session.persona)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, token: &str, now: DateTime<Utc>) -> StoreResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT user_handle, skill, bot, persona, run_id, created_at, expires_at,
                    sources_sensitivity
             FROM sessions WHERE token_hash = $1 AND expires_at > $2",
        )
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let read: String = r.try_get("sources_sensitivity")?;
            Ok(Session {
                token: token.to_string(),
                user: r.try_get("user_handle")?,
                skill: r.try_get("skill")?,
                bot: r.try_get("bot")?,
                persona: r.try_get("persona")?,
                run_id: r.try_get("run_id")?,
                created_at: r.try_get("created_at")?,
                expires_at: r.try_get("expires_at")?,
                sources_sensitivity: Sensitivity::parse(&read)
                    .ok_or_else(|| bad_row("sensitivity", &read))?,
            })
        })
        .transpose()
    }

    async fn raise_sensitivity(&self, token: &str, read: Sensitivity) -> StoreResult<()> {
        // Raised in one statement, so two concurrent reads can't lower it.
        sqlx::query(
            "UPDATE sessions SET sources_sensitivity = $2
             WHERE token_hash = $1
               AND array_position(ARRAY['public', 'internal', 'restricted'], sources_sensitivity)
                   < array_position(ARRAY['public', 'internal', 'restricted'], $2::text)",
        )
        .bind(token_hash(token))
        .bind(read.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn run_sensitivity(&self, run_id: &str) -> StoreResult<Option<Sensitivity>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT sources_sensitivity FROM sessions WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        let mut highest = None;
        for read in rows {
            let s = Sensitivity::parse(&read).ok_or_else(|| bad_row("sensitivity", &read))?;
            highest = highest.max(Some(s));
        }
        Ok(highest)
    }

    async fn revoke(&self, token: &str) -> StoreResult<bool> {
        let done = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(token))
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn revoke_user(&self, user: &str) -> StoreResult<u64> {
        let mut conn = self.pool.acquire().await?;
        Ok(revoke_user_sessions(&mut conn, user).await?)
    }
}

fn item_from_row(r: &PgRow) -> StoreResult<ApprovalItem> {
    let status: String = r.try_get("status")?;
    let read: Option<String> = r.try_get("sources_sensitivity")?;
    Ok(ApprovalItem {
        id: r.try_get("id")?,
        user: r.try_get("user_handle")?,
        skill: r.try_get("skill")?,
        bot: r.try_get("bot")?,
        persona: r.try_get("persona")?,
        run_id: r.try_get("run_id")?,
        tool: r.try_get("tool")?,
        args: r.try_get("args")?,
        args_hash: r.try_get("args_hash")?,
        revision: i64::from(r.try_get::<i32, _>("revision")?),
        status: Status::parse(&status).ok_or_else(|| bad_row("status", &status))?,
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
        decided_by: r.try_get("decided_by")?,
        result: r.try_get("result")?,
        sources_sensitivity: read
            .map(|s| Sensitivity::parse(&s).ok_or_else(|| bad_row("sensitivity", &s)))
            .transpose()?,
        redacted_at: r.try_get("redacted_at")?,
        drafted_args: r.try_get("drafted_args")?,
        edited_at: r.try_get("edited_at")?,
        // Unreadable (written by a newer server): as if never checked.
        target_conversation: r
            .try_get::<Option<Value>, _>("target_conversation")?
            .and_then(|v| serde_json::from_value::<TargetConversation>(v).ok()),
    })
}

const ITEM_COLUMNS: &str = "id, user_handle, skill, bot, run_id, tool, args, args_hash, status, \
                            created_at, decided_at, decided_by, result, sources_sensitivity, \
                            redacted_at, drafted_args, edited_at, target_conversation, revision, \
                            persona";

const REC_COLUMNS: &str = "id, approval_id, reviewer, verdict, note, note_removed_at, \
                           draft_revision, draft_sha256, created_at, withdrawn_at";

fn rec_from_row(r: &PgRow) -> StoreResult<Recommendation> {
    let verdict: String = r.try_get("verdict")?;
    Ok(Recommendation {
        id: r.try_get("id")?,
        approval_id: r.try_get("approval_id")?,
        reviewer: r.try_get("reviewer")?,
        verdict: RecVerdict::parse(&verdict).ok_or_else(|| bad_row("verdict", &verdict))?,
        note: r.try_get("note")?,
        note_removed_at: r.try_get("note_removed_at")?,
        draft_revision: i64::from(r.try_get::<i32, _>("draft_revision")?),
        draft_sha256: r.try_get("draft_sha256")?,
        created_at: r.try_get("created_at")?,
        withdrawn_at: r.try_get("withdrawn_at")?,
    })
}

/// Remove the notes on `id`'s recommendations, inside `tx`: its Slack text is gone.
async fn remove_notes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    now: DateTime<Utc>,
) -> StoreResult<()> {
    sqlx::query(
        "UPDATE approval_recommendations SET note = NULL, note_removed_at = $2
         WHERE approval_id = $1 AND note IS NOT NULL",
    )
    .bind(id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Insert a new item and its ledger row, inside `tx`.
async fn insert_item(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &ApprovalItem,
    row: &LedgerRow,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO approvals
             (id, user_handle, skill, bot, run_id, tool, args, args_hash, status, created_at,
              sources_sensitivity, persona)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
    .bind(item.sources_sensitivity.map(|s| s.to_string()))
    .bind(&item.persona)
    .execute(&mut **tx)
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
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The tool names as a Postgres array parameter.
fn tool_names(tools: &[&str]) -> Vec<String> {
    tools.iter().map(|t| t.to_string()).collect()
}

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
        insert_item(&mut tx, &item, &row).await?;
        tx.commit().await?;
        Ok(item)
    }

    async fn create_reply(
        &self,
        new: NewItem<'_>,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Drafted> {
        let Some((channel, thread_ts)) = reply_thread(new.args) else {
            return Ok(Drafted::New(ApprovalRepo::create(self, new, now).await?));
        };
        let item = new.into_item(now);
        let row = new.ledger_row(&item);
        let mut tx = self.pool.begin().await?;
        // One drafter per user and thread at a time: a concurrent call waits here, then sees
        // this one's item. The lock ends with the transaction.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("pmos-reply\n{}\n{channel}\n{thread_ts}", new.user))
            .execute(&mut *tx)
            .await?;
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals
             WHERE user_handle = $1 AND tool = $2 AND args ->> 'channel' = $3
               AND args ->> 'thread_ts' = $4 AND created_at >= $5
               AND ($6::text IS NULL OR args ->> 'message_ts' = $6)
               AND status IN ('pending', 'executing', 'approved')
             ORDER BY created_at DESC, id LIMIT 1"
        );
        let existing = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(new.user)
            .bind(REPLY_TOOL)
            .bind(channel)
            .bind(thread_ts)
            .bind(since)
            .bind(reply_message(new.args))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(existing) = existing {
            // Nothing to write: end the transaction now, so the lock goes with it.
            tx.rollback().await?;
            return Ok(Drafted::Existing(item_from_row(&existing)?));
        }
        insert_item(&mut tx, &item, &row).await?;
        tx.commit().await?;
        Ok(Drafted::New(item))
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
             ORDER BY created_at DESC, id COLLATE \"C\"
             LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .bind(status.map(Status::as_str))
            .bind(crate::approvals::LIST_LIMIT as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(item_from_row).collect()
    }

    async fn inbox(
        &self,
        user: &str,
        pending_cap: usize,
        decided_limit: usize,
    ) -> StoreResult<crate::approvals::Inbox> {
        let page = |pending: bool, limit: usize| {
            let sql = format!(
                "SELECT {ITEM_COLUMNS} FROM approvals
                 WHERE user_handle = $1 AND (status = 'pending') = $2
                 ORDER BY created_at DESC, id COLLATE \"C\"
                 LIMIT $3"
            );
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(user)
                .bind(pending)
                .bind(i64::try_from(limit).unwrap_or(i64::MAX))
                .fetch_all(&self.pool)
        };
        let pending = page(true, pending_cap).await?;
        let decided = page(false, decided_limit).await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM approvals WHERE user_handle = $1 AND status = 'pending'",
        )
        .bind(user)
        .fetch_one(&self.pool)
        .await?;
        Ok(crate::approvals::Inbox {
            pending: pending
                .iter()
                .map(item_from_row)
                .collect::<StoreResult<_>>()?,
            total_pending: usize::try_from(total).unwrap_or(0),
            decided: decided
                .iter()
                .map(item_from_row)
                .collect::<StoreResult<_>>()?,
        })
    }

    async fn decide_with(
        &self,
        id: &str,
        decided_by: &str,
        verdict: Verdict,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        // An edit's distance is computed before the transaction, from the item as it is
        // now, so no row lock is held while it runs. A pending item's arguments change only
        // when it is decided, so under the lock they are the same; if they aren't (the hash
        // differs), it is computed again there.
        let early = match (&verdict.decision, &verdict.args) {
            (Decision::Edit, Some(edited)) => {
                // Measured from the bot's own arguments (`drafted_args` once a person saved an
                // edit). Under the lock, the same `args_hash` means the same drafted
                // arguments.
                let row = sqlx::query(
                    "SELECT COALESCE(drafted_args, args) AS args, args_hash FROM approvals \
                     WHERE id = $1",
                )
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(StoreError::from)?;
                match row {
                    Some(r) => {
                        let args: Value = r.try_get("args").map_err(StoreError::from)?;
                        let hash: String = r.try_get("args_hash").map_err(StoreError::from)?;
                        Some((hash, args_edit_distance(&args, edited)))
                    }
                    None => None,
                }
            }
            _ => None,
        };
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
        // The same checks, in the same order, as the in-memory store. A refusal ends the
        // transaction at once: a dropped one keeps its row lock until the connection is next
        // used, and a concurrent decision on this item would wait on it.
        let refused = if current.user != decided_by {
            Some(DecideError::Forbidden)
        } else if current.status == Status::Expired {
            Some(DecideError::Expired)
        } else if current.status != Status::Pending {
            Some(DecideError::NotPending)
        } else if !crate::approvals::still_as_shown(&current, verdict.expected_args_hash.as_deref())
        {
            Some(DecideError::Stale)
        } else if verdict.decision == Decision::Edit && verdict.args.is_none() {
            Some(DecideError::MissingArgs)
        } else {
            None
        };
        if let Some(e) = refused {
            tx.rollback().await.map_err(StoreError::from)?;
            return Err(e);
        }
        let reason = verdict
            .reason
            .filter(|_| verdict.decision == Decision::Reject);
        // As in memory: an edit is measured from the bot's own arguments, so a saved edit
        // approved as it stands counts as edited.
        let (status, args, distance, drafted, edited) = match verdict.decision {
            Decision::Reject => (
                Status::Rejected,
                None,
                None,
                current.drafted_args.clone(),
                false,
            ),
            Decision::Approve => {
                let distance = current
                    .drafted_args
                    .as_ref()
                    .map_or(0.0, |d| args_edit_distance(d, &current.args));
                let edited = current.edited();
                (
                    Status::Executing,
                    None,
                    Some(distance),
                    current.drafted_args.clone(),
                    edited,
                )
            }
            Decision::Edit => {
                // Checked above, before anything changed.
                let args = verdict.args.ok_or(DecideError::MissingArgs)?;
                let distance = match early {
                    Some((hash, distance)) if hash == current.args_hash => distance,
                    _ => args_edit_distance(current.drafted(), &args),
                };
                let drafted = crate::approvals::drafted_after(&current, &args);
                let edited = drafted.is_some();
                (
                    Status::Executing,
                    Some(args),
                    Some(distance),
                    drafted,
                    edited,
                )
            }
        };
        let new_hash = args.as_ref().map(args_hash);
        let sql = format!(
            "UPDATE approvals
             SET status = $3, args = COALESCE($4, args), args_hash = COALESCE($5, args_hash),
                 decided_at = $6, decided_by = $2, drafted_args = $7,
                 revision = revision + CASE WHEN $5::text IS NOT NULL AND $5 <> args_hash
                                            THEN 1 ELSE 0 END
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
            .bind(&drafted)
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

    async fn save_draft(
        &self,
        id: &str,
        user: &str,
        args: Value,
        raise_to: Option<Sensitivity>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;
        // The same lock a decision takes: a save and a decision of one item never interleave.
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
        let refused = if current.user != user {
            Some(DecideError::Forbidden)
        } else if current.status == Status::Expired {
            Some(DecideError::Expired)
        } else if current.status != Status::Pending {
            Some(DecideError::NotPending)
        } else {
            None
        };
        if let Some(e) = refused {
            tx.rollback().await.map_err(StoreError::from)?;
            return Err(e);
        }
        let drafted = crate::approvals::drafted_after(&current, &args);
        let edited_at = drafted.as_ref().map(|_| now);
        // Worked out under the row lock, so a concurrent save can't lower it.
        let level = crate::approvals::raised(current.sources_sensitivity, raise_to);
        let sql = format!(
            "UPDATE approvals
             SET args = $2, args_hash = $3, drafted_args = $4, edited_at = $5,
                 sources_sensitivity = $6,
                 revision = revision + CASE WHEN args_hash <> $3 THEN 1 ELSE 0 END
             WHERE id = $1 AND status = 'pending'
             RETURNING {ITEM_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(&args)
            .bind(args_hash(&args))
            .bind(&drafted)
            .bind(edited_at)
            .bind(level.map(|s| s.to_string()))
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from)?
            .ok_or(DecideError::NotPending)?;
        let item = item_from_row(&row)?;
        tx.commit().await.map_err(StoreError::from)?;
        Ok(item)
    }

    async fn set_target_conversation(
        &self,
        id: &str,
        target: &TargetConversation,
    ) -> StoreResult<bool> {
        let done = sqlx::query("UPDATE approvals SET target_conversation = $2 WHERE id = $1")
            .bind(id)
            .bind(sqlx::types::Json(target))
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
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

    async fn expire(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<ApprovalItem>> {
        let mut tx = self.pool.begin().await?;
        // An item a decision holds is skipped: it is being decided, so it won't expire.
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals
             WHERE status = 'pending' AND tool = ANY($1) AND created_at <= $2
             ORDER BY created_at, id
             FOR UPDATE SKIP LOCKED"
        );
        let due = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tool_names(tools))
            .bind(before)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(item_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        if due.is_empty() {
            tx.rollback().await?;
            return Ok(Vec::new());
        }
        let mut expired = Vec::with_capacity(due.len());
        for item in due {
            let sql = format!(
                "UPDATE approvals
                 SET status = 'expired', args = $2, redacted_at = $3, drafted_args = NULL
                 WHERE id = $1 AND status = 'pending'
                 RETURNING {ITEM_COLUMNS}"
            );
            let row = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&item.id)
                .bind(redact_args(&item.args))
                .bind(now)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query("UPDATE ledger SET outcome = 'expired' WHERE id = $1")
                .bind(&item.id)
                .execute(&mut *tx)
                .await?;
            remove_notes(&mut tx, &item.id, now).await?;
            expired.push(item_from_row(&row)?);
        }
        tx.commit().await?;
        Ok(expired)
    }

    async fn redact_decided(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<usize> {
        let mut tx = self.pool.begin().await?;
        let due = sqlx::query(
            "SELECT id, args FROM approvals
             WHERE redacted_at IS NULL AND tool = ANY($1)
               AND status IN ('approved', 'rejected', 'failed') AND decided_at <= $2
             ORDER BY decided_at, id
             FOR UPDATE SKIP LOCKED",
        )
        .bind(tool_names(tools))
        .bind(before)
        .fetch_all(&mut *tx)
        .await?;
        if due.is_empty() {
            tx.rollback().await?;
            return Ok(0);
        }
        for row in &due {
            let id: String = row.try_get("id")?;
            let args: Value = row.try_get("args")?;
            sqlx::query(
                "UPDATE approvals SET args = $2, redacted_at = $3, drafted_args = NULL \
                 WHERE id = $1",
            )
            .bind(&id)
            .bind(redact_args(&args))
            .bind(now)
            .execute(&mut *tx)
            .await?;
            remove_notes(&mut tx, &id, now).await?;
        }
        tx.commit().await?;
        Ok(due.len())
    }

    async fn close_for_owner(&self, user: &str, now: DateTime<Utc>) -> StoreResult<Vec<String>> {
        let mut tx = self.pool.begin().await?;
        let ids = close_owner_drafts(&mut tx, user, now).await?;
        tx.commit().await?;
        Ok(ids)
    }

    async fn pending_of(
        &self,
        users: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)> {
        if users.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals
             WHERE user_handle = ANY($1) AND status = 'pending'
             ORDER BY created_at DESC, id COLLATE \"C\"
             LIMIT $2"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(users)
            .bind(i64::try_from(cap).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM approvals WHERE user_handle = ANY($1) AND status = 'pending'",
        )
        .bind(users)
        .fetch_one(&self.pool)
        .await?;
        Ok((
            rows.iter().map(item_from_row).collect::<StoreResult<_>>()?,
            usize::try_from(total).unwrap_or(0),
        ))
    }

    async fn recommend(
        &self,
        new: NewRecommendation<'_>,
        now: DateTime<Utc>,
    ) -> Result<Recommendation, RecommendError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from)?;
        // The lock a decision and a saved edit take: none of them interleave on one item.
        let sql = format!("SELECT {ITEM_COLUMNS} FROM approvals WHERE id = $1 FOR UPDATE");
        let current = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(new.approval_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(StoreError::from)?
            .as_ref()
            .map(item_from_row)
            .transpose()?;
        let refused = match &current {
            None => Err(RecommendError::NotFound),
            Some(item) => crate::approvals::recommend_refusal(item, &new),
        };
        if let Err(e) = refused {
            tx.rollback().await.map_err(StoreError::from)?;
            return Err(e);
        }
        sqlx::query(
            "UPDATE approval_recommendations SET withdrawn_at = $3, note = NULL
             WHERE approval_id = $1 AND reviewer = $2 AND withdrawn_at IS NULL",
        )
        .bind(new.approval_id)
        .bind(new.reviewer)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;
        let stored = new.into_stored(now);
        sqlx::query(
            "INSERT INTO approval_recommendations
                 (id, approval_id, reviewer, verdict, note, draft_revision, draft_sha256,
                  created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&stored.id)
        .bind(&stored.approval_id)
        .bind(&stored.reviewer)
        .bind(stored.verdict.as_str())
        .bind(&stored.note)
        .bind(i32::try_from(stored.draft_revision).map_err(|_| bad_row("revision", "too large"))?)
        .bind(&stored.draft_sha256)
        .bind(stored.created_at)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;
        tx.commit().await.map_err(StoreError::from)?;
        Ok(stored)
    }

    async fn persona_counts(
        &self,
        user: Option<&str>,
        since: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, u64>> {
        let rows = sqlx::query(
            "SELECT COALESCE(persona, $3) AS persona, count(*) AS n FROM approvals
             WHERE created_at >= $1 AND ($2::text IS NULL OR user_handle = $2)
             GROUP BY 1",
        )
        .bind(since)
        .bind(user)
        .bind(pmos_contract::personas::ALWAYS_ON)
        .fetch_all(&self.pool)
        .await?;
        let mut out = BTreeMap::new();
        for r in &rows {
            let n: i64 = r.try_get("n")?;
            out.insert(r.try_get("persona")?, u64::try_from(n).unwrap_or(0));
        }
        Ok(out)
    }

    async fn release_hold(&self, id: &str, by: &str, now: DateTime<Utc>) -> StoreResult<bool> {
        let done = sqlx::query(
            "INSERT INTO approval_hold_releases (approval_id, released_by, released_at)
             SELECT id, $2, $3 FROM approvals WHERE id = $1
             ON CONFLICT (approval_id) DO NOTHING",
        )
        .bind(id)
        .bind(by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn hold_released(&self, id: &str) -> StoreResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM approval_hold_releases WHERE approval_id = $1)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn unreleased_pending_outside(
        &self,
        offered: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)> {
        const WHERE: &str = "status = 'pending' AND persona IS NOT NULL AND persona <> ALL($1)
             AND NOT EXISTS (SELECT 1 FROM approval_hold_releases r WHERE r.approval_id = approvals.id)";
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM approvals WHERE {WHERE}
             ORDER BY created_at DESC, id COLLATE \"C\" LIMIT $2"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(offered)
            .bind(i64::try_from(cap).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await?;
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM approvals WHERE {WHERE}"
        )))
        .bind(offered)
        .fetch_one(&self.pool)
        .await?;
        Ok((
            rows.iter().map(item_from_row).collect::<StoreResult<_>>()?,
            usize::try_from(total).unwrap_or(0),
        ))
    }

    async fn recommendations(&self, approval_ids: &[String]) -> StoreResult<Vec<Recommendation>> {
        if approval_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {REC_COLUMNS} FROM approval_recommendations
             WHERE approval_id = ANY($1) AND withdrawn_at IS NULL
             ORDER BY created_at, id COLLATE \"C\""
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(approval_ids)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(rec_from_row).collect()
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
        // Only the database's purge function records as `system`, so the overview's last
        // purge can't be one the application wrote.
        if event.client == AuditClient::System {
            return Err(StoreError(
                "`system` events are recorded by the database's purge function only".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO audit_events
                 (at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
                  output_ref, detail, client, source_titles, persona)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
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
        // Null rather than `{}` when there are none, as for events before the column.
        .bind(
            (!event.source_titles.is_empty())
                .then(|| sqlx::types::Json(event.source_titles.clone())),
        )
        .bind(&event.persona)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn reads(
        &self,
        user: &str,
        run_id: &str,
        until: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Option<Vec<ReadEvent>>> {
        // `audit_events_run` covers `run_id`.
        let rows = sqlx::query(
            "SELECT at, tool, sources, source_titles FROM audit_events
             WHERE run_id = $1 AND user_handle = $2 AND outcome = 'ok'
               AND cardinality(sources) > 0 AND at <= $3
             ORDER BY at DESC, id DESC
             LIMIT $4",
        )
        .bind(run_id)
        .bind(user)
        .bind(until)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(ReadEvent {
                at: r.try_get("at")?,
                tool: r.try_get("tool")?,
                sources: r.try_get("sources")?,
                titles: r
                    .try_get::<Option<sqlx::types::Json<BTreeMap<String, String>>>, _>(
                        "source_titles",
                    )?
                    .map(|j| j.0)
                    .unwrap_or_default(),
            });
        }
        Ok(Some(out))
    }

    async fn search(&self, query: &AuditQuery) -> StoreResult<Option<Vec<StoredEvent>>> {
        // `audit_events_at` covers the order and the cursor.
        let rows = sqlx::query(
            "SELECT id, at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
                    output_ref, detail, client, source_titles, persona
             FROM audit_events
             WHERE ($1::timestamptz IS NULL OR (at, id) < ($1, $2))
               AND ($3::text IS NULL OR user_handle = $3)
               AND ($4::text IS NULL OR tool = $4)
               AND ($5::text IS NULL OR outcome = $5)
               AND ($6::timestamptz IS NULL OR at >= $6)
               AND ($7::timestamptz IS NULL OR at < $7)
               AND ($9::text IS NULL OR persona = $9)
             ORDER BY at DESC, id DESC
             LIMIT $8",
        )
        .bind(query.before.map(|c| c.at))
        .bind(query.before.map(|c| c.id).unwrap_or_default())
        .bind(&query.actor)
        .bind(&query.action)
        .bind(query.outcome.map(Outcome::as_str))
        .bind(query.from)
        .bind(query.to)
        .bind(i64::try_from(query.bounded_limit()).unwrap_or(i64::MAX))
        .bind(&query.persona)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let outcome: String = r.try_get("outcome")?;
            let client: String = r.try_get("client")?;
            out.push(StoredEvent {
                id: r.try_get("id")?,
                event: AuditEvent {
                    at: r.try_get("at")?,
                    client: AuditClient::parse(&client)
                        .ok_or_else(|| bad_row("client", &client))?,
                    run_id: r.try_get("run_id")?,
                    user: r.try_get("user_handle")?,
                    skill: r.try_get("skill")?,
                    bot: r.try_get("bot")?,
                    persona: r.try_get("persona")?,
                    tool: r.try_get("tool")?,
                    outcome: Outcome::parse(&outcome)
                        .ok_or_else(|| bad_row("outcome", &outcome))?,
                    sources: r.try_get("sources")?,
                    source_titles: r
                        .try_get::<Option<sqlx::types::Json<BTreeMap<String, String>>>, _>(
                            "source_titles",
                        )?
                        .map(|j| j.0)
                        .unwrap_or_default(),
                    args_hash: r.try_get("args_hash")?,
                    output_ref: r.try_get("output_ref")?,
                    detail: r.try_get("detail")?,
                },
            });
        }
        Ok(Some(out))
    }

    async fn count_outcomes(
        &self,
        since: DateTime<Utc>,
    ) -> StoreResult<Option<BTreeMap<Outcome, u64>>> {
        let rows = sqlx::query(
            "SELECT outcome, count(*) AS n FROM audit_events WHERE at >= $1 GROUP BY outcome",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let mut counts = BTreeMap::new();
        for r in &rows {
            let outcome: String = r.try_get("outcome")?;
            let n: i64 = r.try_get("n")?;
            if let Some(o) = Outcome::parse(&outcome) {
                counts.insert(o, u64::try_from(n).unwrap_or_default());
            }
        }
        Ok(Some(counts))
    }

    /// Through `purge_audit_events` (migration 0013), the one way the database lets an
    /// audit event be deleted: the floor, the cutoff, the batch's own event and the lock
    /// are all the function's. `now` is unused: the database's clock decides.
    async fn purge(
        &self,
        retention_days: u32,
        batch: u32,
        _now: DateTime<Utc>,
    ) -> StoreResult<Option<Purged>> {
        check_purge(retention_days, batch)?;
        let row = sqlx::query(
            "SELECT deleted, cutoff, more, ran
             FROM purge_audit_events(make_interval(days => $1), $2)",
        )
        .bind(i32::try_from(retention_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(batch).unwrap_or(i32::MAX))
        .fetch_one(&self.pool)
        .await?;
        if !row.try_get::<bool, _>("ran")? {
            return Ok(Some(Purged::Busy));
        }
        Ok(Some(Purged::Ran(PurgeBatch {
            deleted: u64::try_from(row.try_get::<i64, _>("deleted")?).unwrap_or_default(),
            cutoff: row.try_get("cutoff")?,
            more: row.try_get("more")?,
        })))
    }

    async fn retention(&self) -> StoreResult<Option<RetentionFacts>> {
        // `audit_events_at` gives the oldest; `audit_events_purges` the last purge.
        let oldest_event_at: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT min(at) FROM audit_events")
                .fetch_one(&self.pool)
                .await?;
        let last = sqlx::query(
            "SELECT at, detail FROM audit_events
             WHERE tool = $1 AND client = 'system'
             ORDER BY at DESC, id DESC
             LIMIT 1",
        )
        .bind(PURGE_TOOL)
        .fetch_optional(&self.pool)
        .await?;
        let last_purge = match last {
            None => None,
            Some(r) => {
                let detail: Option<String> = r.try_get("detail")?;
                match detail.as_deref().and_then(parse_purge_detail) {
                    Some((deleted, _)) => Some(LastPurge {
                        at: r.try_get("at")?,
                        deleted,
                    }),
                    None => None,
                }
            }
        };
        Ok(Some(RetentionFacts {
            oldest_event_at,
            last_purge,
        }))
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
        suspended_at: r.try_get("suspended_at")?,
    })
}

const CREDENTIAL_COLUMNS: &str = "user_handle, provider, key_id, access_token, refresh_token, \
                                  expires_at, scopes, cloud_id, site_url, external_id, updated_at, \
                                  suspended_at";

/// Suspend `user`'s credentials (deactivation), on `conn`: the user store runs it inside the
/// deactivation's own transaction.
pub(crate) async fn suspend_credentials(
    conn: &mut sqlx::PgConnection,
    user: &str,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE credentials SET suspended_at = $2 WHERE user_handle = $1 AND suspended_at IS NULL",
    )
    .bind(user)
    .bind(at)
    .execute(conn)
    .await?
    .rows_affected())
}

/// Make `user`'s suspended credentials usable again (reactivation within the grace).
pub(crate) async fn restore_credentials(
    conn: &mut sqlx::PgConnection,
    user: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE credentials SET suspended_at = NULL
         WHERE user_handle = $1 AND suspended_at IS NOT NULL",
    )
    .bind(user)
    .execute(conn)
    .await?
    .rows_affected())
}

/// Delete `user`'s suspended credentials (the grace is over).
pub(crate) async fn delete_suspended_credentials(
    conn: &mut sqlx::PgConnection,
    user: &str,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM credentials WHERE user_handle = $1 AND suspended_at IS NOT NULL")
            .bind(user)
            .execute(conn)
            .await?
            .rows_affected(),
    )
}

/// End every MCP session of `user` (deactivation).
pub(crate) async fn revoke_user_sessions(
    conn: &mut sqlx::PgConnection,
    user: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE user_handle = $1")
        .bind(user)
        .execute(conn)
        .await?
        .rows_affected())
}

/// Close `user`'s pending drafts as the system: `rejected`, ledger reason
/// `owner_deactivated`. Returns their ids.
pub(crate) async fn close_owner_drafts(
    conn: &mut sqlx::PgConnection,
    user: &str,
    now: DateTime<Utc>,
) -> Result<Vec<String>, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar(
        "UPDATE approvals SET status = 'rejected', decided_at = $2, decided_by = $3
         WHERE user_handle = $1 AND status = 'pending'
         RETURNING id",
    )
    .bind(user)
    .bind(now)
    .bind(crate::approvals::SYSTEM_DECIDER)
    .fetch_all(&mut *conn)
    .await?;
    if !ids.is_empty() {
        sqlx::query(
            "UPDATE ledger
             SET outcome = 'rejected', reason = 'owner_deactivated', decided_at = $2,
                 decided_by = $3
             WHERE id = ANY($1)",
        )
        .bind(&ids)
        .bind(now)
        .bind(crate::approvals::SYSTEM_DECIDER)
        .execute(&mut *conn)
        .await?;
    }
    Ok(ids)
}

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

    async fn suspend(&self, user: &str, at: DateTime<Utc>) -> StoreResult<u64> {
        let mut conn = self.pool.acquire().await?;
        Ok(suspend_credentials(&mut conn, user, at).await?)
    }

    async fn restore(&self, user: &str) -> StoreResult<u64> {
        let mut conn = self.pool.acquire().await?;
        Ok(restore_credentials(&mut conn, user).await?)
    }

    async fn delete_suspended(&self, user: &str) -> StoreResult<u64> {
        let mut conn = self.pool.acquire().await?;
        Ok(delete_suspended_credentials(&mut conn, user).await?)
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
