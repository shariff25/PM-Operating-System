//! The bot marketplace's `/api` routes (`docs/web-api.md`, "Bots"; `docs/bot-marketplace-spec.md`
//! §6, slice W1): `GET /api/bots`, `PUT /api/me/team`, and what `GET /api/me`, the ask route,
//! drafts, tasks and the reviewer's views add.
//!
//! **The tool server decides.** The catalog (`GET /internal/personas`) and the PM's own team
//! (`GET` and `PUT /internal/team/{handle}`) are read and changed there, every call naming the
//! signed-in person and carrying a user assertion for them, bound to the exact request. What
//! `pmos-web` checks first (the shape of a key, `nudge`, whether a key to add is offered, and
//! for an ask whether the bot is on the team) refuses early and fails closed; it never grants
//! anything the tool server would refuse. There is no route for anyone's team but the
//! signed-in person's own: the handle sent upstream is always theirs.
//!
//! **Catalog text is untrusted content** (names, titles, summaries, example asks, need labels):
//! it is passed on as the JSON strings the tool server sent, byte for byte, re-picked field by
//! field ([`persona_card`]); nothing here builds HTML or instructions from it.
//!
//! **Removing a bot** moves or pauses its routines at the runtime *before* the team changes
//! (spec §6, "Removal"); if that can't be done, the team is left as it was.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use pmos_contract::bots::valid_name;
use pmos_contract::permissions::Permission;
use pmos_contract::personas::ALWAYS_ON;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{no_longer_a_member, valid_id, ApiError, ApiResult, AppState, Signed};
use crate::drafts::{persona_key, DraftSummary};
use crate::settings::UserState;
use crate::upstream::UpstreamError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/bots", get(list_bots))
        .route("/me/team", put(change_team))
}

/// How long a catalog read is reused for attribution (never for `GET /api/me`, `GET
/// /api/bots` or a team change, which read it afresh).
pub const CATALOG_FRESH: Duration = Duration::from_secs(10);
/// How long a catalog read may stand in for attribution while the tool server can't be read.
pub const CATALOG_LAST_GOOD: Duration = Duration::from_secs(5 * 60);
/// How many of the PM's latest tasks with one bot `last_used_at` looks at (`GET
/// /v1/tasks?persona=<key>&limit=`): a few, so a hidden revision task can be skipped.
pub const LAST_USED_TASKS: usize = 5;
/// The most keys one team change names, in `add` and in `remove` (the tool server's limit).
pub const MAX_TEAM_KEYS: usize = 16;
/// Where a team change comes from (the tool server's `added_via`, less `seed`).
pub const VIA: [&str; 3] = ["onboarding", "bots_page", "picker"];
/// What happens to a removed bot's routines.
pub const ROUTINES: [&str; 2] = ["nudge", "pause"];

/// The offered personas, as the tool server served them.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Re-picked by [`persona_card`], in the tool server's order (Nudge first, then by
    /// `order`).
    pub personas: Vec<Value>,
    pub offered: BTreeSet<String>,
}

impl Catalog {
    fn from_wire(v: &Value) -> Option<Self> {
        let list = v.get("personas")?.as_array()?;
        let mut personas = Vec::new();
        let mut offered = BTreeSet::new();
        for p in list {
            match persona_card(p) {
                Some(card) => {
                    if let Some(k) = card["key"].as_str() {
                        if offered.insert(k.to_string()) {
                            personas.push(card);
                        }
                    }
                }
                None => tracing::warn!(
                    "the tool server listed a persona not in the catalog's shape; left out"
                ),
            }
        }
        Some(Self { personas, offered })
    }

    /// Offered bots besides Nudge.
    fn others(&self) -> usize {
        self.personas
            .iter()
            .filter(|p| p["always_on"] != json!(true) && p["key"] != json!(ALWAYS_ON))
            .count()
    }

    fn always_on(&self, key: &str) -> bool {
        key == ALWAYS_ON
            || self
                .personas
                .iter()
                .any(|p| p["key"] == json!(key) && p["always_on"] == json!(true))
    }
}

