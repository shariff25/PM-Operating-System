//! The Reviewer's routes (`docs/web-api.md`, "Reviews"; slice U6a): `/api/reviews…`, mirroring
//! the tool server's `GET /internal/reviews`, `GET /internal/reviews/{id}` and
//! `POST /internal/reviews/{id}/recommendation`.
//!
//! **The tool server decides.** Every call names the signed-in person as `user=` and carries a
//! user assertion for them, bound to the exact request (body included); the tool server reads
//! who they are, whom they review and what they may see from its own tables. What `pmos-web`
//! checks first is a convenience: someone without the `reviewer` role gets `403 forbidden`
//! with no upstream call. A recommendation also passes the origin and CSRF checks every
//! `pmos-web` write does ([`Signed`]).
//!
//! **Answers are re-picked field by field** ([`review_draft`]): a draft's text, title, target
//! and sources are built the way the owner's draft is (`crate::drafts`), and only for a draft
//! the tool server marks `restricted: false`; anything else is metadata only. A reviewer's
//! note is never logged. **Refusals keep the tool server's code** with `pmos-web`'s own
//! sentence ([`REVIEW_CODES`]); an unknown code is `502 upstream_unavailable`, and a
//! recommendation whose answer was lost is `502 unknown_outcome`.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model::{valid_handle, Role};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{valid_id, ApiError, ApiResult, AppState, Signed};
use crate::drafts;
use crate::settings::UserState;
use crate::upstream::{Approval, ApprovalSource, UpstreamError};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/reviews", get(inbox))
        .route("/reviews/{id}", get(read))
        .route("/reviews/{id}/recommendation", post(recommend))
}

/// The refusals the tool server's reviewer routes answer with, the status `pmos-web` relays
/// them with, and `pmos-web`'s own sentence. Any other code is `502 upstream_unavailable`.
pub const REVIEW_CODES: [(&str, u16, &str); 11] = [
    ("bad_request", 400, "That request wasn't in a shape PM OS takes. Nothing changed."),
    ("invalid", 422, "A note is at most 1000 characters. Nothing changed."),
    ("forbidden", 403, "Only an active reviewer can review drafts. Nothing changed."),
    ("not_found", 404, "There's no such draft among the drafts you review. Nothing changed."),
    ("own_draft", 403, "This is your own draft: you decide it, you don't review it. Nothing changed."),
    ("restricted_draft", 403, "This draft uses restricted sources; its owner can share it with you. Nothing changed."),
    ("not_pending", 409, "This draft has already been decided. Nothing changed."),
    ("expired", 409, "This Slack draft passed its 24 hours. Nothing changed."),
    ("draft_changed", 409, "This draft changed since you read it. Read it again, then recommend. Nothing changed."),
    ("assertion_required", 503, "Recommending needs signed requests, and this PM OS isn't running with them. Nothing changed."),
    ("unavailable", 503, "PM OS storage isn't answering. Nothing changed. Try again in a minute."),
];

/// The longest note a recommendation takes, in characters (the tool server's limit).
pub const MAX_NOTE: usize = 1000;
/// The longest owner name shown.
const MAX_NAME: usize = 100;

fn bad_reply() -> ApiError {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
}

/// A tool server refusal, as `pmos-web` answers it: the same code, its own sentence.
fn refused(status: u16, body: &Value) -> ApiError {
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    match REVIEW_CODES.iter().find(|(c, ..)| *c == code) {
        Some((code, status, sentence)) => ApiError::new(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::CONFLICT),
            code,
            *sentence,
        ),
        None => {
            tracing::warn!(
                status,
                "the tool server refused a reviewer call with an unknown code"
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

/// Only someone holding `reviewer` (as the user database answered for this request).
fn need_reviewer(signed: &Signed) -> Result<(), ApiError> {
    if signed.member.roles.contains(&Role::Reviewer) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only a reviewer can review drafts.",
        ))
    }
}

