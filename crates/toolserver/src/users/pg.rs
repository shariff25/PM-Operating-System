//! The Postgres [`UserStore`] (migration 0015).
//!
//! A write: one READ COMMITTED transaction; the users advisory lock first (the same key the
//! `users_invariants()` trigger takes, so the trigger's own lock is then re-entrant); the
//! directory read under it; the plan checked; the rows, their `role_changes` and their audit
//! events written; commit, where the trigger checks every invariant again on a fresh snapshot.
//! A refusal, by the plan or by the trigger, rolls everything back and records a `denied`
//! audit event on its own.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};

use super::link::{self, EntraLink, LinkError, LinkRefusal, LinkResult, Linked, Planned};
use super::rules::{self, denied_event, Effect, Rules, Write};
use super::{
    audit_event, Actor, Applied, ChangeKind, Directory, Identity, IdentityKind, Op, PilotException,
    Refusal, RoleChange, RoleGrant, Status, User, UserError, UserResult, UserRole, UserStore, Via,
};
use crate::audit::AuditEvent;
use crate::pg::PgStore;
use crate::store::{StoreError, StoreResult};

/// The users lock, as the trigger takes it.
const LOCK: &str =
    "SELECT pg_advisory_xact_lock(hashtext('pmos.users'), hashtext(current_schema()))";

/// The retention function deletes at most this many rows per call.
const PURGE_BATCH: i32 = 10_000;

fn bad(what: &str, value: &str) -> StoreError {
    StoreError(format!("unexpected {what} `{value}` in the users tables"))
}

fn via(s: &str) -> StoreResult<Via> {
    Via::parse(s).ok_or_else(|| bad("client", s))
}

fn user_from_row(row: &PgRow) -> StoreResult<User> {
    let status: String = row.try_get("status")?;
    let created: String = row.try_get("created_client")?;
    Ok(User {
        handle: row.try_get("handle")?,
        display_name: row.try_get("display_name")?,
        sign_in_name: row.try_get("sign_in_name")?,
        sign_in_name_confirmed_at: row.try_get("sign_in_name_confirmed_at")?,
        sign_in_name_confirmed_by: row.try_get("sign_in_name_confirmed_by")?,
        atlassian_email: row.try_get("atlassian_email")?,
        github_login: row.try_get("github_login")?,
        status: Status::parse(&status).ok_or_else(|| bad("status", &status))?,
        created_at: row.try_get("created_at")?,
        created_by: row.try_get("created_by")?,
        created_via: via(&created)?,
        deactivated_at: row.try_get("deactivated_at")?,
        deactivated_by: row.try_get("deactivated_by")?,
        roles: Default::default(),
        identities: Default::default(),
    })
}

