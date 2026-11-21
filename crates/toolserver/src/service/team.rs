//! The bot marketplace's rules at the tool server (`docs/bot-marketplace-spec.md` §5–§7, slice
//! B1): the catalog's projection, a PM's own team (read and change) and the admin's read of
//! who uses which bot. The routes are `app/team.rs`; the store is `crate::team`; what a
//! persona session may do is [`ToolService::create_bot_session_as`] and `policy_for`.
//!
//! - **Only the catalog's own fields leave.** A persona is served as [`persona_json`]: the
//!   fields the spec lists, with `reads`, `drafts` and `needs[].met` derived from its role's
//!   tools by `pmos lint`, never anything else of its file (owner, version, order, role, body).
//! - **A PM changes only their own team**, and only an active user who may ask a bot
//!   (`ask-bot`): an Auditor-only person gets `not_allowed`, a deactivated or unknown one
//!   `not_a_member`. Nobody changes another person's team; there is no route for it.
//! - **Fail closed.** A key that isn't offered (unknown, withdrawn, its role missing) can't be
//!   added (`bot_unavailable`); the always-on persona can't be added or removed (`always_on`);
//!   a key that isn't a valid name is `bad_request` and is never recorded. A withdrawn or
//!   deleted persona's row can still be removed.
//! - **Audited**: every change, allowed or refused, is a `team.change` event with a fixed
//!   detail (keys only); at most [`CHANGES_PER_HOUR`] attempts an hour per person.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use pmos_contract::permissions::Permission;
use pmos_contract::personas::{Persona, ALWAYS_ON};
use serde_json::{json, Value};

use super::ToolService;
use crate::audit::{Client, Outcome};
use crate::store::StoreError;
use crate::team::{
    change_detail, change_event, AddedVia, TeamChange, TeamError, TeamRow, CHANGES_PER_HOUR,
    MAX_TEAM,
};
use crate::users::access::permits;

/// Why a team read or change was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamRefusal {
    /// Not a user, or deactivated.
    #[error("this person isn't an active PM OS user")]
    NotAMember,
    /// Active, but may not ask a bot (an Auditor-only person).
    #[error("this person's role doesn't include asking bots, so they have no team")]
    NotAllowed,
    #[error("{0}")]
    BadRequest(String),
    /// The request named the always-on persona.
    #[error("Nudge is always on your team.")]
    AlwaysOn,
    /// A key to add isn't offered: unknown, withdrawn, or its role is missing.
    #[error("That bot isn't offered any more.")]
    Unavailable,
    #[error("too many team changes in the last hour; try again later")]
    RateLimited,
    /// The team would hold more than [`MAX_TEAM`] bots.
    #[error("a team holds at most {MAX_TEAM} bots; remove one first")]
    Full,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl TeamRefusal {
    /// The wire's `code`.
    pub fn code(&self) -> &'static str {
        match self {
            TeamRefusal::NotAMember => "not_a_member",
            TeamRefusal::NotAllowed => "not_allowed",
            TeamRefusal::BadRequest(_) => "bad_request",
            TeamRefusal::AlwaysOn => "always_on",
            TeamRefusal::Unavailable => "bot_unavailable",
            TeamRefusal::RateLimited => "rate_limited",
            TeamRefusal::Full => "team_full",
            TeamRefusal::Store(_) => "unavailable",
        }
    }

    /// The fixed note a refused change's audit event records (`crate::team::DENIED_NOTES`).
    fn note(&self) -> &'static str {
        match self {
            TeamRefusal::Unavailable => "not_offered",
            other => other.code(),
        }
    }
}

impl From<TeamError> for TeamRefusal {
    fn from(e: TeamError) -> Self {
        match e {
            TeamError::Full => TeamRefusal::Full,
            TeamError::Store(e) => TeamRefusal::Store(e),
        }
    }
}

/// A persona as the catalog serves it: the spec's `Persona` shape and nothing else.
pub fn persona_json(p: &Persona) -> Value {
    json!({
        "key": p.name,
        "display_name": p.display_name,
        "title": p.title,
        "mascot": p.mascot,
        "always_on": p.always_on,
        "summary": p.summary,
        "examples": p.examples,
        "can_do": p.can_do,
        "reads": p.reads,
        "drafts": p.drafts,
        "needs": p.needs.iter().map(|n| json!({
            "source": n.source,
            "label": n.label,
            "without": n.without,
            "met": n.met,
        })).collect::<Vec<_>>(),
    })
}