/// A catalog entry, field by field: each string exactly as it came (untrusted content, shown
/// as plain text by the app), or `None` when a field is missing or of another type.
pub fn persona_card(p: &Value) -> Option<Value> {
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(|v| json!(v));
    let strings = |k: &str| -> Option<Value> {
        let list = p.get(k)?.as_array()?;
        list.iter()
            .all(Value::is_string)
            .then(|| Value::Array(list.clone()))
    };
    let key = p
        .get("key")
        .and_then(Value::as_str)
        .filter(|k| valid_name(k))?;
    let needs = p
        .get("needs")
        .map(|n| -> Option<Value> {
            let mut out = Vec::new();
            for need in n.as_array()? {
                out.push(json!({
                    "source": need.get("source")?.as_str()?,
                    "label": need.get("label")?.as_str()?,
                    "without": need.get("without")?.as_str()?,
                    "met": need.get("met")?.as_bool()?,
                }));
            }
            Some(Value::Array(out))
        })
        .unwrap_or(Some(json!([])))?;
    Some(json!({
        "key": key,
        "display_name": s("display_name")?,
        "title": s("title")?,
        "mascot": s("mascot")?,
        "always_on": p.get("always_on").and_then(Value::as_bool)?,
        "summary": s("summary")?,
        "examples": strings("examples")?,
        "can_do": strings("can_do")?,
        "reads": strings("reads")?,
        "drafts": strings("drafts")?,
        "needs": needs,
    }))
}

fn bad_reply() -> ApiError {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
}

/// The catalog. `fresh`: read it now; otherwise a read at most [`CATALOG_FRESH`] old will do,
/// and while the tool server can't be read one at most [`CATALOG_LAST_GOOD`] old.
pub(super) async fn catalog(
    state: &AppState,
    signed: &Signed,
    fresh: bool,
) -> Result<Arc<Catalog>, ApiError> {
    let cached = state.0.catalog.lock().expect("catalog lock").clone();
    if let Some((at, c)) = &cached {
        if !fresh && at.elapsed() <= CATALOG_FRESH {
            return Ok(c.clone());
        }
    }
    let read = signed
        .toolserver(state)
        .personas(signed.handle())
        .await
        .map_err(ApiError::upstream)
        .and_then(|v| Catalog::from_wire(&v).ok_or_else(bad_reply));
    match read {
        Ok(c) => {
            let c = Arc::new(c);
            *state.0.catalog.lock().expect("catalog lock") = Some((Instant::now(), c.clone()));
            Ok(c)
        }
        Err(e) => match cached {
            Some((at, c)) if !fresh && at.elapsed() <= CATALOG_LAST_GOOD => {
                tracing::warn!(
                    code = e.code(),
                    "catalog unreadable: attribution on the last read"
                );
                Ok(c)
            }
            _ => Err(e),
        },
    }
}

/// What attribution is checked against: the catalog, or `None` when it can't be read (then
/// every key reads as not offered: the app never claims a bot it can't check).
pub(super) type Offered = Option<Arc<Catalog>>;

/// The catalog for attribution; `None` (logged) when it can't be read.
pub(super) async fn offered(state: &AppState, signed: &Signed) -> Offered {
    offered_if(state, signed, true).await
}

/// [`offered`], read only when `needed` (some item names a bot other than Nudge): Nudge's
/// attribution needs no catalog.
pub(super) async fn offered_if(state: &AppState, signed: &Signed, needed: bool) -> Offered {
    if !needed {
        return None;
    }
    match catalog(state, signed, false).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(
                code = e.code(),
                "catalog unavailable: attributed bots read as not offered"
            );
            None
        }
    }
}

/// Is `key` (`None` is Nudge) offered now?
pub(super) fn is_offered(key: Option<&str>, offered: &Offered) -> bool {
    match key {
        None => true,
        Some(k) => offered.as_ref().is_some_and(|c| c.offered.contains(k)),
    }
}

/// Set a draft summary's `persona_offered`.
pub(super) fn attribute(summary: &mut DraftSummary, offered: &Offered) {
    summary.persona_offered = is_offered(summary.persona.as_deref(), offered);
}

/// One bot on the PM's team, as the tool server's `Team` lists it.
#[derive(Debug, Clone)]
pub(super) struct TeamBot {
    pub key: String,
    pub offered: bool,
    pub added_at: Option<String>,
    pub added_via: Option<String>,
}