/// Everything the rules read, as of `now`.
async fn load_directory(conn: &mut PgConnection, now: DateTime<Utc>) -> StoreResult<Directory> {
    let mut dir = Directory::default();
    for row in sqlx::query(
        "SELECT handle, display_name, sign_in_name, sign_in_name_confirmed_at,
                sign_in_name_confirmed_by, atlassian_email, github_login, status, created_at,
                created_by, created_client, deactivated_at, deactivated_by
         FROM users",
    )
    .fetch_all(&mut *conn)
    .await?
    {
        let user = user_from_row(&row)?;
        dir.users.insert(user.handle.clone(), user);
    }
    for row in sqlx::query("SELECT handle, role, granted_at, granted_by FROM user_roles")
        .fetch_all(&mut *conn)
        .await?
    {
        let handle: String = row.try_get("handle")?;
        let role: String = row.try_get("role")?;
        let role = UserRole::parse(&role).ok_or_else(|| bad("role", &role))?;
        if let Some(u) = dir.users.get_mut(&handle) {
            u.roles.insert(
                role,
                RoleGrant {
                    at: row.try_get("granted_at")?,
                    by: row.try_get("granted_by")?,
                },
            );
        }
    }
    for row in sqlx::query(
        "SELECT handle, kind, issuer, subject, linked_at, linked_by, linked_client, host_confirmed
         FROM user_identities",
    )
    .fetch_all(&mut *conn)
    .await?
    {
        let handle: String = row.try_get("handle")?;
        let kind: String = row.try_get("kind")?;
        let kind = IdentityKind::parse(&kind).ok_or_else(|| bad("identity kind", &kind))?;
        let linked: String = row.try_get("linked_client")?;
        if let Some(u) = dir.users.get_mut(&handle) {
            u.identities.insert(
                kind,
                Identity {
                    kind,
                    issuer: row.try_get("issuer")?,
                    subject: row.try_get("subject")?,
                    linked_at: row.try_get("linked_at")?,
                    linked_by: row.try_get("linked_by")?,
                    linked_via: via(&linked)?,
                    host_confirmed: row.try_get("host_confirmed")?,
                },
            );
        }
    }
    if let Some(row) = sqlx::query(
        "SELECT enabled_at, enabled_by, expires_at FROM pilot_exceptions
         WHERE name = 'admin_and_auditor' AND ended_at IS NULL",
    )
    .fetch_optional(&mut *conn)
    .await?
    {
        dir.exception = Some(PilotException {
            enabled_at: row.try_get("enabled_at")?,
            enabled_by: row.try_get("enabled_by")?,
            expires_at: row.try_get("expires_at")?,
        });
    }
    dir.capped_deactivations = sqlx::query_scalar(
        "SELECT at FROM role_changes
         WHERE change = 'deactivate' AND client IN ('web', 'bot')
           AND at > $1 - interval '1 hour' AND at <= $1
         ORDER BY at",
    )
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    dir.version = sqlx::query_scalar("SELECT v FROM directory_version")
        .fetch_one(&mut *conn)
        .await?;
    Ok(dir)
}