async fn call(
    state: &AppState,
    signed: &Signed,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    let write = method != reqwest::Method::GET;
    let lost = || {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "unknown_outcome",
            "PM OS can't tell whether your recommendation was recorded. Open the draft again to see.",
        )
    };
    match signed
        .toolserver(state)
        .reviews(signed.handle(), method, segments, body)
        .await
    {
        Ok((status, v)) if (200..300).contains(&status) => Ok(v),
        // A server error other than `503` after the write was sent: it may have been recorded.
        Ok((status, _)) if write && status >= 500 && status != 503 => {
            tracing::warn!(
                status,
                "a recommendation failed upstream; its outcome is unknown"
            );
            Err(lost())
        }
        Ok((status, v)) => Err(refused(status, &v)),
        Err(UpstreamError::Unknown { why, .. }) if write => {
            tracing::warn!(why = %why, "a recommendation's answer was lost");
            Err(lost())
        }
        Err(e) => Err(ApiError::upstream(e)),
    }
}

/// Plain text of at most `max` characters: control characters removed, trimmed.
fn plain(text: &str, max: usize) -> String {
    let t: String = text.chars().filter(|c| !c.is_control()).take(max).collect();
    t.trim().to_string()
}

/// The reviewer's own recommendation, re-picked; `null` for none.
fn my_recommendation(v: &Value) -> Value {
    let Some(r) = v.as_object() else {
        return Value::Null;
    };
    let Some(verdict) = r
        .get("verdict")
        .and_then(Value::as_str)
        .filter(|v| matches!(*v, "approve" | "changes"))
    else {
        return Value::Null;
    };
    let note = r
        .get("note")
        .and_then(Value::as_str)
        .map(|n| {
            n.chars()
                .filter(|c| *c == '\n' || !c.is_control())
                .take(MAX_NOTE)
                .collect::<String>()
        })
        .filter(|n| !n.trim().is_empty());
    json!({
        "verdict": verdict,
        "note": note,
        "note_removed": r.get("note_removed").and_then(Value::as_bool).unwrap_or(false),
        "revision": r.get("draft_revision").and_then(Value::as_i64),
        "created_at": r.get("created_at").and_then(Value::as_str),
        "stale": r.get("stale").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// One draft as a reviewer gets it, from the tool server's review view; `None` for an item
/// that isn't a draft in this contract (another write tool) or isn't shaped as one. Text,
/// target, sources and the version only when the tool server says `restricted: false`.
pub fn review_draft(v: &Value) -> Option<Value> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|i| valid_id(i))?;
    let owner = v
        .get("owner")
        .and_then(Value::as_str)
        .filter(|h| valid_handle(h))?;
    let kind = drafts::kind(v.get("tool").and_then(Value::as_str)?)?;
    let name = v
        .get("owner_name")
        .and_then(Value::as_str)
        .map(|n| plain(n, MAX_NAME))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| owner.to_string());
    // Fail closed: anything but an explicit `false` is restricted.
    let restricted = v.get("restricted").and_then(Value::as_bool) != Some(false);
    let str_field = |k: &str| v.get(k).and_then(Value::as_str);
    let mut out = json!({
        "id": id,
        "owner": owner,
        "owner_name": name,
        "kind": kind,
        "status": drafts::status(str_field("status").unwrap_or_default()),
        "created_at": str_field("created_at"),
        "expires_at": str_field("expires_at"),
        "restricted": restricted,
        "revision": v.get("revision").and_then(Value::as_i64),
    });
    if restricted {
        return Some(out);
    }
    let mut wire = v.clone();
    wire["user"] = json!(owner);
    let item: Approval = serde_json::from_value(wire).ok()?;
    let draft = drafts::draft(&item, &UserState::default())?;
    let found: Vec<ApprovalSource> = v
        .get("sources")
        .cloned()
        .and_then(|s| serde_json::from_value(s).ok())
        .unwrap_or_default();
    let (draft, _) = drafts::with_sources(draft, &item, found);
    let extra = json!({
        "title": draft.summary.title,
        "body": draft.body,
        "target": draft.target,
        "sources": draft.sources,
        "edited": draft.edited,
        "args_hash": str_field("args_hash"),
        "my_recommendation": my_recommendation(v.get("my_recommendation").unwrap_or(&Value::Null)),
    });
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
    Some(out)
}