/// Midnight UTC on the first of `now`'s month.
fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Each key once, sorted; `Err` names the first key that isn't a valid persona name.
fn keys(list: &[String]) -> Result<Vec<String>, TeamRefusal> {
    let mut out = BTreeSet::new();
    for k in list {
        if !pmos_contract::bots::valid_name(k) {
            return Err(TeamRefusal::BadRequest(
                "every key is a persona's name: lowercase letters, digits and single hyphens"
                    .into(),
            ));
        }
        out.insert(k.clone());
    }
    Ok(out.into_iter().collect())
}

impl ToolService {
    /// `{catalog_sha, personas}`: the **offered** personas, the always-on one first, then by
    /// `order` (`GET /internal/personas`). The same projection for every caller.
    pub fn persona_catalog(&self) -> Value {
        let roster = self.roster();
        let personas: Vec<Value> = roster
            .personas()
            .filter(|p| roster.offered(&p.name).is_some())
            .map(|p| persona_json(p))
            .collect();
        json!({ "catalog_sha": self.content_sha(), "personas": personas })
    }

    /// May `handle` have a team? An active user holding `ask-bot`. Read fresh, for every call.
    async fn team_member(&self, handle: &str) -> Result<(), TeamRefusal> {
        let dir = self.people().directory().await?;
        match dir.get(handle) {
            Some(u) if !u.active() => Err(TeamRefusal::NotAMember),
            Some(u) if permits(u, Permission::AskBot) => Ok(()),
            Some(_) => Err(TeamRefusal::NotAllowed),
            None => Err(TeamRefusal::NotAMember),
        }
    }

    /// `handle`'s `Team`: `{handle, bots, drafts_this_month}`. `bots` lists Nudge, then each
    /// row in catalog order (a row whose persona left the catalog last, by key), each with
    /// `offered`; `drafts_this_month` counts this PM's approval items per persona since the
    /// first of the month, UTC (Nudge's under `nudge`).
    pub async fn team(&self, handle: &str, now: DateTime<Utc>) -> Result<Value, TeamRefusal> {
        self.team_member(handle).await?;
        let rows = self.stores().teams.team(handle).await?;
        self.team_answer(handle, &rows, now).await
    }

    async fn team_answer(
        &self,
        handle: &str,
        rows: &[TeamRow],
        now: DateTime<Utc>,
    ) -> Result<Value, TeamRefusal> {
        let roster = self.roster();
        let mut bots = vec![json!({
            "key": ALWAYS_ON,
            "always_on": true,
            "added_at": null,
            "offered": true,
        })];
        let rank = |key: &str| roster.personas().position(|p| p.name == key);
        let mut ordered: Vec<&TeamRow> = rows.iter().collect();
        ordered.sort_by(|a, b| {
            let (ra, rb) = (rank(&a.persona), rank(&b.persona));
            // Known personas in catalog order, then any no longer in it, by key.
            ra.is_none()
                .cmp(&rb.is_none())
                .then(ra.cmp(&rb))
                .then_with(|| a.persona.cmp(&b.persona))
        });
        for row in ordered {
            bots.push(json!({
                "key": row.persona,
                "always_on": false,
                "added_at": row.added_at,
                "added_via": row.added_via.as_str(),
                "offered": roster.offered(&row.persona).is_some(),
            }));
        }
        let drafts = self
            .stores()
            .approvals
            .persona_counts(Some(handle), month_start(now))
            .await?;
        Ok(json!({ "handle": handle, "bots": bots, "drafts_this_month": drafts }))
    }

    /// Change `handle`'s own team for `client`: add and remove persona keys atomically
    /// (`PUT /internal/team/{handle}`). Every attempt is audited as `team.change`.
    pub async fn change_team(
        &self,
        client: Client,
        handle: &str,
        add: &[String],
        remove: &[String],
        via: &str,
        now: DateTime<Utc>,
    ) -> Result<Value, TeamRefusal> {
        match self
            .try_change_team(client, handle, add, remove, via, now)
            .await
        {
            Ok(team) => Ok(team),
            Err(e) => {
                if !matches!(e, TeamRefusal::Store(_)) {
                    self.audit(change_event(
                        client,
                        handle,
                        Outcome::Denied,
                        e.note().to_string(),
                        now,
                    ))
                    .await;
                }
                Err(e)
            }
        }
    }

