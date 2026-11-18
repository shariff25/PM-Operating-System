//! The Reviewer's routes (`docs/user-database-spec.md` §8, slice U6a): a reviewer's inbox of
//! the pending drafts of the PMs assigned to them, one such draft, and a recommendation on it.
//! The rules are `crate::service` (`review_*`, `recommend`) and `crate::reviews`.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /internal/reviews?user=<reviewer>` | The inbox: `{items, total, truncated, pms}`. |
//! | `GET /internal/reviews/{id}?user=<reviewer>` | One draft, with its sources; metadata only when restricted. |
//! | `POST /internal/reviews/{id}/recommendation?user=<reviewer>` | `{verdict, note?, draft_revision, args_hash}`: `{recommendation, draft}`. |
//!
//! **Only `pmos-web`'s token**, acting for a signed-in person: the orchestrator's, the Slack
//! bot's and the console's get `403` (recommending happens in the web app only). **Every call
//! carries `pmos-web`'s signed user assertion**, bound to the request (body included), whose
//! subject is the `user=` it names, in every assertion mode (`401 bad_assertion` otherwise): a
//! reviewer reads other people's drafts, so no mode lets a call through unsigned. **A
//! recommendation also needs `require`**: on the development override (or `off`/`verify`) it
//! answers `503 assertion_required` and records nothing, as the Admin API's writes do.
//!
//! Who the reviewer is and what they may see is read from the user database for each call,
//! never from the assertion's roles. Every refusal is audited (`denied`, with its code); a
//! refusal before the reviewer is verified is recorded as `anonymous`.

use axum::extract::{OriginalUri, Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use super::admin::{coded, require_assertion};
use super::users::writes_allowed;
use super::{bearer, client_for, error, AppState};
use crate::audit::{Client, Outcome};
use crate::reviews::{
    clean_note, valid_full_hash, RecVerdict, INBOX_ACTION, READ_ACTION, RECOMMEND_ACTION,
};
use crate::service::{About, ReviewError};

/// The largest body a recommendation takes: a 1000-character note is at most 4000 bytes.
pub const MAX_REVIEW_BODY: usize = 16 * 1024;

/// Who a call refused before anyone was verified is recorded as.
const ANONYMOUS: &str = "anonymous";

pub(super) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/reviews", get(inbox))
        .route("/reviews/{id}", get(read))
        .route("/reviews/{id}/recommendation", post(recommend))
        .layer(middleware::from_fn_with_state(state, identify_reviewer))
}

/// The audit action and target of a reviewer route, from its method and path.
fn action_of(method: &Method, path: &str) -> (&'static str, Option<String>) {
    let path = path.strip_prefix("/internal").unwrap_or(path);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let target = parts.get(1).map(|s| s.to_string());
    match (method, parts.as_slice()) {
        (&Method::POST, _) => (RECOMMEND_ACTION, target),
        (_, ["reviews"]) => (INBOX_ACTION, None),
        _ => (READ_ACTION, target),
    }
}

async fn record_anonymous(state: &AppState, action: &str, target: Option<&str>, code: &str) {
    state
        .svc
        .record_review(
            ANONYMOUS,
            action,
            Outcome::Denied,
            target.map_or(About::Nothing, About::Id),
            Some(code.to_string()),
            Utc::now(),
        )
        .await;
}

/// `pmos-web` only, its assertion for the named reviewer required in every mode; a
/// recommendation only in `require`.
async fn identify_reviewer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let (action, target) = action_of(req.method(), &path);
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Web) => Client::Web,
        Some(_) => {
            record_anonymous(&state, action, target.as_deref(), "forbidden").await;
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web, acting for a signed-in reviewer, may use these routes",
            );
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let assertions = &state.people.assertions;
    if req.method() != Method::GET && !writes_allowed(assertions) {
        record_anonymous(&state, action, target.as_deref(), "assertion_required").await;
        tracing::warn!(
            "a recommendation was refused: the tool server isn't running in assertion require"
        );
        return coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "recommendations need signed user assertions (PMOS_ASSERTION_MODE=require); this tool server runs without them",
        );
    }
    let mut req = match require_assertion(assertions, client, req, MAX_REVIEW_BODY).await {
        Ok(checked) => checked,
        Err(refused) => {
            record_anonymous(&state, action, target.as_deref(), "bad_assertion").await;
            return *refused;
        }
    };
    req.extensions_mut().insert(client);
    next.run(req).await
}