/// The PM's own team: Nudge, then each row; and the drafts per persona this month.
#[derive(Debug, Clone, Default)]
pub(super) struct Team {
    pub bots: Vec<TeamBot>,
    pub drafts_this_month: BTreeMap<String, u64>,
}

impl Team {
    fn from_wire(v: &Value, handle: &str) -> Option<Self> {
        if v.get("handle").and_then(Value::as_str) != Some(handle) {
            return None;
        }
        let mut bots = Vec::new();
        for b in v.get("bots")?.as_array()? {
            let key = b.get("key")?.as_str().filter(|k| valid_name(k))?;
            let text = |k: &str| b.get(k).and_then(Value::as_str).map(String::from);
            bots.push(TeamBot {
                key: key.to_string(),
                // Fail closed: anything but an explicit `true` is not offered.
                offered: b.get("offered").and_then(Value::as_bool) == Some(true),
                added_at: text("added_at"),
                added_via: text("added_via"),
            });
        }
        let drafts_this_month = v
            .get("drafts_this_month")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, n)| Some((k.clone(), n.as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            bots,
            drafts_this_month,
        })
    }

    fn find(&self, key: &str) -> Option<&TeamBot> {
        self.bots.iter().find(|b| b.key == key)
    }

    /// The offered keys on the team, Nudge first.
    pub fn keys(&self) -> Vec<String> {
        let mut out = vec![ALWAYS_ON.to_string()];
        out.extend(
            self.bots
                .iter()
                .filter(|b| b.offered && b.key != ALWAYS_ON)
                .map(|b| b.key.clone()),
        );
        out
    }
}

fn coded(status: StatusCode, code: &str, sentence: &str) -> ApiError {
    ApiError::new(status, code, sentence)
}

pub(super) fn bot_not_on_team() -> ApiError {
    coded(
        StatusCode::CONFLICT,
        "bot_not_on_team",
        "That bot isn't on your team. Add it to your team first. Nothing changed.",
    )
}

pub(super) fn bot_unavailable() -> ApiError {
    coded(
        StatusCode::CONFLICT,
        "bot_unavailable",
        "That bot isn't offered any more. Nothing changed.",
    )
}

fn always_on() -> ApiError {
    coded(
        StatusCode::CONFLICT,
        "always_on",
        "Nudge is always on your team. Nothing changed.",
    )
}

fn lost_team_change() -> ApiError {
    coded(
        StatusCode::BAD_GATEWAY,
        "unknown_outcome",
        "PM OS can't tell whether your team changed. Reload the Bots page to see.",
    )
}

/// A tool server refusal of a team read or change, as `pmos-web` answers it: the same code
/// (B1's list), `pmos-web`'s own sentence. Anything else is `502 upstream_unavailable`.
fn team_refusal(status: u16, body: &Value) -> ApiError {
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    match (status, code) {
        (403, "not_allowed") => ApiError::not_allowed(),
        (403, "not_a_member") => no_longer_a_member(),
        (400, "bad_request") => {
            ApiError::bad_request("That team change isn't in a shape PM OS takes. Nothing changed.")
        }
        (409, "always_on") => always_on(),
        (409, "bot_unavailable") => bot_unavailable(),
        (409, "team_full") => coded(
            StatusCode::CONFLICT,
            "team_full",
            "A team holds at most 16 bots. Remove one first. Nothing changed.",
        ),
        (429, "rate_limited") => coded(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "You've changed your team many times this hour. Try again later. Nothing changed.",
        ),
        (503, "assertion_required") => coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "Changing your team needs signed requests, and this PM OS isn't running with them. \
             Nothing changed.",
        ),
        (503, "unavailable") => coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "PM OS storage isn't answering. Nothing changed. Try again in a minute.",
        ),
        _ => {
            tracing::warn!(
                status,
                "the tool server refused a team call with an unexpected code"
            );
            ApiError::upstream(UpstreamError::Refused {
                service: "tool server",
                status,
                code: None,
                error: None,
                audience: None,
            })
        }
    }
}

