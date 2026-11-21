//! Each PM's team of marketplace bots (`docs/bot-marketplace-spec.md` §5, slice B1).
//!
//! A **persona** (Chief, Sprout, Echo) is a role the PM's own personal bot runs as, chosen per
//! ask. Which personas a PM may ask is their **team**: one `user_bots` row per persona they
//! picked. Nudge, the always-on generalist, is on every team and is never a row.
//!
//! - **Current state, not history.** A row is deleted when the PM removes the bot. History is
//!   the audit log: every change is a `team.change` event, written in the same transaction as
//!   the rows (in memory: recorded first, and the change is applied only if it was), and purged
//!   at 90 days by the one retention job like every other event. Nothing here deletes an event.
//! - **Inert rows.** A row whose persona is withdrawn (`offered: false`) or no longer in the
//!   catalog is kept, and grants nothing until the persona is offered again.
//! - **Limits.** At most [`MAX_TEAM`] rows per person, in code here and in the database (a
//!   deferred constraint trigger, migration 0024). The service also limits each person to
//!   [`CHANGES_PER_HOUR`] changes an hour.
//!
//! [`TeamStore`] has a Postgres implementation (below, on [`crate::pg::PgStore`]) and an
//! in-memory one ([`MemoryTeams`]) that behave the same; `tests/team.rs` runs every case on
//! both. One difference is deliberate: the in-memory store doesn't know the user database, so
//! it doesn't refuse a handle that isn't a user (Postgres's foreign key does); the service
//! checks the person before every write.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;

use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::store::{StoreError, StoreResult};

/// The most rows one person's team may hold (Nudge is never a row).
pub const MAX_TEAM: usize = pmos_contract::personas::MAX_PERSONAS;

/// The most team changes one person may make in an hour (`429 rate_limited` past it).
pub const CHANGES_PER_HOUR: usize = 30;

/// The audit action of a team change, allowed or refused.
pub const TEAM_CHANGE: &str = "team.change";

/// The audit action of the admin's read of who uses which persona.
pub const PERSONAS_READ: &str = "admin.personas_read";

/// The run id team changes are recorded under.
pub const TEAM_RUN: &str = "team";

/// How a row was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedVia {
    /// The "Pick your team" step.
    Onboarding,
    /// The Bots page.
    BotsPage,
    /// "Add" in the ask box's picker.
    Picker,
    /// The eval stack's seed (`evals` only, in memory; never from a route).
    Seed,
}

impl AddedVia {
    pub const ALL: [AddedVia; 4] = [
        AddedVia::Onboarding,
        AddedVia::BotsPage,
        AddedVia::Picker,
        AddedVia::Seed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AddedVia::Onboarding => "onboarding",
            AddedVia::BotsPage => "bots_page",
            AddedVia::Picker => "picker",
            AddedVia::Seed => "seed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// The ways a person's own request may name: never `seed`.
    pub fn from_request(s: &str) -> Option<Self> {
        Self::parse(s).filter(|v| *v != AddedVia::Seed)
    }
}

/// One persona on a person's team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamRow {
    pub persona: String,
    pub added_at: DateTime<Utc>,
    pub added_via: AddedVia,
}

