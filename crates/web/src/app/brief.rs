//! The daily brief and follow-ups: `GET /api/brief`, `POST /api/followups/{id}/dismiss`,
//! `POST /api/followups/{id}/draft`, and the producer's `PUT /internal/brief/{handle}`.
//!
//! All of it answers `501 not_built` until `PMOS_WEB_BRIEF=1`, so the app hides the section.
//! A follow-up is the PM's own: another PM's id, a dismissed, an expired or an unknown one is
//! `404 not_found`. Drafting makes a task for the PM's personal bot; its draft then arrives
//! as an ordinary approval, and nothing is posted without the PM approving it.
//!
//! **Nothing calls the internal route yet.** It is the seam for the Slack brief routine (or a
//! later job) to fill a PM's day: it needs `PMOS_WEB_BRIEF_TOKEN` as a bearer token, replaces
//! that PM's follow-ups and `changed` items, and checks every field (counts, lengths, one-line
//! text, `http`/`https` urls) before anything is stored.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put, MethodRouter};
use axum::{Json, Router};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::json;

use super::{is_json, now, parse, task_out, valid_id, ApiError, ApiResult, AppState, Signed};
use crate::followups::{
    Changed, FollowUp, Kind, NewFollowUp, MAX_ITEMS, MAX_SUMMARY, MAX_SYSTEM, MAX_URL, MAX_WHO,
};
use crate::upstream::UpstreamError;

/// The largest body the internal route reads.
pub const MAX_BODY_BYTES: usize = 128 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/brief", get(get_brief))
        .route("/followups/{id}/dismiss", post(dismiss))
        .route("/followups/{id}/draft", post(draft))
}

pub(super) fn internal() -> MethodRouter<AppState> {
    put(put_internal).layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

fn not_built() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_built",
        "The daily brief isn't turned on yet.",
    )
}

fn on(state: &AppState) -> Result<(), ApiError> {
    if state.0.brief.enabled {
        Ok(())
    } else {
        Err(not_built())
    }
}

/// Purge what has expired. Runs at sign-in, which is cheap enough and often enough that no
/// scheduler is needed; a failure is logged, never shown.
pub(super) async fn housekeeping(state: &AppState) {
    let at = now(state);
    let (idle, max) = (state.0.idle, state.0.max);
    match state.0.sessions.purge(at, idle, max).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(removed = n, "purged expired sessions"),
        Err(e) => tracing::warn!(error = %e, "session purge failed"),
    }
    if let Err(e) = state.0.settings.purge(at).await {
        tracing::warn!(error = %e, "settings purge failed");
    }
    if let Err(e) = state.0.followups.purge(at).await {
        tracing::warn!(error = %e, "follow-up purge failed");
    }
}

fn followup_json(f: &FollowUp, at: DateTime<Utc>) -> serde_json::Value {
    json!({
        "id": f.id, "system": f.system, "who": f.who, "summary": f.summary, "url": f.url,
        "age_days": (at - f.created).num_days().max(0),
    })
}

async fn get_brief(State(state): State<AppState>, signed: Signed) -> ApiResult {
    on(&state)?;
    let at = now(&state);
    let handle = signed.handle();
    let all = state
        .0
        .followups
        .list(handle, at)
        .await
        .map_err(ApiError::store)?;
    let day = state
        .0
        .followups
        .day(handle, at)
        .await
        .map_err(ApiError::store)?;
    let of = |kind: Kind| -> Vec<serde_json::Value> {
        all.iter()
            .filter(|f| f.kind == kind)
            .map(|f| followup_json(f, at))
            .collect()
    };
    let (date, changed) = match day {
        Some(d) => (d.date, d.changed),
        None => (at.date_naive(), Vec::new()),
    };
    Ok(Json(json!({
        "date": date.to_string(),
        "changed": changed.iter().map(|c| json!({
            "system": c.system, "title": c.title, "summary": c.summary, "url": c.url,
        })).collect::<Vec<_>>(),
        "waiting_on_you": of(Kind::WaitingOnYou),
        "you_wait_on": of(Kind::YouWaitOn),
    }))
    .into_response())
}

