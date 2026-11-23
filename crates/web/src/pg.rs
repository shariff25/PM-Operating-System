//! The Postgres store: sessions, per-PM settings and follow-ups in one pool, with its own
//! migrations table (`_pmos_web_migrations`), so sharing a database with the bot runtime, the
//! tool server and the docs server (each with their own) can't clash.
//!
//! - **Sessions** are stored by the SHA-256 of the id only. Idle and absolute expiry are in
//!   the `UPDATE … WHERE` that touches a session, so an expired one can never be read, even
//!   before a purge; [`SessionStore::purge`] deletes the rest and runs at sign-in.
//! - **Settings:** [`SettingsStore::update`] is one transaction holding `FOR UPDATE` on the PM's
//!   settings row, then writes only what the change altered.
//! - **Follow-ups:** see [`crate::followups`].
//!
//! Every query filters on the handle (or the id hash): there is no read across users.

use std::collections::HashSet;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::{PgConnection, Row};

use crate::followups::{
    expiry, Changed, Day, FollowUp, FollowUpStore, Kind, NewFollowUp, CHANGED_TTL_HOURS,
};
use crate::session::{IdHash, Session, SessionStore, StoreError};
use crate::settings::{
    DraftEdit, PracticeDraft, SettingsStore, StoredSuggestion, Update, UserState,
    EDIT_RETENTION_DAYS,
};

/// How long draft flags and revision-task marks are kept.
const FLAG_RETENTION_DAYS: i64 = 30;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

fn err(e: sqlx::Error) -> StoreError {
    StoreError(e.to_string())
}

fn secs(d: Duration) -> f64 {
    d.num_milliseconds() as f64 / 1000.0
}

impl PgStore {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(StdDuration::from_secs(5))
            .connect(url)
            .await
            .map_err(err)?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name("_pmos_web_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError(e.to_string()))
    }

    /// Has this database ever held real data (slice H4)? Never cleared once written.
    pub async fn held_real_data(&self) -> Result<bool, StoreError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM web_held_real_data)")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StoreError(e.to_string()))
    }

    /// The `held_real_data` rule at start-up (spec §3.6): with live connectors the marker is
    /// written (for good), whatever the source; in fake-data mode a database with it is refused.
    pub async fn check_held_real_data(&self, live: bool, fake_only: bool) -> Result<(), String> {
        if live {
            sqlx::query(
                "INSERT INTO web_held_real_data (id) VALUES (true) ON CONFLICT (id) DO NOTHING",
            )
            .execute(&self.pool)
            .await
            .map_err(|e| format!("writing the held_real_data marker: {e}"))?;
        }
        if fake_only
            && self
                .held_real_data()
                .await
                .map_err(|e| format!("reading the held_real_data marker: {e}"))?
        {
            return Err(pmos_contract::model_source::fake_data_refusal(
                pmos_contract::model_source::HELD_REAL_DATA,
            ));
        }
        Ok(())
    }
}

// Sessions.