async fn execute(conn: &mut PgConnection, writes: &[Write]) -> Result<(), sqlx::Error> {
    for w in writes {
        match w {
            Write::InsertUser(u) => {
                sqlx::query(
                    "INSERT INTO users (handle, display_name, sign_in_name,
                         sign_in_name_confirmed_at, sign_in_name_confirmed_by, atlassian_email,
                         github_login, status, created_at, created_by, created_client,
                         deactivated_at, deactivated_by)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                )
                .bind(&u.handle)
                .bind(&u.display_name)
                .bind(&u.sign_in_name)
                .bind(u.sign_in_name_confirmed_at)
                .bind(&u.sign_in_name_confirmed_by)
                .bind(&u.atlassian_email)
                .bind(&u.github_login)
                .bind(u.status.as_str())
                .bind(u.created_at)
                .bind(&u.created_by)
                .bind(u.created_via.as_str())
                .bind(u.deactivated_at)
                .bind(&u.deactivated_by)
                .execute(&mut *conn)
                .await?;
            }
            Write::UpdateUser(u) => {
                sqlx::query(
                    "UPDATE users SET display_name = $2, sign_in_name = $3,
                         sign_in_name_confirmed_at = $4, sign_in_name_confirmed_by = $5,
                         atlassian_email = $6, github_login = $7, status = $8,
                         deactivated_at = $9, deactivated_by = $10
                     WHERE handle = $1",
                )
                .bind(&u.handle)
                .bind(&u.display_name)
                .bind(&u.sign_in_name)
                .bind(u.sign_in_name_confirmed_at)
                .bind(&u.sign_in_name_confirmed_by)
                .bind(&u.atlassian_email)
                .bind(&u.github_login)
                .bind(u.status.as_str())
                .bind(u.deactivated_at)
                .bind(&u.deactivated_by)
                .execute(&mut *conn)
                .await?;
            }
            Write::InsertRole {
                handle,
                role,
                grant,
            } => {
                sqlx::query(
                    "INSERT INTO user_roles (handle, role, granted_at, granted_by)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(handle)
                .bind(role.as_str())
                .bind(grant.at)
                .bind(&grant.by)
                .execute(&mut *conn)
                .await?;
            }
            Write::DeleteRole { handle, role } => {
                sqlx::query("DELETE FROM user_roles WHERE handle = $1 AND role = $2")
                    .bind(handle)
                    .bind(role.as_str())
                    .execute(&mut *conn)
                    .await?;
            }
            Write::InsertIdentity { handle, identity } => {
                sqlx::query(
                    "INSERT INTO user_identities (handle, kind, issuer, subject, linked_at,
                         linked_by, linked_client, host_confirmed)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(handle)
                .bind(identity.kind.as_str())
                .bind(&identity.issuer)
                .bind(&identity.subject)
                .bind(identity.linked_at)
                .bind(&identity.linked_by)
                .bind(identity.linked_via.as_str())
                .bind(identity.host_confirmed)
                .execute(&mut *conn)
                .await?;
            }
            Write::DeleteIdentity { handle, kind } => {
                sqlx::query("DELETE FROM user_identities WHERE handle = $1 AND kind = $2")
                    .bind(handle)
                    .bind(kind.as_str())
                    .execute(&mut *conn)
                    .await?;
            }
            Write::OpenException(e) => {
                sqlx::query(
                    "INSERT INTO pilot_exceptions (name, enabled_at, enabled_by, expires_at)
                     VALUES ('admin_and_auditor', $1, $2, $3)",
                )
                .bind(e.enabled_at)
                .bind(&e.enabled_by)
                .bind(e.expires_at)
                .execute(&mut *conn)
                .await?;
            }
            Write::EndException { ended_at, ended_by } => {
                sqlx::query(
                    "UPDATE pilot_exceptions SET ended_at = $1, ended_by = $2
                     WHERE name = 'admin_and_auditor' AND ended_at IS NULL",
                )
                .bind(ended_at)
                .bind(ended_by)
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    Ok(())
}

/// A write's consequence outside the users tables, in its own transaction (§6).
async fn run_effect(
    conn: &mut PgConnection,
    effect: &Effect,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    use crate::pg as store;
    match effect {
        Effect::RevokeSessions(h) => {
            store::revoke_user_sessions(conn, h).await?;
        }
        Effect::SuspendCredentials(h) => {
            store::suspend_credentials(conn, h, now).await?;
        }
        Effect::RestoreCredentials(h) => {
            store::restore_credentials(conn, h).await?;
        }
        Effect::DeleteSuspendedCredentials(h) => {
            store::delete_suspended_credentials(conn, h).await?;
        }
        Effect::CloseHeldDrafts(h) => {
            store::close_owner_drafts(conn, h, now).await?;
        }
    }
    Ok(())
}

async fn insert_change(conn: &mut PgConnection, c: &RoleChange) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO role_changes (at, actor, client, host_user, target, change, role, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(c.at)
    .bind(&c.actor)
    .bind(c.via.as_str())
    .bind(&c.host_user)
    .bind(&c.target)
    .bind(c.change.as_str())
    .bind(c.role.map(UserRole::as_str))
    .bind(&c.detail)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// An audit event, in the caller's transaction (the purge's `system` rule in `pg.rs` is for
/// application events through the audit sink; these are the users module's own).
async fn insert_event(conn: &mut PgConnection, e: &AuditEvent) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_events
             (at, run_id, user_handle, skill, bot, tool, outcome, sources, args_hash,
              output_ref, detail, client)
         VALUES ($1, $2, $3, NULL, NULL, $4, $5, '{}', NULL, $6, $7, $8)",
    )
    .bind(e.at)
    .bind(&e.run_id)
    .bind(&e.user)
    .bind(&e.tool)
    .bind(e.outcome.as_str())
    .bind(&e.output_ref)
    .bind(&e.detail)
    .bind(e.client.as_str())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A database refusal as a [`UserError`]: the trigger's `<code>: <message>`, a unique key,
/// a check, a foreign key; anything else is a storage error.
fn map_db(e: sqlx::Error) -> UserError {
    if let sqlx::Error::Database(db) = &e {
        let message = db.message().to_string();
        match db.code().as_deref() {
            Some("23505") => {
                let what = match db.constraint().unwrap_or_default() {
                    "users_sign_in_name" => "that sign-in name belongs to another user",
                    "users_atlassian_email" => "that Atlassian email belongs to another user",
                    "users_github_login" => "that GitHub login belongs to another user",
                    "users_pkey" => "that handle is taken (handles are never reused)",
                    "user_identities_kind_issuer_subject_key" => {
                        "that key belongs to another user (one person per key)"
                    }
                    "user_identities_handle_kind_key" => "the user already has a key of that kind",
                    "one_open_exception" => "a pilot exception is already open",
                    _ => "a key that must be unique is taken",
                };
                return UserError::refused(Refusal::Taken, what);
            }
            Some("23514") => {
                if let Some((code, rest)) = message.split_once(": ") {
                    if let Some(refusal) = Refusal::parse(code) {
                        return UserError::refused(refusal, rest);
                    }
                }
                return UserError::refused(Refusal::Invalid, message);
            }
            Some("23503") => return UserError::refused(Refusal::NotFound, "no such user"),
            _ => {}
        }
    }
    UserError::Store(e.into())
}

impl PgStore {
    async fn apply_in_tx(&self, actor: &Actor, op: &Op, now: DateTime<Utc>) -> UserResult<Applied> {
        let mut tx = self.pool().begin().await.map_err(StoreError::from)?;
        sqlx::query(LOCK)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from)?;
        let dir = load_directory(&mut tx, now).await?;
        let plan = rules::plan(&dir, actor, op, now, &self.users_rules())?;
        execute(&mut tx, &plan.writes).await.map_err(map_db)?;
        for effect in &plan.effects {
            run_effect(&mut tx, effect, now)
                .await
                .map_err(|e| UserError::Store(e.into()))?;
        }
        for c in &plan.changes {
            insert_change(&mut tx, c).await.map_err(map_db)?;
        }
        for e in &plan.events {
            insert_event(&mut tx, &audit_event(actor, e, now))
                .await
                .map_err(map_db)?;
        }
        // The trigger runs here, under the lock, on a fresh snapshot.
        tx.commit().await.map_err(map_db)?;
        Ok(Applied { lines: plan.lines })
    }
}

#[async_trait]
impl UserStore for PgStore {
    fn rules(&self) -> Rules {
        self.users_rules()
    }

    async fn directory(&self, now: DateTime<Utc>) -> StoreResult<Directory> {
        let mut conn = self.pool().acquire().await?;
        load_directory(&mut conn, now).await
    }

    async fn apply(&self, actor: &Actor, op: Op, now: DateTime<Utc>) -> UserResult<Applied> {
        match self.apply_in_tx(actor, &op, now).await {
            Ok(applied) => Ok(applied),
            Err(e) => {
                if let Some(refusal) = e.refusal() {
                    let event = audit_event(actor, &denied_event(actor, &op, refusal), now);
                    let mut conn = self.pool().acquire().await.map_err(StoreError::from)?;
                    insert_event(&mut conn, &event)
                        .await
                        .map_err(StoreError::from)?;
                }
                Err(e)
            }
        }
    }

    async fn role_changes(
        &self,
        target: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<RoleChange>> {
        let rows = sqlx::query(
            "SELECT id, at, actor, client, host_user, target, change, role, detail
             FROM role_changes
             WHERE $1::text IS NULL OR target = $1
             ORDER BY id DESC
             LIMIT $2",
        )
        .bind(target)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let client: String = row.try_get("client")?;
                let change: String = row.try_get("change")?;
                let role: Option<String> = row.try_get("role")?;
                Ok(RoleChange {
                    id: row.try_get("id")?,
                    at: row.try_get("at")?,
                    actor: row.try_get("actor")?,
                    via: via(&client)?,
                    host_user: row.try_get("host_user")?,
                    target: row.try_get("target")?,
                    change: ChangeKind::parse(&change).ok_or_else(|| bad("change", &change))?,
                    role: match role {
                        Some(r) => Some(UserRole::parse(&r).ok_or_else(|| bad("role", &r))?),
                        None => None,
                    },
                    detail: row.try_get("detail")?,
                })
            })
            .collect()
    }

    async fn purge_role_changes(&self, _now: DateTime<Utc>) -> StoreResult<u64> {
        // The database decides what may go (migration 0015); `now()` there is the cutoff's
        // clock, as for the audit purge.
        let mut total = 0u64;
        for _ in 0..crate::retention::MAX_BATCHES {
            let row = sqlx::query("SELECT deleted, more FROM purge_role_changes($1)")
                .bind(PURGE_BATCH)
                .fetch_one(self.pool())
                .await?;
            total += u64::try_from(row.try_get::<i64, _>("deleted")?).unwrap_or_default();
            if !row.try_get::<bool, _>("more")? {
                break;
            }
        }
        Ok(total)
    }

    async fn snapshot(&self, now: DateTime<Utc>) -> StoreResult<Directory> {
        self.directory_snapshot(now).await
    }

    async fn link_entra(&self, link: &EntraLink, now: DateTime<Utc>) -> LinkResult {
        self.first_entra_link(link, now).await
    }
}

// --- The read API and the first Entra link (slice U2) ----------------------------------------

/// A unique key the identity insert hit, as a link refusal: one person per key
/// (`(kind, issuer, subject)`: another user won the race for this `(tid, oid)`), one key of
/// each kind per person (`(handle, kind)`: this user was linked to another one meanwhile).
fn link_refusal(e: &sqlx::Error) -> Option<LinkRefusal> {
    let sqlx::Error::Database(db) = e else {
        return None;
    };
    match (db.code().as_deref(), db.constraint()) {
        (Some("23505"), Some("user_identities_kind_issuer_subject_key")) => {
            Some(LinkRefusal::Taken)
        }
        (Some("23505"), Some("user_identities_handle_kind_key")) => Some(LinkRefusal::Linked),
        (Some("23514"), _) if db.message().starts_with("evals_account:") => {
            Some(LinkRefusal::EvalsAccount)
        }
        _ => None,
    }
}

impl PgStore {
    /// The directory in one REPEATABLE READ, READ ONLY transaction: every table and the
    /// version from the same snapshot.
    async fn directory_snapshot(&self, now: DateTime<Utc>) -> StoreResult<Directory> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let dir = load_directory(&mut tx, now).await?;
        tx.commit().await?;
        Ok(dir)
    }

    async fn first_entra_link(&self, link: &EntraLink, now: DateTime<Utc>) -> LinkResult {
        let mut seen: Option<Directory> = None;
        match self.link_in_tx(link, now, &mut seen).await {
            Err(LinkError::Refused(why)) => {
                let (actor, event) = link::refused_event(seen.as_ref(), &link.handle, why);
                let mut conn = self.pool().acquire().await.map_err(StoreError::from)?;
                insert_event(&mut conn, &link::audit(&actor, &event, now))
                    .await
                    .map_err(StoreError::from)?;
                Err(LinkError::Refused(why))
            }
            other => other,
        }
    }

    async fn link_in_tx(
        &self,
        link: &EntraLink,
        now: DateTime<Utc>,
        seen: &mut Option<Directory>,
    ) -> LinkResult {
        let mut tx = self.pool().begin().await.map_err(StoreError::from)?;
        sqlx::query(LOCK)
            .execute(&mut *tx)
            .await
            .map_err(StoreError::from)?;
        let dir = load_directory(&mut tx, now).await?;
        let planned = link::plan(&dir, link, now);
        *seen = Some(dir);
        let plan = match planned.map_err(LinkError::Refused)? {
            Planned::Same(user) => {
                return Ok(Linked {
                    user: *user,
                    first_link: false,
                })
            }
            Planned::Link(plan) => plan,
        };
        let refuse = |e: sqlx::Error| match link_refusal(&e) {
            Some(why) => LinkError::Refused(why),
            None => LinkError::Store(e.into()),
        };
        execute(&mut tx, std::slice::from_ref(&plan.write))
            .await
            .map_err(refuse)?;
        insert_change(&mut tx, &plan.change).await.map_err(refuse)?;
        insert_event(&mut tx, &audit_event(&plan.actor, &plan.event, now))
            .await
            .map_err(refuse)?;
        // The trigger runs here (and bumps the directory's version).
        tx.commit().await.map_err(refuse)?;
        Ok(Linked {
            user: plan.user,
            first_link: true,
        })
    }
}