#[derive(Deserialize)]
struct Named {
    user: Option<String>,
}

/// The reviewer the call names (already matched to the assertion's subject).
fn reviewer(q: &Named) -> Option<&str> {
    q.user.as_deref().map(str::trim).filter(|u| !u.is_empty())
}

fn bad_request(why: &str) -> Response {
    coded(StatusCode::BAD_REQUEST, "bad_request", why)
}

/// A refusal as the wire answers it.
fn refusal(e: ReviewError) -> Response {
    let status = match &e {
        ReviewError::Forbidden | ReviewError::OwnDraft | ReviewError::Restricted => {
            StatusCode::FORBIDDEN
        }
        ReviewError::NotFound => StatusCode::NOT_FOUND,
        ReviewError::NotPending | ReviewError::Expired | ReviewError::DraftChanged { .. } => {
            StatusCode::CONFLICT
        }
        ReviewError::Store(err) => {
            tracing::error!(error = %err, "storage unavailable for a reviewer call");
            return coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "PM OS storage is unavailable",
            );
        }
    };
    let mut body = json!({ "code": e.code(), "error": e.to_string() });
    if let ReviewError::DraftChanged {
        revision,
        args_hash,
    } = &e
    {
        body["revision"] = json!(revision);
        body["args_hash"] = json!(args_hash);
    }
    (status, Json(body)).into_response()
}

/// `GET /internal/reviews?user=<reviewer>`.
async fn inbox(State(state): State<AppState>, Query(q): Query<Named>) -> Response {
    let Some(who) = reviewer(&q) else {
        return bad_request("`user` (the reviewer) is required");
    };
    match state.svc.review_inbox(who, Utc::now()).await {
        Ok(inbox) => {
            let truncated = inbox.total > inbox.items.len();
            Json(json!({
                "items": inbox.items,
                "total": inbox.total,
                "truncated": truncated,
                "pms": inbox.pms,
            }))
            .into_response()
        }
        Err(e) => refusal(e),
    }
}

/// `GET /internal/reviews/{id}?user=<reviewer>`.
async fn read(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<Named>,
) -> Response {
    let Some(who) = reviewer(&q) else {
        return bad_request("`user` (the reviewer) is required");
    };
    match state.svc.review_item(who, &id, Utc::now()).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => refusal(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecommendBody {
    verdict: String,
    #[serde(default)]
    note: Option<String>,
    draft_revision: i64,
    args_hash: String,
}

/// `POST /internal/reviews/{id}/recommendation?user=<reviewer>`.
async fn recommend(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let Some(who) = reviewer(&q) else {
        return bad_request("`user` (the reviewer) is required");
    };
    let now = Utc::now();
    let refuse = |code: &'static str| {
        let state = state.clone();
        let who = who.to_string();
        let id = id.clone();
        async move {
            state
                .svc
                .record_review(
                    &who,
                    RECOMMEND_ACTION,
                    Outcome::Denied,
                    About::Id(&id),
                    Some(code.to_string()),
                    now,
                )
                .await;
        }
    };
    let parsed = serde_json::from_slice::<RecommendBody>(&body)
        .ok()
        .and_then(|b| {
            let verdict = RecVerdict::parse(&b.verdict)?;
            (b.draft_revision >= 1 && valid_full_hash(&b.args_hash)).then_some((verdict, b))
        });
    let Some((verdict, b)) = parsed else {
        refuse("bad_request").await;
        return bad_request(
            "the body is {verdict: \"approve\" | \"changes\", note?, draft_revision, args_hash} (see docs/web-api.md)",
        );
    };
    let note = match b.note.as_deref().map(clean_note) {
        None => None,
        Some(Ok(note)) => note,
        Some(Err(_)) => {
            refuse("invalid").await;
            return coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid",
                "a note is at most 1000 characters",
            );
        }
    };
    match state
        .svc
        .recommend(who, &id, verdict, note, b.draft_revision, &b.args_hash, now)
        .await
    {
        Ok((_, draft)) => Json(json!({
            "recommendation": draft["my_recommendation"].clone(),
            "draft": draft,
        }))
        .into_response(),
        Err(e) => refusal(e),
    }
}