#[async_trait]
impl SessionStore for PgStore {
    async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn insert(&self, id: IdHash, session: Session) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO web_sessions
                 (id_hash, handle, csrf_token, created_at, last_seen, entra_oid, entra_tid,
                  password_epoch)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id_hash) DO UPDATE SET handle = EXCLUDED.handle,
                 csrf_token = EXCLUDED.csrf_token, created_at = EXCLUDED.created_at,
                 last_seen = EXCLUDED.last_seen, entra_oid = EXCLUDED.entra_oid,
                 entra_tid = EXCLUDED.entra_tid, password_epoch = EXCLUDED.password_epoch",
        )
        .bind(&id[..])
        .bind(&session.handle)
        .bind(&session.csrf_token)
        .bind(session.created_at)
        .bind(session.last_seen)
        .bind(&session.entra_oid)
        .bind(&session.entra_tid)
        .bind(session.password_epoch)
        .execute(&self.pool)
        .await
        .map_err(err)?;
        Ok(())
    }

    async fn touch(
        &self,
        id: &IdHash,
        now: DateTime<Utc>,
        idle: Duration,
        max: Duration,
    ) -> Result<Option<Session>, StoreError> {
        let row = sqlx::query(
            "UPDATE web_sessions SET last_seen = $2
             WHERE id_hash = $1
               AND last_seen + make_interval(secs => $3) > $2
               AND created_at + make_interval(secs => $4) > $2
             RETURNING handle, csrf_token, created_at, last_seen, entra_oid, entra_tid,
                       password_epoch",
        )
        .bind(&id[..])
        .bind(now)
        .bind(secs(idle))
        .bind(secs(max))
        .fetch_optional(&self.pool)
        .await
        .map_err(err)?;
        match row {
            Some(r) => Ok(Some(Session {
                handle: r.try_get("handle").map_err(err)?,
                csrf_token: r.try_get("csrf_token").map_err(err)?,
                created_at: r.try_get("created_at").map_err(err)?,
                last_seen: r.try_get("last_seen").map_err(err)?,
                entra_oid: r.try_get("entra_oid").map_err(err)?,
                entra_tid: r.try_get("entra_tid").map_err(err)?,
                password_epoch: r.try_get("password_epoch").map_err(err)?,
            })),
            None => {
                // Expired (or never there): make sure nothing is left of it.
                self.remove(id).await?;
                Ok(None)
            }
        }
    }

    async fn remove(&self, id: &IdHash) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM web_sessions WHERE id_hash = $1")
            .bind(&id[..])
            .execute(&self.pool)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn remove_user(&self, handle: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM web_sessions WHERE handle = $1")
            .bind(handle)
            .execute(&self.pool)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn purge(
        &self,
        now: DateTime<Utc>,
        idle: Duration,
        max: Duration,
    ) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "DELETE FROM web_sessions
             WHERE last_seen + make_interval(secs => $2) <= $1
                OR created_at + make_interval(secs => $3) <= $1",
        )
        .bind(now)
        .bind(secs(idle))
        .bind(secs(max))
        .execute(&self.pool)
        .await
        .map_err(err)?
        .rows_affected())
    }
}

// Settings.