/// The signed-in PM's own team, from the tool server. Fails closed.
pub(super) async fn team(state: &AppState, signed: &Signed) -> Result<Team, ApiError> {
    let (status, v) = signed
        .toolserver(state)
        .team(signed.handle(), None)
        .await
        .map_err(ApiError::upstream)?;
    if !(200..300).contains(&status) {
        return Err(team_refusal(status, &v));
    }
    Team::from_wire(&v, signed.handle()).ok_or_else(bad_reply)
}

/// A persona key from a request: `Ok(None)` for Nudge, `Ok(Some(key))` for another, `Err` for
/// anything not shaped like a key.
pub(super) fn requested_key(key: &str) -> Result<Option<String>, ApiError> {
    if !valid_name(key) {
        return Err(ApiError::bad_request(
            "A bot is named by its key: lowercase letters, digits and single hyphens.",
        ));
    }
    Ok((key != ALWAYS_ON).then(|| key.to_string()))
}

/// May the signed-in PM ask `key` (`None` is Nudge) now? Nudge always; another only when it is
/// on their team (`409 bot_not_on_team`) and still offered (`409 bot_unavailable`), from a
/// fresh read of the team. Fails closed when the team can't be read.
pub(super) async fn check_on_team(
    state: &AppState,
    signed: &Signed,
    key: Option<&str>,
) -> Result<(), ApiError> {
    let Some(key) = key else {
        return Ok(());
    };
    let team = team(state, signed).await?;
    match team.find(key) {
        Some(b) if b.offered => Ok(()),
        Some(_) => Err(bot_unavailable()),
        None => Err(bot_not_on_team()),
    }
}

/// What `GET /api/me` says about the team (`docs/web-api.md`, "Me").
#[derive(Debug, Clone, Default)]
pub(super) struct MeTeam {
    pub may_ask: bool,
    /// The team or the catalog couldn't be read just now.
    pub unavailable: bool,
    pub keys: Vec<String>,
    /// Offered bots besides Nudge.
    pub others_offered: usize,
}

/// The team for `GET /api/me`: never an upstream call for someone who may not ask (an
/// Auditor), or on the last-known-good path.
pub(super) async fn me_team(state: &AppState, signed: &Signed) -> MeTeam {
    if !signed.member.allows(Permission::AskBot) {
        return MeTeam::default();
    }
    let unavailable = MeTeam {
        may_ask: true,
        unavailable: true,
        keys: vec![ALWAYS_ON.to_string()],
        others_offered: 0,
    };
    if signed.stale {
        return unavailable;
    }
    let (t, c) = (
        team(state, signed).await,
        catalog(state, signed, true).await,
    );
    match (t, c) {
        (Ok(t), Ok(c)) => MeTeam {
            may_ask: true,
            unavailable: false,
            keys: t.keys(),
            others_offered: c.others(),
        },
        (t, c) => {
            let code = t.err().or(c.err()).map(|e| e.code().to_string());
            tracing::warn!(user = %signed.handle(), code = ?code, "team unavailable for /api/me");
            unavailable
        }
    }
}

/// `Me`'s team fields: `team`, `ask_persona`, `team_unavailable`, `show_team_step` and
/// `show_team_note`, with the exact rules `docs/web-api.md` ("Me") gives.
pub(super) fn me_fields(st: &UserState, t: &MeTeam) -> Map<String, Value> {
    let seen = |k: &str| st.hints_seen.contains(k);
    let steps_done = ["tour_asked", "tour_opened", "tour_decided"]
        .iter()
        .filter(|k| seen(k))
        .count();
    let first_visit = !st.opened_a_draft;
    // The guided start, as the app's `tourState` works it out.
    let started = !seen("tour_skipped") && (first_visit || seen("tour_started") || steps_done > 0);
    let tour_active = st.nudges && started && steps_done < 3;
    let eligible = t.may_ask
        && !t.unavailable
        && t.others_offered > 0
        && !seen("team_picked")
        && !seen("team_skipped");
    let show_step = eligible && tour_active;
    let show_note = eligible && !tour_active && !seen("team_announced") && t.keys.len() == 1;
    let ask_persona = if !t.may_ask {
        Value::Null
    } else {
        let stored = st.ask_persona.as_deref().unwrap_or(ALWAYS_ON);
        if t.keys.iter().any(|k| k == stored) {
            json!(stored)
        } else {
            json!(ALWAYS_ON)
        }
    };
    let mut m = Map::new();
    m.insert("team".into(), json!(t.keys));
    m.insert("ask_persona".into(), ask_persona);
    m.insert("team_unavailable".into(), json!(t.unavailable));
    m.insert("show_team_step".into(), json!(show_step));
    m.insert("show_team_note".into(), json!(show_note));
    m
}