async fn dismiss(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    on(&state)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    // Idempotent: dismissing again is `204`; only an unknown, expired or someone else's id
    // is `404`.
    let found = state
        .0
        .followups
        .dismiss(signed.handle(), &id, now(&state))
        .await
        .map_err(ApiError::store)?;
    if !found {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The ask for the bot. The summary is a one-line note the producer wrote, passed as data.
fn draft_prompt(f: &FollowUp) -> String {
    let task = match f.kind {
        Kind::WaitingOnYou => format!(
            "Draft a reply to {who} on {system}. They are waiting on me about: \"{summary}\".",
            who = f.who,
            system = f.system,
            summary = f.summary,
        ),
        Kind::YouWaitOn => format!(
            "Draft a short, gentle nudge to {who} on {system}. I am waiting on them for: \
             \"{summary}\".",
            who = f.who,
            system = f.system,
            summary = f.summary,
        ),
    };
    format!(
        "{task}\nThe thread is here: {url}\nRead the thread first for what was actually said. \
         Write the draft through your normal write tool so it arrives as an approval for me to \
         review; do not send or post anything yourself. Keep it short and in my voice.",
        url = f.url,
    )
}

async fn draft(State(state): State<AppState>, signed: Signed, Path(id): Path<String>) -> ApiResult {
    on(&state)?;
    signed.need(Permission::AskBot)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let Some(f) = state
        .0
        .followups
        .open(signed.handle(), &id, now(&state))
        .await
        .map_err(ApiError::store)?
    else {
        return Err(ApiError::not_found());
    };
    let task = state
        .0
        .runtime
        .create_task(signed.handle(), &signed.bot(), &draft_prompt(&f))
        .await
        .map_err(|e| match e {
            UpstreamError::Refused {
                status: 403 | 404 | 422,
                ..
            } => ApiError::new(
                StatusCode::CONFLICT,
                "task_refused",
                "Your bot can't take this. If you're new to PM OS, finish onboarding in Slack \
                 first.",
            ),
            e => ApiError::upstream(e),
        })?;
    if task.requested_by != signed.handle() {
        return Err(ApiError::upstream(UpstreamError::Failed {
            service: "bot runtime",
            why: "made the task for someone else".into(),
        }));
    }
    tracing::info!(user = %signed.handle(), task = %task.id, followup = %id, "follow-up draft asked");
    Ok((StatusCode::CREATED, Json(task_out(task, Some(&[])))).into_response())
}

// The producer's route.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangedIn {
    system: String,
    title: String,
    #[serde(default)]
    summary: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowIn {
    system: String,
    who: String,
    summary: String,
    url: String,
    created_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefIn {
    date: Option<String>,
    #[serde(default)]
    changed: Vec<ChangedIn>,
    #[serde(default)]
    waiting_on_you: Vec<FollowIn>,
    #[serde(default)]
    you_wait_on: Vec<FollowIn>,
}

fn bad(field: &str, why: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "bad_brief",
        format!("{field}: {why}"),
    )
}

/// One line of plain text: trimmed, not empty (unless `allow_empty`), within `max`
/// characters, no control characters (so no line breaks).
fn line(field: &str, text: &str, max: usize, allow_empty: bool) -> Result<String, ApiError> {
    let text = text.trim();
    if text.is_empty() && !allow_empty {
        return Err(bad(field, "must not be empty"));
    }
    if text.chars().count() > max {
        return Err(bad(field, &format!("at most {max} characters")));
    }
    if text.chars().any(char::is_control) {
        return Err(bad(field, "must be one line of plain text"));
    }
    Ok(text.to_string())
}

fn url(field: &str, text: &str) -> Result<String, ApiError> {
    let text = line(field, text, MAX_URL, false)?;
    let parsed = reqwest::Url::parse(&text).map_err(|_| bad(field, "not a url"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || text.chars().any(char::is_whitespace)
    {
        return Err(bad(
            field,
            "must be an http or https url without credentials",
        ));
    }
    Ok(text)
}

fn system(field: &str, text: &str) -> Result<String, ApiError> {
    let text = line(field, text, MAX_SYSTEM, false)?;
    let mut chars = text.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok {
        Ok(text)
    } else {
        Err(bad(field, "lower-case letters, digits, `-` and `_` only"))
    }
}

type Checked = (NaiveDate, Vec<Changed>, Vec<NewFollowUp>);

fn validate(body: BriefIn, at: DateTime<Utc>) -> Result<Checked, ApiError> {
    for (name, n) in [
        ("changed", body.changed.len()),
        ("waiting_on_you", body.waiting_on_you.len()),
        ("you_wait_on", body.you_wait_on.len()),
    ] {
        if n > MAX_ITEMS {
            return Err(bad(name, &format!("at most {MAX_ITEMS} items")));
        }
    }
    let date = match body.date {
        None => at.date_naive(),
        Some(d) => NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .map_err(|_| bad("date", "must be YYYY-MM-DD"))?,
    };
    let changed = body
        .changed
        .iter()
        .map(|c| {
            Ok(Changed {
                system: system("changed.system", &c.system)?,
                title: line("changed.title", &c.title, MAX_SUMMARY, false)?,
                summary: line("changed.summary", &c.summary, MAX_SUMMARY, true)?,
                url: url("changed.url", &c.url)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let mut followups = Vec::new();
    for (kind, list) in [
        (Kind::WaitingOnYou, &body.waiting_on_you),
        (Kind::YouWaitOn, &body.you_wait_on),
    ] {
        for f in list {
            let created = match &f.created_at {
                None => None,
                Some(c) => {
                    let c = DateTime::parse_from_rfc3339(c)
                        .map_err(|_| bad("created_at", "must be an RFC 3339 time"))?
                        .with_timezone(&Utc);
                    if c > at + Duration::minutes(5) {
                        return Err(bad("created_at", "is in the future"));
                    }
                    Some(c)
                }
            };
            followups.push(NewFollowUp {
                kind,
                system: system("system", &f.system)?,
                who: line("who", &f.who, MAX_WHO, false)?,
                summary: line("summary", &f.summary, MAX_SUMMARY, false)?,
                url: url("url", &f.url)?,
                created,
            });
        }
    }
    Ok((date, changed, followups))
}

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

async fn put_internal(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut res = match fill(&state, &handle, &headers, &body).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    };
    // Server to server: never cached.
    res.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().expect("header"));
    res
}

async fn fill(state: &AppState, handle: &str, headers: &HeaderMap, body: &Bytes) -> ApiResult {
    // Off, or no token set: the route doesn't exist as far as a caller can tell.
    let Some(token) = state
        .0
        .brief
        .token
        .as_deref()
        .filter(|_| state.0.brief.enabled)
    else {
        return Err(not_built());
    };
    if !pmos_runtime::token_eq(bearer(headers), token) {
        tracing::warn!("refused a brief write with a bad token");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_token",
            "That service token isn't accepted.",
        ));
    }
    // Only for someone on the team: no rows for handles that aren't members.
    if !valid_id(handle) || super::load_team(state)?.by_handle(handle).is_none() {
        return Err(ApiError::not_found());
    }
    if !is_json(headers) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "bad_content_type",
            "Send application/json.",
        ));
    }
    let at = now(state);
    let (date, changed, followups) = validate(parse(body)?, at)?;
    let (n_changed, n_followups) = (changed.len(), followups.len());
    state
        .0
        .followups
        .replace(handle, date, changed, followups, at)
        .await
        .map_err(ApiError::store)?;
    tracing::info!(user = %handle, changed = n_changed, followups = n_followups, "brief filled");
    Ok(Json(json!({"changed": n_changed, "followups": n_followups})).into_response())
}