async fn load_state(conn: &mut PgConnection, handle: &str) -> Result<UserState, StoreError> {
    let mut st = UserState::default();
    if let Some(r) = sqlx::query(
        "SELECT hints_seen, nudges, mascot, opened_a_draft, ask_persona FROM web_user_settings
         WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err)?
    {
        let hints: Vec<String> = r.try_get("hints_seen").map_err(err)?;
        st.hints_seen = hints.into_iter().collect();
        st.nudges = r.try_get("nudges").map_err(err)?;
        st.mascot = r.try_get("mascot").map_err(err)?;
        st.opened_a_draft = r.try_get("opened_a_draft").map_err(err)?;
        st.ask_persona = r.try_get("ask_persona").map_err(err)?;
    }
    for r in sqlx::query("SELECT draft_id, title, body FROM web_draft_edits WHERE handle = $1")
        .bind(handle)
        .fetch_all(&mut *conn)
        .await
        .map_err(err)?
    {
        st.edits.insert(
            r.try_get("draft_id").map_err(err)?,
            DraftEdit {
                title: r.try_get("title").map_err(err)?,
                body: r.try_get("body").map_err(err)?,
            },
        );
    }
    for r in sqlx::query(
        "SELECT draft_id, task_id, instruction, status, body, error
         FROM web_suggestions WHERE handle = $1",
    )
    .bind(handle)
    .fetch_all(&mut *conn)
    .await
    .map_err(err)?
    {
        st.suggestions.insert(
            r.try_get("draft_id").map_err(err)?,
            StoredSuggestion {
                task_id: r.try_get("task_id").map_err(err)?,
                instruction: r.try_get("instruction").map_err(err)?,
                status: r.try_get("status").map_err(err)?,
                body: r.try_get("body").map_err(err)?,
                error: r.try_get("error").map_err(err)?,
            },
        );
    }
    st.practice = sqlx::query(
        "SELECT id, status, created_at, decided_at FROM web_practice_drafts WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err)?
    .map(|r: PgRow| -> Result<PracticeDraft, StoreError> {
        Ok(PracticeDraft {
            id: r.try_get("id").map_err(err)?,
            status: r.try_get("status").map_err(err)?,
            created_at: r.try_get("created_at").map_err(err)?,
            decided_at: r.try_get("decided_at").map_err(err)?,
        })
    })
    .transpose()?;
    for r in sqlx::query("SELECT draft_id FROM web_draft_flags WHERE handle = $1")
        .bind(handle)
        .fetch_all(&mut *conn)
        .await
        .map_err(err)?
    {
        st.external.insert(r.try_get("draft_id").map_err(err)?);
    }
    for r in sqlx::query("SELECT task_id FROM web_revision_tasks WHERE handle = $1")
        .bind(handle)
        .fetch_all(&mut *conn)
        .await
        .map_err(err)?
    {
        st.revision_tasks.insert(r.try_get("task_id").map_err(err)?);
    }
    Ok(st)
}

/// Write what `new` changed from `old`, and nothing else, so an untouched edit keeps its age.
async fn write_state(
    conn: &mut PgConnection,
    handle: &str,
    old: &UserState,
    new: &UserState,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let hints: Vec<String> = new.hints_seen.iter().cloned().collect();
    sqlx::query(
        "UPDATE web_user_settings SET hints_seen = $2, nudges = $3, opened_a_draft = $4,
             mascot = $5, updated_at = $6, ask_persona = $7 WHERE handle = $1",
    )
    .bind(handle)
    .bind(&hints)
    .bind(new.nudges)
    .bind(new.opened_a_draft)
    .bind(new.mascot)
    .bind(now)
    .bind(&new.ask_persona)
    .execute(&mut *conn)
    .await
    .map_err(err)?;

    for draft_id in old.edits.keys().filter(|k| !new.edits.contains_key(*k)) {
        sqlx::query("DELETE FROM web_draft_edits WHERE handle = $1 AND draft_id = $2")
            .bind(handle)
            .bind(draft_id)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    for (draft_id, e) in &new.edits {
        if old.edits.get(draft_id) == Some(e) {
            continue;
        }
        sqlx::query(
            "INSERT INTO web_draft_edits (handle, draft_id, title, body, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (handle, draft_id) DO UPDATE SET title = EXCLUDED.title,
                 body = EXCLUDED.body, updated_at = EXCLUDED.updated_at",
        )
        .bind(handle)
        .bind(draft_id)
        .bind(&e.title)
        .bind(&e.body)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }
    for draft_id in old
        .suggestions
        .keys()
        .filter(|k| !new.suggestions.contains_key(*k))
    {
        sqlx::query("DELETE FROM web_suggestions WHERE handle = $1 AND draft_id = $2")
            .bind(handle)
            .bind(draft_id)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    for (draft_id, s) in &new.suggestions {
        if old.suggestions.get(draft_id) == Some(s) {
            continue;
        }
        sqlx::query(
            "INSERT INTO web_suggestions
                 (handle, draft_id, task_id, instruction, status, body, error, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (handle, draft_id) DO UPDATE SET task_id = EXCLUDED.task_id,
                 instruction = EXCLUDED.instruction, status = EXCLUDED.status,
                 body = EXCLUDED.body, error = EXCLUDED.error,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(handle)
        .bind(draft_id)
        .bind(&s.task_id)
        .bind(&s.instruction)
        .bind(&s.status)
        .bind(&s.body)
        .bind(&s.error)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }

    if old.practice != new.practice {
        match &new.practice {
            None => {
                sqlx::query("DELETE FROM web_practice_drafts WHERE handle = $1")
                    .bind(handle)
                    .execute(&mut *conn)
                    .await
                    .map_err(err)?;
            }
            Some(p) => {
                sqlx::query(
                    "INSERT INTO web_practice_drafts (handle, id, status, created_at, decided_at)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (handle) DO UPDATE SET id = EXCLUDED.id,
                         status = EXCLUDED.status, created_at = EXCLUDED.created_at,
                         decided_at = EXCLUDED.decided_at",
                )
                .bind(handle)
                .bind(&p.id)
                .bind(&p.status)
                .bind(&p.created_at)
                .bind(&p.decided_at)
                .execute(&mut *conn)
                .await
                .map_err(err)?;
            }
        }
    }

    let removed: Vec<String> = old.external.difference(&new.external).cloned().collect();
    if !removed.is_empty() {
        sqlx::query("DELETE FROM web_draft_flags WHERE handle = $1 AND draft_id = ANY($2)")
            .bind(handle)
            .bind(&removed)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    for id in new.external.difference(&old.external) {
        sqlx::query(
            "INSERT INTO web_draft_flags (handle, draft_id, created_at) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(handle)
        .bind(id)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }
    let removed: Vec<String> = old
        .revision_tasks
        .difference(&new.revision_tasks)
        .cloned()
        .collect();
    if !removed.is_empty() {
        sqlx::query("DELETE FROM web_revision_tasks WHERE handle = $1 AND task_id = ANY($2)")
            .bind(handle)
            .bind(&removed)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    for id in new.revision_tasks.difference(&old.revision_tasks) {
        sqlx::query(
            "INSERT INTO web_revision_tasks (handle, task_id, created_at) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(handle)
        .bind(id)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }
    Ok(())
}

#[async_trait]
impl SettingsStore for PgStore {
    async fn get(&self, handle: &str) -> Result<UserState, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(err)?;
        load_state(&mut conn, handle).await
    }

    async fn update<'a>(
        &'a self,
        handle: &'a str,
        change: Update<'a>,
    ) -> Result<UserState, StoreError> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query(
            "INSERT INTO web_user_settings (handle, updated_at) VALUES ($1, $2)
             ON CONFLICT (handle) DO NOTHING",
        )
        .bind(handle)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        // The lock that makes two tabs' changes take turns.
        sqlx::query("SELECT handle FROM web_user_settings WHERE handle = $1 FOR UPDATE")
            .bind(handle)
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
        let old = load_state(&mut tx, handle).await?;
        let mut new = old.clone();
        change(&mut new);
        write_state(&mut tx, handle, &old, &new, now).await?;
        tx.commit().await.map_err(err)?;
        Ok(new)
    }

    async fn purge(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let stale = now - Duration::days(EDIT_RETENTION_DAYS);
        let flags = now - Duration::days(FLAG_RETENTION_DAYS);
        let mut removed = 0;
        for sql in [
            "DELETE FROM web_draft_edits WHERE updated_at < $1",
            "DELETE FROM web_suggestions WHERE updated_at < $1",
        ] {
            removed += sqlx::query(sql)
                .bind(stale)
                .execute(&self.pool)
                .await
                .map_err(err)?
                .rows_affected();
        }
        for sql in [
            "DELETE FROM web_draft_flags WHERE created_at < $1",
            "DELETE FROM web_revision_tasks WHERE created_at < $1",
        ] {
            removed += sqlx::query(sql)
                .bind(flags)
                .execute(&self.pool)
                .await
                .map_err(err)?
                .rows_affected();
        }
        Ok(removed)
    }
}

// Follow-ups.

fn followup_from_row(r: &PgRow) -> Result<FollowUp, StoreError> {
    let kind: String = r.try_get("kind").map_err(err)?;
    Ok(FollowUp {
        id: r.try_get("id").map_err(err)?,
        kind: Kind::parse(&kind)
            .ok_or_else(|| StoreError(format!("unexpected kind `{kind}` in storage")))?,
        system: r.try_get("system").map_err(err)?,
        who: r.try_get("who").map_err(err)?,
        summary: r.try_get("summary").map_err(err)?,
        url: r.try_get("url").map_err(err)?,
        created: r.try_get("created").map_err(err)?,
        expires: r.try_get("expires").map_err(err)?,
        dismissed: r.try_get("dismissed").map_err(err)?,
    })
}

#[async_trait]
impl FollowUpStore for PgStore {
    async fn list(&self, handle: &str, now: DateTime<Utc>) -> Result<Vec<FollowUp>, StoreError> {
        sqlx::query(
            "SELECT id, kind, system, who, summary, url, created, expires, dismissed
             FROM web_followups
             WHERE handle = $1 AND NOT dismissed AND expires > $2
             ORDER BY created, id",
        )
        .bind(handle)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(err)?
        .iter()
        .map(followup_from_row)
        .collect()
    }

    async fn open(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<FollowUp>, StoreError> {
        sqlx::query(
            "SELECT id, kind, system, who, summary, url, created, expires, dismissed
             FROM web_followups
             WHERE handle = $1 AND id = $2 AND NOT dismissed AND expires > $3",
        )
        .bind(handle)
        .bind(id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(err)?
        .as_ref()
        .map(followup_from_row)
        .transpose()
    }

    async fn dismiss(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "UPDATE web_followups SET dismissed = true
             WHERE handle = $1 AND id = $2 AND expires > $3",
        )
        .bind(handle)
        .bind(id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(err)?
        .rows_affected()
            > 0)
    }

    async fn replace(
        &self,
        handle: &str,
        date: NaiveDate,
        changed: Vec<Changed>,
        followups: Vec<NewFollowUp>,
        persona: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut seen = HashSet::new();
        let items: Vec<(String, NewFollowUp)> = followups
            .into_iter()
            .map(|f| (f.id(handle), f))
            .filter(|(id, _)| seen.insert(id.clone()))
            .collect();
        let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query("DELETE FROM web_followups WHERE handle = $1 AND NOT (id = ANY($2))")
            .bind(handle)
            .bind(&ids)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        for (id, f) in &items {
            // A follow-up already held keeps its `created` and `dismissed`.
            sqlx::query(
                "INSERT INTO web_followups
                     (handle, id, kind, system, who, summary, url, created, expires, dismissed)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, false)
                 ON CONFLICT (handle, id) DO UPDATE SET kind = EXCLUDED.kind,
                     system = EXCLUDED.system, who = EXCLUDED.who,
                     summary = EXCLUDED.summary, url = EXCLUDED.url,
                     expires = EXCLUDED.expires",
            )
            .bind(handle)
            .bind(id)
            .bind(f.kind.as_str())
            .bind(&f.system)
            .bind(&f.who)
            .bind(&f.summary)
            .bind(&f.url)
            .bind(f.created.unwrap_or(now))
            .bind(expiry(&f.system, now))
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        }
        sqlx::query(
            "INSERT INTO web_brief_days (handle, date, changed, updated_at, persona)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (handle) DO UPDATE SET date = EXCLUDED.date,
                 changed = EXCLUDED.changed, updated_at = EXCLUDED.updated_at,
                 persona = EXCLUDED.persona",
        )
        .bind(handle)
        .bind(date)
        .bind(Json(&changed))
        .bind(now)
        .bind(persona)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        tx.commit().await.map_err(err)
    }

    async fn day(&self, handle: &str, now: DateTime<Utc>) -> Result<Option<Day>, StoreError> {
        let Some(r) = sqlx::query(
            "SELECT date, changed, updated_at, persona FROM web_brief_days
             WHERE handle = $1 AND updated_at + make_interval(secs => $3) > $2",
        )
        .bind(handle)
        .bind(now)
        .bind(secs(Duration::hours(CHANGED_TTL_HOURS)))
        .fetch_optional(&self.pool)
        .await
        .map_err(err)?
        else {
            return Ok(None);
        };
        let changed: Json<Vec<Changed>> = r.try_get("changed").map_err(err)?;
        Ok(Some(Day {
            date: r.try_get("date").map_err(err)?,
            changed: changed.0,
            updated: r.try_get("updated_at").map_err(err)?,
            persona: r.try_get("persona").map_err(err)?,
        }))
    }

    async fn delivered(&self, handle: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        let row = sqlx::query("SELECT updated_at FROM web_brief_days WHERE handle = $1")
            .bind(handle)
            .fetch_optional(&self.pool)
            .await
            .map_err(err)?;
        row.map(|r| r.try_get("updated_at").map_err(err))
            .transpose()
    }

    async fn purge(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let a = sqlx::query("DELETE FROM web_followups WHERE expires <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(err)?
            .rows_affected();
        // The day's items go; the row stays with its `updated_at`, which is only when the
        // last brief was delivered (`GET /api/brief`'s `delivered_at`), no text.
        let b = sqlx::query(
            "UPDATE web_brief_days SET changed = '[]'::jsonb
             WHERE updated_at + make_interval(secs => $2) <= $1 AND changed <> '[]'::jsonb",
        )
        .bind(now)
        .bind(secs(Duration::hours(CHANGED_TTL_HOURS)))
        .execute(&self.pool)
        .await
        .map_err(err)?
        .rows_affected();
        Ok(a + b)
    }
}