/// A routine as the Bots page shows it: the PM's own, read from the runtime.
fn routine_out(r: &Value) -> Option<(Option<String>, Value)> {
    let id = r
        .get("id")
        .and_then(Value::as_str)
        .filter(|i| valid_id(i))?;
    let persona = persona_key(r.get("persona").and_then(Value::as_str));
    let schedule = r.get("schedule");
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    Some((
        persona,
        json!({
            "id": id,
            "enabled": r.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            "cron": s(schedule.and_then(|x| x.get("cron"))),
            "tz": s(schedule.and_then(|x| x.get("tz"))),
            "prompt": s(r.get("prompt")),
            "next_run_at": s(r.get("next_run_at")),
        }),
    ))
}

/// What the runtime says of the PM's bots: each one's latest task and its routines. `None`
/// for each part it couldn't say.
struct Usage {
    last_used: Option<BTreeMap<Option<String>, String>>,
    routines: Option<Vec<(Option<String>, Value)>>,
}

/// The bots `usage` asks the runtime about: each offered one (Nudge as `None`) and each
/// withdrawn row.
fn usage_keys(catalog: &Catalog, team: &Team) -> Vec<Option<String>> {
    let mut keys: Vec<Option<String>> = catalog
        .personas
        .iter()
        .filter_map(|p| p["key"].as_str())
        .map(|k| (!catalog.always_on(k)).then(|| k.to_string()))
        .collect();
    keys.extend(
        team.bots
            .iter()
            .filter(|b| b.key != ALWAYS_ON && !catalog.offered.contains(&b.key))
            .map(|b| Some(b.key.clone())),
    );
    if !keys.contains(&None) {
        keys.insert(0, None);
    }
    keys
}

async fn usage(state: &AppState, signed: &Signed, keys: &[Option<String>]) -> Usage {
    let runtime = signed.runtime(state);
    let handle = signed.handle();
    let hidden = match state.0.settings.get(handle).await {
        Ok(st) => st.revision_tasks,
        Err(e) => {
            tracing::warn!(error = %e, "settings unavailable: last-used dates left out");
            return Usage {
                last_used: None,
                routines: None,
            };
        }
    };
    // One filtered read per bot (R1's `?persona=`), newest first. Only a task that is the PM's,
    // not hidden, and really ran as that bot counts, so a runtime that ignored the filter
    // can't put another bot's date on this one.
    let mut last_used = Some(BTreeMap::new());
    for key in keys {
        let filter = key.as_deref().unwrap_or(ALWAYS_ON);
        match runtime.tasks_as(handle, filter, LAST_USED_TASKS).await {
            Ok(tasks) => {
                let latest = tasks.iter().find(|t| {
                    t.requested_by == handle
                        && !hidden.contains(&t.id)
                        && persona_key(t.persona.as_deref()) == *key
                });
                if let (Some(out), Some(t)) = (last_used.as_mut(), latest) {
                    out.insert(key.clone(), t.created_at.clone());
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "tasks unavailable: last-used dates left out");
                last_used = None;
                break;
            }
        }
    }
    let routines = match runtime.routines(handle).await {
        Ok(list) => Some(
            list.iter()
                .filter(|r| r.get("owner").and_then(Value::as_str) == Some(handle))
                .filter_map(routine_out)
                .collect(),
        ),
        Err(e) => {
            tracing::warn!(error = ?e, "routines unavailable: left out of the Bots page");
            None
        }
    };
    Usage {
        last_used,
        routines,
    }
}