/// One change to a person's team: keys already checked by the caller (valid names, never the
/// always-on persona, disjoint). Adding a key already there or removing one that isn't is a
/// no-op, and the change is applied atomically.
#[derive(Debug, Clone)]
pub struct TeamChange<'a> {
    pub handle: &'a str,
    pub add: &'a [String],
    pub remove: &'a [String],
    pub via: AddedVia,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamError {
    /// The change would leave more than [`MAX_TEAM`] rows. Nothing changed.
    #[error("a team holds at most {MAX_TEAM} bots")]
    Full,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The detail a `team.change` event records: keys only, in a fixed format
/// (`add=chief,sprout remove=echo via=onboarding`; an empty list is `-`).
pub fn change_detail(add: &[String], remove: &[String], via: AddedVia) -> String {
    let list = |keys: &[String]| {
        if keys.is_empty() {
            "-".to_string()
        } else {
            keys.join(",")
        }
    };
    format!(
        "add={} remove={} via={}",
        list(add),
        list(remove),
        via.as_str()
    )
}

/// The fixed notes a refused `team.change` records: the spec's four
/// (`docs/bot-marketplace-spec.md` §7), then the route's and the store's own.
pub const DENIED_NOTES: [&str; 10] = [
    "not_offered",
    "always_on",
    "rate_limited",
    "not_allowed",
    "not_a_member",
    "bad_request",
    "team_full",
    "forbidden",
    "bad_assertion",
    "assertion_required",
];

/// Is `detail` one the team code writes ([`change_detail`] or a [`DENIED_NOTES`] entry)? The
/// admin audit projection serves only such details.
pub fn is_safe_detail(detail: &str) -> bool {
    if DENIED_NOTES.contains(&detail) {
        return true;
    }
    let keys = |s: &str| {
        s == "-"
            || s.split(',')
                .all(|k| pmos_contract::bots::valid_name(k) && !k.is_empty())
    };
    let mut parts = detail.split(' ');
    let (Some(add), Some(remove), Some(via), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    add.strip_prefix("add=").is_some_and(keys)
        && remove.strip_prefix("remove=").is_some_and(keys)
        && via
            .strip_prefix("via=")
            .is_some_and(|v| AddedVia::parse(v).is_some())
}

/// The audit event of a team change by `handle`, through `client`.
pub fn change_event(
    client: Client,
    handle: &str,
    outcome: Outcome,
    detail: String,
    now: DateTime<Utc>,
) -> AuditEvent {
    AuditEvent {
        at: now,
        client,
        run_id: TEAM_RUN.to_string(),
        user: handle.to_string(),
        skill: None,
        bot: None,
        persona: None,
        tool: TEAM_CHANGE.to_string(),
        outcome,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: Some(handle.to_string()),
        detail: Some(detail),
    }
}

/// The eval stack's seed (`docs/bot-marketplace-spec.md` §5): every offered persona on the
/// `evals` account's team (`added_via: seed`), so eval tasks pass the same team check as
/// everyone. Called only where the in-memory user store is seeded (`PMOS_USERS_SEED`, which
/// the configuration refuses with a database or live connectors), and only when the seed has
/// an `evals` user. Returns how many personas were seeded.
pub async fn seed_evals(
    teams: &dyn TeamStore,
    users: &dyn crate::users::UserStore,
    roster: &crate::catalog::Roster,
    now: DateTime<Utc>,
) -> Result<usize, TeamError> {
    let evals = crate::users::EVALS;
    if users.snapshot(now).await?.get(evals).is_none() {
        return Ok(0);
    }
    let add: Vec<String> = roster
        .personas()
        .filter(|p| !p.always_on && roster.offered(&p.name).is_some())
        .map(|p| p.name.clone())
        .collect();
    if add.is_empty() {
        return Ok(0);
    }
    let change = TeamChange {
        handle: evals,
        add: &add,
        remove: &[],
        via: AddedVia::Seed,
        now,
    };
    let event = change_event(
        Client::Host,
        evals,
        Outcome::Ok,
        change_detail(&add, &[], AddedVia::Seed),
        now,
    );
    teams.change(&change, &event).await?;
    Ok(add.len())
}

#[async_trait]
pub trait TeamStore: Send + Sync {
    /// `handle`'s rows, by when they were added (ties by key).
    async fn team(&self, handle: &str) -> StoreResult<Vec<TeamRow>>;

    /// Is `persona` on `handle`'s team (a row, whatever the catalog says now)?
    async fn has(&self, handle: &str, persona: &str) -> StoreResult<bool>;

    /// Apply `change` and record `event` (its `team.change` audit event) together: both or
    /// neither. Returns the team afterwards. Refused with nothing changed when the result would
    /// hold more than [`MAX_TEAM`] rows.
    async fn change(
        &self,
        change: &TeamChange<'_>,
        event: &AuditEvent,
    ) -> Result<Vec<TeamRow>, TeamError>;

    /// Every row: `(persona, handle)`, by persona then handle. For the admin's read.
    async fn rows(&self) -> StoreResult<Vec<(String, String)>>;
}

/// The team in memory, with the same rules as Postgres.
#[derive(Default)]
pub struct MemoryTeams {
    /// An async lock: a change holds it while its audit event is recorded.
    rows: tokio::sync::Mutex<BTreeMap<String, BTreeMap<String, TeamRow>>>,
    audit: Option<Arc<dyn AuditSink>>,
}

impl MemoryTeams {
    /// Record each change's audit event to `audit` (as the database does in its transaction).
    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(audit);
        self
    }
}

fn sorted(rows: &BTreeMap<String, TeamRow>) -> Vec<TeamRow> {
    let mut out: Vec<TeamRow> = rows.values().cloned().collect();
    out.sort_by(|a, b| {
        a.added_at
            .cmp(&b.added_at)
            .then_with(|| a.persona.cmp(&b.persona))
    });
    out
}

#[async_trait]
impl TeamStore for MemoryTeams {
    async fn team(&self, handle: &str) -> StoreResult<Vec<TeamRow>> {
        let rows = self.rows.lock().await;
        Ok(rows.get(handle).map(sorted).unwrap_or_default())
    }

    async fn has(&self, handle: &str, persona: &str) -> StoreResult<bool> {
        let rows = self.rows.lock().await;
        Ok(rows.get(handle).is_some_and(|r| r.contains_key(persona)))
    }

    async fn change(
        &self,
        change: &TeamChange<'_>,
        event: &AuditEvent,
    ) -> Result<Vec<TeamRow>, TeamError> {
        // Planned under the lock, recorded, then applied: an event that can't be recorded
        // leaves the team as it was, and two changes for one person can't both pass the cap.
        let mut all = self.rows.lock().await;
        let mut next = all.get(change.handle).cloned().unwrap_or_default();
        for key in change.remove {
            next.remove(key);
        }
        for key in change.add {
            next.entry(key.clone()).or_insert_with(|| TeamRow {
                persona: key.clone(),
                added_at: change.now,
                added_via: change.via,
            });
        }
        if next.len() > MAX_TEAM {
            return Err(TeamError::Full);
        }
        if let Some(audit) = &self.audit {
            audit.record(event.clone()).await?;
        }
        let out = sorted(&next);
        if next.is_empty() {
            all.remove(change.handle);
        } else {
            all.insert(change.handle.to_string(), next);
        }
        Ok(out)
    }

    async fn rows(&self) -> StoreResult<Vec<(String, String)>> {
        let rows = self.rows.lock().await;
        let mut out: Vec<(String, String)> = rows
            .iter()
            .flat_map(|(h, r)| r.keys().map(move |p| (p.clone(), h.clone())))
            .collect();
        out.sort();
        Ok(out)
    }
}

// --- Postgres ----------------------------------------------------------------------------

fn row_from(r: &sqlx::postgres::PgRow) -> StoreResult<TeamRow> {
    let via: String = r.try_get("added_via").map_err(db)?;
    Ok(TeamRow {
        persona: r.try_get("persona").map_err(db)?,
        added_at: r.try_get("added_at").map_err(db)?,
        added_via: AddedVia::parse(&via)
            .ok_or_else(|| StoreError(format!("unexpected added_via in the database: {via}")))?,
    })
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError(e.to_string())
}

async fn team_in(conn: &mut sqlx::PgConnection, handle: &str) -> StoreResult<Vec<TeamRow>> {
    let rows = sqlx::query(
        "SELECT persona, added_at, added_via FROM user_bots WHERE handle = $1
         ORDER BY added_at, persona",
    )
    .bind(handle)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    rows.iter().map(row_from).collect()
}

#[async_trait]
impl TeamStore for crate::pg::PgStore {
    async fn team(&self, handle: &str) -> StoreResult<Vec<TeamRow>> {
        let mut conn = self.pool().acquire().await.map_err(db)?;
        team_in(&mut conn, handle).await
    }

    async fn has(&self, handle: &str, persona: &str) -> StoreResult<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM user_bots WHERE handle = $1 AND persona = $2)",
        )
        .bind(handle)
        .bind(persona)
        .fetch_one(self.pool())
        .await
        .map_err(db)
    }

    async fn change(
        &self,
        change: &TeamChange<'_>,
        event: &AuditEvent,
    ) -> Result<Vec<TeamRow>, TeamError> {
        let mut tx = self.pool().begin().await.map_err(db)?;
        // One change per person at a time, so the cap is checked against what is there.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pmos.user_bots'), hashtext($1))")
            .bind(change.handle)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM user_bots WHERE handle = $1 AND persona = ANY($2)")
            .bind(change.handle)
            .bind(change.remove)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for key in change.add {
            sqlx::query(
                "INSERT INTO user_bots (handle, persona, added_at, added_via)
                 VALUES ($1, $2, $3, $4) ON CONFLICT (handle, persona) DO NOTHING",
            )
            .bind(change.handle)
            .bind(key)
            .bind(change.now)
            .bind(change.via.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        let rows = team_in(&mut tx, change.handle).await?;
        if rows.len() > MAX_TEAM {
            // The deferred trigger would refuse it at commit; say so plainly first.
            return Err(TeamError::Full);
        }
        sqlx::query(
            "INSERT INTO audit_events
                 (at, run_id, user_handle, skill, bot, persona, tool, outcome, sources,
                  args_hash, output_ref, detail, client)
             VALUES ($1, $2, $3, NULL, NULL, NULL, $4, $5, '{}', NULL, $6, $7, $8)",
        )
        .bind(event.at)
        .bind(&event.run_id)
        .bind(&event.user)
        .bind(&event.tool)
        .bind(event.outcome.as_str())
        .bind(&event.output_ref)
        .bind(&event.detail)
        .bind(event.client.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        match tx.commit().await {
            Ok(()) => Ok(rows),
            Err(sqlx::Error::Database(e)) if e.message().contains("at most 16 bots") => {
                Err(TeamError::Full)
            }
            Err(e) => Err(TeamError::Store(db(e))),
        }
    }

    async fn rows(&self) -> StoreResult<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT persona, handle FROM user_bots ORDER BY persona, handle")
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("persona").map_err(db)?,
                    r.try_get("handle").map_err(db)?,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_are_a_fixed_format() {
        let d = change_detail(
            &["chief".into(), "sprout".into()],
            &["echo".into()],
            AddedVia::Onboarding,
        );
        assert_eq!(d, "add=chief,sprout remove=echo via=onboarding");
        assert!(is_safe_detail(&d));
        assert_eq!(
            change_detail(&[], &["echo".into()], AddedVia::BotsPage),
            "add=- remove=echo via=bots_page"
        );
        for note in DENIED_NOTES {
            assert!(is_safe_detail(note));
        }
        for bad in [
            "add=Chief remove=- via=onboarding",
            "add=chief remove=- via=somewhere",
            "add=chief remove=- via=onboarding extra",
            "add=chief",
            "a prompt the PM typed",
        ] {
            assert!(!is_safe_detail(bad), "{bad}");
        }
    }

    #[test]
    fn a_request_never_names_the_seed() {
        assert_eq!(AddedVia::from_request("picker"), Some(AddedVia::Picker));
        assert_eq!(AddedVia::from_request("seed"), None);
        assert_eq!(AddedVia::parse("seed"), Some(AddedVia::Seed));
    }
}