    async fn try_change_team(
        &self,
        client: Client,
        handle: &str,
        add: &[String],
        remove: &[String],
        via: &str,
        now: DateTime<Utc>,
    ) -> Result<Value, TeamRefusal> {
        self.team_member(handle).await?;
        if !self.count_team_change(handle, now) {
            return Err(TeamRefusal::RateLimited);
        }
        let via = AddedVia::from_request(via).ok_or_else(|| {
            TeamRefusal::BadRequest("`via` is `onboarding`, `bots_page` or `picker`".into())
        })?;
        if add.len() > MAX_TEAM || remove.len() > MAX_TEAM {
            return Err(TeamRefusal::BadRequest(format!(
                "at most {MAX_TEAM} keys in `add` and in `remove`"
            )));
        }
        let (add, remove) = (keys(add)?, keys(remove)?);
        if add.is_empty() && remove.is_empty() {
            return Err(TeamRefusal::BadRequest(
                "name at least one key in `add` or `remove`".into(),
            ));
        }
        if add.iter().any(|k| remove.contains(k)) {
            return Err(TeamRefusal::BadRequest(
                "a key can't be both added and removed".into(),
            ));
        }
        // The always-on persona is never a row (the lint names it `nudge`; any persona the
        // catalog marks always-on counts too).
        let roster = self.roster();
        let always_on =
            |k: &String| k == ALWAYS_ON || roster.persona(k).is_some_and(|p| p.always_on);
        if add.iter().chain(&remove).any(always_on) {
            return Err(TeamRefusal::AlwaysOn);
        }
        if add.iter().any(|k| roster.offered(k).is_none()) {
            return Err(TeamRefusal::Unavailable);
        }
        let change = TeamChange {
            handle,
            add: &add,
            remove: &remove,
            via,
            now,
        };
        let event = change_event(
            client,
            handle,
            Outcome::Ok,
            change_detail(&add, &remove, via),
            now,
        );
        let rows = self.stores().teams.change(&change, &event).await?;
        self.team_answer(handle, &rows, now).await
    }

    /// Count one team change attempt by `handle` at `now`; `false` (not counted) once
    /// [`CHANGES_PER_HOUR`] were made in the hour before.
    fn count_team_change(&self, handle: &str, now: DateTime<Utc>) -> bool {
        let mut all = self.team_changes.lock().expect("team changes lock");
        let since = now - Duration::hours(1);
        all.retain(|_, q| {
            while q.front().is_some_and(|t| *t <= since) {
                q.pop_front();
            }
            !q.is_empty()
        });
        let q = all.entry(handle.to_string()).or_default();
        if q.len() >= CHANGES_PER_HOUR {
            return false;
        }
        q.push_back(now);
        true
    }

    /// `AdminPersonas`: every persona in the catalog, offered or not, with how many people
    /// have it on their team and who, its needs, and how many items it drafted in the last 30
    /// days; Nudge first (on every team: `users` and `handles` are `null`). `unknown` lists
    /// team rows naming a persona no longer in the catalog. Keys, handles and counts only.
    pub async fn admin_personas(&self, now: DateTime<Utc>) -> Result<Value, StoreError> {
        let roster = self.roster();
        let rows = self.stores().teams.rows().await?;
        let drafts = self
            .stores()
            .approvals
            .persona_counts(None, now - Duration::days(30))
            .await?;
        let mut holders: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (persona, handle) in &rows {
            holders.entry(persona).or_default().push(handle);
        }
        let mut personas = Vec::new();
        for p in roster.personas() {
            let handles = holders.remove(p.name.as_str());
            let (users, handles) = if p.always_on {
                (Value::Null, Value::Null)
            } else {
                let h = handles.unwrap_or_default();
                (json!(h.len()), json!(h))
            };
            personas.push(json!({
                "key": p.name,
                "display_name": p.display_name,
                "offered": roster.offered(&p.name).is_some(),
                "always_on": p.always_on,
                "users": users,
                "handles": handles,
                "needs": p.needs.iter().map(|n| json!({
                    "source": n.source,
                    "label": n.label,
                    "met": n.met,
                })).collect::<Vec<_>>(),
                "drafts_30d": drafts.get(&p.name).copied().unwrap_or(0),
            }));
        }
        let unknown: Vec<Value> = holders
            .into_iter()
            .map(|(key, handles)| {
                json!({
                    "key": key,
                    "users": handles.len(),
                    "handles": handles,
                    "drafts_30d": drafts.get(key).copied().unwrap_or(0),
                })
            })
            .collect();
        Ok(json!({ "personas": personas, "unknown": unknown }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_month_starts_on_the_first_utc() {
        let now: DateTime<Utc> = "2026-11-21T15:30:00Z".parse().unwrap();
        assert_eq!(
            month_start(now),
            "2026-11-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn keys_are_deduplicated_and_checked() {
        assert_eq!(
            keys(&["sprout".into(), "chief".into(), "chief".into()]).unwrap(),
            vec!["chief".to_string(), "sprout".to_string()]
        );
        for bad in ["Chief", "chief!", "", "a--b", "<img>"] {
            assert!(keys(&[bad.to_string()]).is_err(), "{bad}");
        }
    }
}