fn usage_fields(key: Option<&str>, team: &Team, usage: &Usage) -> Map<String, Value> {
    let owned = key.map(String::from);
    let mut m = Map::new();
    m.insert(
        "drafts_this_month".into(),
        json!(team
            .drafts_this_month
            .get(key.unwrap_or(ALWAYS_ON))
            .copied()
            .unwrap_or(0)),
    );
    m.insert(
        "last_used_at".into(),
        json!(usage.last_used.as_ref().and_then(|l| l.get(&owned))),
    );
    m.insert(
        "routines".into(),
        match &usage.routines {
            None => Value::Null,
            Some(list) => Value::Array(
                list.iter()
                    .filter(|(p, _)| *p == owned)
                    .map(|(_, r)| r.clone())
                    .collect(),
            ),
        },
    );
    m
}

/// `{bots: [BotCard], withdrawn: [WithdrawnBot]}`.
fn bots_answer(catalog: &Catalog, team: &Team, usage: &Usage) -> Value {
    let mut bots = Vec::new();
    for card in &catalog.personas {
        let key = card["key"].as_str().unwrap_or_default();
        let nudge = catalog.always_on(key);
        let row = team.find(key).filter(|_| !nudge);
        let mut c = card.as_object().cloned().unwrap_or_default();
        c.insert("on_team".into(), json!(nudge || row.is_some()));
        c.insert(
            "added_at".into(),
            json!(row.and_then(|r| r.added_at.clone())),
        );
        c.insert(
            "added_via".into(),
            json!(row.and_then(|r| r.added_via.clone())),
        );
        // No service records when a persona was first offered (W1, departure): never `true`.
        c.insert("new".into(), json!(false));
        let lookup = (!nudge).then_some(key);
        c.extend(usage_fields(lookup, team, usage));
        bots.push(Value::Object(c));
    }
    let withdrawn: Vec<Value> = team
        .bots
        .iter()
        .filter(|b| b.key != ALWAYS_ON && !catalog.offered.contains(&b.key))
        .map(|b| {
            let mut m = Map::new();
            m.insert("key".into(), json!(b.key));
            m.insert("added_at".into(), json!(b.added_at));
            m.insert("added_via".into(), json!(b.added_via));
            m.extend(usage_fields(Some(&b.key), team, usage));
            Value::Object(m)
        })
        .collect();
    json!({ "bots": bots, "withdrawn": withdrawn })
}