/// A draft's line in the inbox: [`review_draft`] without the body, target and sources.
fn summary(v: &Value) -> Option<Value> {
    let mut d = review_draft(v)?;
    if let Some(o) = d.as_object_mut() {
        for k in ["body", "target", "sources"] {
            o.remove(k);
        }
        if !o.contains_key("title") {
            o.insert("title".into(), Value::Null);
            o.insert("my_recommendation".into(), Value::Null);
        }
    }
    Some(d)
}

/// `GET /api/reviews`: the pending drafts of the PMs assigned to the signed-in reviewer.
async fn inbox(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need_reviewer(&signed)?;
    tracing::info!(user = %signed.member.handle, route = "/api/reviews", "reviewer read");
    let v = call(&state, &signed, reqwest::Method::GET, &[], None).await?;
    let (Some(items), Some(total)) = (
        v.get("items").and_then(Value::as_array),
        v.get("total").and_then(Value::as_u64),
    ) else {
        return Err(bad_reply());
    };
    let drafts: Vec<Value> = items.iter().filter_map(summary).collect();
    let pms: Vec<&str> = v
        .get("pms")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .filter(|h| valid_handle(h))
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "drafts": drafts,
        "total": total,
        "truncated": v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "pms": pms,
    }))
    .into_response())
}

/// `GET /api/reviews/{id}`.
async fn read(State(state): State<AppState>, signed: Signed, Path(id): Path<String>) -> ApiResult {
    need_reviewer(&signed)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    tracing::info!(user = %signed.member.handle, route = "/api/reviews/{id}", "reviewer read");
    let v = call(&state, &signed, reqwest::Method::GET, &[&id], None).await?;
    let draft = review_draft(&v).ok_or_else(bad_reply)?;
    Ok(Json(draft).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecommendBody {
    verdict: String,
    #[serde(default)]
    note: Option<String>,
    revision: i64,
    args_hash: String,
}

/// `POST /api/reviews/{id}/recommendation` `{verdict, note?, revision, args_hash}`.
async fn recommend(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    need_reviewer(&signed)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let req: RecommendBody = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("The body is {verdict, note?, revision, args_hash}."))?;
    if !matches!(req.verdict.as_str(), "approve" | "changes") {
        return Err(ApiError::bad_request(
            "`verdict` is `approve` or `changes`.",
        ));
    }
    if req.revision < 1 || req.args_hash.len() != 64 {
        return Err(ApiError::bad_request(
            "`revision` and `args_hash` are the ones the draft was read with.",
        ));
    }
    if req
        .note
        .as_deref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE)
    {
        let (code, status, sentence) = REVIEW_CODES[1];
        return Err(ApiError::new(
            StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
            code,
            sentence,
        ));
    }
    let mut upstream = Map::new();
    upstream.insert("verdict".into(), json!(req.verdict));
    if let Some(n) = req.note {
        upstream.insert("note".into(), json!(n));
    }
    upstream.insert("draft_revision".into(), json!(req.revision));
    upstream.insert("args_hash".into(), json!(req.args_hash));
    // Never the note: who and where only.
    tracing::info!(user = %signed.member.handle, route = "/api/reviews/{id}/recommendation", "recommendation");
    let v = call(
        &state,
        &signed,
        reqwest::Method::POST,
        &[&id, "recommendation"],
        Some(&Value::Object(upstream)),
    )
    .await?;
    let draft = v
        .get("draft")
        .and_then(review_draft)
        .ok_or_else(bad_reply)?;
    Ok(Json(json!({
        "recommendation": draft.get("my_recommendation").cloned().unwrap_or(Value::Null),
        "draft": draft,
    }))
    .into_response())
}