/// `GET /api/bots`: the offered bots, with which are on the PM's team and how each is used.
async fn list_bots(State(state): State<AppState>, signed: Signed) -> ApiResult {
    signed.need(Permission::AskBot)?;
    let catalog = catalog(&state, &signed, true).await?;
    let team = team(&state, &signed).await?;
    let usage = usage(&state, &signed, &usage_keys(&catalog, &team)).await;
    Ok(Json(bots_answer(&catalog, &team, &usage)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    via: String,
    #[serde(default)]
    routines: Option<String>,
}

/// Each key once, checked; `Err` for one not shaped like a key.
fn checked_keys(list: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out = BTreeSet::new();
    for k in list {
        if !valid_name(k) {
            return Err(ApiError::bad_request(
                "Every bot in `add` and `remove` is named by its key. Nothing changed.",
            ));
        }
        out.insert(k.clone());
    }
    Ok(out.into_iter().collect())
}

/// Move (`"nudge"`) or pause (`"pause"`) the PM's routines that run as one of `removed`, at
/// the runtime. `Err` stops the team change: `502 upstream_unavailable` when nothing was
/// changed, `502 unknown_outcome` once a routine may have been.
async fn settle_routines(
    state: &AppState,
    signed: &Signed,
    removed: &[String],
    how: &str,
) -> Result<usize, ApiError> {
    let runtime = signed.runtime(state);
    let handle = signed.handle();
    let list = runtime.routines(handle).await.map_err(ApiError::upstream)?;
    let body = if how == "pause" {
        json!({ "enabled": false })
    } else {
        // R1: `null` is Nudge.
        json!({ "persona": null })
    };
    let mut changed = 0;
    for r in list
        .iter()
        .filter(|r| r.get("owner").and_then(Value::as_str) == Some(handle))
    {
        let Some((Some(persona), out)) = routine_out(r) else {
            continue;
        };
        if !removed.contains(&persona) {
            continue;
        }
        let id = out["id"].as_str().unwrap_or_default();
        if let Err(e) = runtime.patch_routine(handle, id, &body).await {
            tracing::warn!(error = ?e, routine = %id, changed, "a removed bot's routine couldn't be moved; the team is unchanged");
            let lost = matches!(e, UpstreamError::Unknown { .. });
            return Err(if changed > 0 || lost {
                coded(
                    StatusCode::BAD_GATEWAY,
                    "unknown_outcome",
                    "PM OS couldn't move all of that bot's routines, so your team is unchanged. \
                     Some routines may have moved: reload the Bots page to see.",
                )
            } else {
                ApiError::upstream(e)
            });
        }
        changed += 1;
    }
    Ok(changed)
}

/// `PUT /api/me/team` `{add?, remove?, via, routines?}`: change the signed-in PM's own team.
async fn change_team(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    signed.need(Permission::AskBot)?;
    let req: TeamBody = serde_json::from_slice(&body).map_err(|_| {
        ApiError::bad_request(
            "The body is {add?: [key], remove?: [key], via, routines?}. Nothing changed.",
        )
    })?;
    if !VIA.contains(&req.via.as_str()) {
        return Err(ApiError::bad_request(
            "`via` is `onboarding`, `bots_page` or `picker`. Nothing changed.",
        ));
    }
    let how = req.routines.as_deref().unwrap_or("nudge");
    if !ROUTINES.contains(&how) {
        return Err(ApiError::bad_request(
            "`routines` is `nudge` or `pause`. Nothing changed.",
        ));
    }
    if req.add.len() > MAX_TEAM_KEYS || req.remove.len() > MAX_TEAM_KEYS {
        return Err(ApiError::bad_request(
            "At most 16 bots in `add` and in `remove`. Nothing changed.",
        ));
    }
    let (add, remove) = (checked_keys(&req.add)?, checked_keys(&req.remove)?);
    if add.is_empty() && remove.is_empty() {
        return Err(ApiError::bad_request(
            "Name at least one bot to add or remove. Nothing changed.",
        ));
    }
    if add.iter().any(|k| remove.contains(k)) {
        return Err(ApiError::bad_request(
            "A bot can't be both added and removed. Nothing changed.",
        ));
    }
    let catalog = catalog(&state, &signed, true).await?;
    if add.iter().chain(&remove).any(|k| catalog.always_on(k)) {
        return Err(always_on());
    }
    if add.iter().any(|k| !catalog.offered.contains(k)) {
        return Err(bot_unavailable());
    }
    // Routines first (spec §6, "Removal"): a bot leaves the team only once nothing of the
    // PM's runs as it any more.
    if !remove.is_empty() {
        let moved = settle_routines(&state, &signed, &remove, how).await?;
        if moved > 0 {
            tracing::info!(user = %signed.handle(), moved, how, "a removed bot's routines were settled before the team change");
        }
    }
    let upstream = json!({ "add": add, "remove": remove, "via": req.via });
    let answer = signed
        .toolserver(&state)
        .team(signed.handle(), Some(&upstream))
        .await;
    let (status, v) = match answer {
        Ok(a) => a,
        Err(UpstreamError::Unknown { why, .. }) => {
            tracing::warn!(why = %why, "a team change's answer was lost");
            return Err(lost_team_change());
        }
        Err(e) => return Err(ApiError::upstream(e)),
    };
    if status >= 500 && status != 503 {
        tracing::warn!(
            status,
            "a team change failed upstream; its outcome is unknown"
        );
        return Err(lost_team_change());
    }
    if !(200..300).contains(&status) {
        return Err(team_refusal(status, &v));
    }
    let team = Team::from_wire(&v, signed.handle()).ok_or_else(lost_team_change)?;
    tracing::info!(user = %signed.handle(), added = add.len(), removed = remove.len(), via = %req.via, "team changed");
    let usage = usage(&state, &signed, &usage_keys(&catalog, &team)).await;
    Ok(Json(bots_answer(&catalog, &team, &usage)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(key: &str) -> Value {
        json!({"key": key, "display_name": "Chief", "title": "Chief of Staff", "mascot": "chief",
               "always_on": false, "summary": "Runs <b>your</b> week", "examples": ["Prep me"],
               "can_do": ["meeting-prep"], "reads": ["jira"], "drafts": ["Jira issues"],
               "needs": [{"source": "analytics", "label": "an analytics source", "without": "I say so.", "met": false}],
               "role": "chief-of-staff", "owner": "SECRET"})
    }

    #[test]
    fn a_card_keeps_the_catalog_fields_only_and_their_text_as_it_came() {
        let card = persona_card(&persona("chief")).unwrap();
        assert_eq!(card["summary"], "Runs <b>your</b> week");
        assert!(card.get("role").is_none() && card.get("owner").is_none());
        assert_eq!(card["needs"][0]["met"], false);
        assert!(persona_card(&persona("Not A Key")).is_none());
        let mut bad = persona("chief");
        bad["examples"] = json!([1]);
        assert!(persona_card(&bad).is_none());
    }

    fn st(hints: &[&str], opened: bool) -> UserState {
        UserState {
            hints_seen: hints.iter().map(|h| h.to_string()).collect(),
            opened_a_draft: opened,
            ..UserState::default()
        }
    }

    fn team(keys: &[&str]) -> MeTeam {
        MeTeam {
            may_ask: true,
            unavailable: false,
            keys: keys.iter().map(|k| k.to_string()).collect(),
            others_offered: 3,
        }
    }

    #[test]
    fn the_team_step_and_note_follow_the_rules() {
        let t = team(&["nudge"]);
        // A first visit: the step, not the note.
        let m = me_fields(&st(&[], false), &t);
        assert_eq!(
            (m["show_team_step"].clone(), m["show_team_note"].clone()),
            (json!(true), json!(false))
        );
        // Picked or skipped: neither.
        for mark in ["team_picked", "team_skipped"] {
            let m = me_fields(&st(&[mark], false), &t);
            assert_eq!(
                (m["show_team_step"].clone(), m["show_team_note"].clone()),
                (json!(false), json!(false)),
                "{mark}"
            );
        }
        // A returning user: never the step; the note until dismissed.
        let m = me_fields(&st(&[], true), &t);
        assert_eq!(
            (m["show_team_step"].clone(), m["show_team_note"].clone()),
            (json!(false), json!(true))
        );
        let m = me_fields(&st(&["team_announced"], true), &t);
        assert_eq!(m["show_team_note"], false);
        // Nudges off: no step (the guided start is off), the note instead.
        let mut off = st(&[], false);
        off.nudges = false;
        let m = me_fields(&off, &t);
        assert_eq!(
            (m["show_team_step"].clone(), m["show_team_note"].clone()),
            (json!(false), json!(true))
        );
        // A returning user who already has a bot: no note.
        assert_eq!(
            me_fields(&st(&[], true), &team(&["nudge", "chief"]))["show_team_note"],
            false
        );
        // Nothing offered besides Nudge, or the team unknown: neither.
        let mut none = team(&["nudge"]);
        none.others_offered = 0;
        assert_eq!(me_fields(&st(&[], false), &none)["show_team_step"], false);
        let mut unknown = team(&["nudge"]);
        unknown.unavailable = true;
        assert_eq!(
            me_fields(&st(&[], false), &unknown)["show_team_step"],
            false
        );
        // An Auditor: neither, and no default bot.
        let m = me_fields(&st(&[], false), &MeTeam::default());
        assert_eq!(
            (
                m["show_team_step"].clone(),
                m["show_team_note"].clone(),
                m["ask_persona"].clone()
            ),
            (json!(false), json!(false), Value::Null)
        );
        assert_eq!(m["team"], json!([]));
    }

    #[test]
    fn the_default_bot_falls_back_to_nudge() {
        let mut s = st(&[], true);
        s.ask_persona = Some("chief".into());
        assert_eq!(
            me_fields(&s, &team(&["nudge", "chief"]))["ask_persona"],
            "chief"
        );
        assert_eq!(me_fields(&s, &team(&["nudge"]))["ask_persona"], "nudge");
        assert_eq!(
            me_fields(&st(&[], true), &team(&["nudge", "chief"]))["ask_persona"],
            "nudge"
        );
    }
}
