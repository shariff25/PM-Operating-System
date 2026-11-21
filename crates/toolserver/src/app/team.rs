//! The bot marketplace's internal routes (`docs/bot-marketplace-spec.md` §6, slice B1). The
//! rules are `crate::service::team`; the store is `crate::team`.
//!
//! | Route | Token | Does |
//! | --- | --- | --- |
//! | `GET /internal/personas` | `pmos-web`, the orchestrator (runtime), the Slack bot | `{catalog_sha, personas}`: the offered personas, Nudge first. The same projection for each. |
//! | `GET /internal/team/{handle}` | `pmos-web` | `Team`: the person's own team. |
//! | `PUT /internal/team/{handle}` | `pmos-web` | `{add?, remove?, via}`: change it, atomically; answers `Team`. |
//! | `GET /internal/admin/personas?user=<viewer>` | `pmos-web` | `AdminPersonas`: who uses which, for `view-audit-log` or `manage-members`. |
//! | `POST /internal/admin/personas/drafts/{id}/release?user=<admin>` | `pmos-web` | An Admin (`edit-content`) releases a draft held because its bot was withdrawn. Assertion `require` only. |
//!
//! **`/internal/team/{handle}` acts for one PM.** Only `pmos-web`'s token (every other gets
//! `403`); in **every** assertion mode the call carries `pmos-web`'s signed user assertion,
//! bound to the request (body included): missing or invalid is `401 bad_assertion`, a subject
//! other than the path's `handle` is `403 forbidden`. A change also needs assertion `require`:
//! on the development override (or `off`/`verify`) it answers `503 assertion_required` and
//! changes nothing, as the reviewer routes do. Who the person is, and whether they may have a
//! team, is read from the user database on every call, never from the assertion's roles.
//!
//! **The catalog** names no one, so it needs no assertion (one that is sent is checked as
//! `PMOS_ASSERTION_MODE` says). The console's token gets `403`.
//!
//! **The admin read** is checked as the other admin reads are (`app/admin.rs`): the viewer's
//! permission in the tool server's own tables, the assertion as the mode says, and recorded as
//! `admin.personas_read`, allowed or refused.

use axum::extract::{OriginalUri, Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::Utc;
use pmos_contract::permissions::Permission;
use serde::Deserialize;

use super::admin::{coded, viewer_coalesced};
use super::users::writes_allowed;
use super::{bearer, check_assertion, client_for, error, unavailable, AppState};
use crate::assertion::{Mode, Refusal};
use crate::audit::{Client, Outcome};
use crate::service::TeamRefusal;
use crate::team::{change_event, PERSONAS_READ};

/// The largest body a team change takes (two lists of at most 16 keys).
pub const MAX_TEAM_BODY: usize = 8 * 1024;

/// Who a call refused before anyone was verified is recorded as.
const ANONYMOUS: &str = "anonymous";

pub(super) fn routes(state: AppState) -> Router<AppState> {
    let catalog =
        Router::new()
            .route("/personas", get(personas))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                identify_reader,
            ));
    let team = Router::new()
        .route("/team/{handle}", get(read_team).put(change_team))
        .layer(middleware::from_fn_with_state(state.clone(), identify_pm));
    let admin = Router::new()
        .route("/admin/personas", get(admin_personas))
        .route(
            "/admin/personas/drafts/{id}/release",
            axum::routing::post(release_held_draft),
        )
        .layer(middleware::from_fn_with_state(state, identify_admin));
    catalog.merge(team).merge(admin)
}

/// The catalog's readers: `pmos-web`, the orchestrator (runtime) and the Slack bot.
async fn identify_reader(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(c @ (Client::Orchestrator | Client::Bot | Client::Web)) => c,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "this token may not use this endpoint",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let mut req = req;
    let assertions = &state.people.assertions;
    if client == Client::Web && assertions.mode() != Mode::Off {
        match check_assertion(assertions, client, req).await {
            Ok((checked, _)) => req = checked,
            Err(refused) => return *refused,
        }
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

/// The handle a `/team/{handle}` path names.
fn path_handle(path: &str) -> Option<&str> {
    let path = path.strip_prefix("/internal").unwrap_or(path);
    let rest = path.strip_prefix("/team/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

/// `pmos-web` only, its assertion for the path's person required in every mode; a change only
/// in `require`.
async fn identify_pm(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let uri = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.clone())
        .unwrap_or_else(|| req.uri().clone());
    let write = req.method() != Method::GET;
    let handle = path_handle(uri.path()).unwrap_or_default().to_string();
    let refused = |state: AppState, code: &'static str| async move {
        if write {
            state
                .svc
                .audit(change_event(
                    Client::Web,
                    ANONYMOUS,
                    Outcome::Denied,
                    code.to_string(),
                    Utc::now(),
                ))
                .await;
        }
    };
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Web) => Client::Web,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web, acting for a signed-in person, may use these routes",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let assertions = &state.people.assertions;
    if write && !writes_allowed(assertions) {
        refused(state.clone(), "assertion_required").await;
        tracing::warn!(
            "a team change was refused: the tool server isn't running in assertion require"
        );
        return coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "team changes need signed user assertions (PMOS_ASSERTION_MODE=require); this tool server runs without them",
        );
    }
    // Verified against the exact request, body included.
    let Some(header) = req.headers().get(pmos_assertion::HEADER).cloned() else {
        refused(state.clone(), "bad_assertion").await;
        return *super::refuse_assertion(client, Refusal::Missing);
    };
    let Ok(header) = header.to_str() else {
        refused(state.clone(), "bad_assertion").await;
        return *super::refuse_assertion(client, Refusal::Header);
    };
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_TEAM_BODY).await else {
        refused(state.clone(), "bad_assertion").await;
        return *super::refuse_assertion(client, Refusal::Body);
    };
    let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let who = match assertions.verify(
        client.as_str(),
        header,
        parts.method.as_str(),
        target,
        &bytes,
        pmos_assertion::unix_now(),
    ) {
        Ok(who) => who,
        Err(why) => {
            refused(state.clone(), "bad_assertion").await;
            return *super::refuse_assertion(client, why);
        }
    };
    if who.subject() != handle {
        tracing::info!("a team call named someone other than its signed person");
        refused(state.clone(), "forbidden").await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a person reads and changes only their own team",
        );
    }
    let mut req = Request::from_parts(parts, axum::body::Body::from(bytes));
    req.extensions_mut().insert(client);
    next.run(req).await
}

/// `pmos-web` only, checked as the other admin reads are.
async fn identify_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Web) => Client::Web,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web, acting for a signed-in person, may use this route",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let mut req = req;
    let assertions = &state.people.assertions;
    if assertions.mode() != Mode::Off {
        match check_assertion(assertions, client, req).await {
            Ok((checked, _)) => req = checked,
            Err(refused) => return *refused,
        }
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

/// A refusal as the wire answers it.
fn refusal(e: TeamRefusal) -> Response {
    let status = match &e {
        TeamRefusal::NotAMember | TeamRefusal::NotAllowed => StatusCode::FORBIDDEN,
        TeamRefusal::BadRequest(_) => StatusCode::BAD_REQUEST,
        TeamRefusal::AlwaysOn | TeamRefusal::Unavailable | TeamRefusal::Full => {
            StatusCode::CONFLICT
        }
        TeamRefusal::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        TeamRefusal::Store(err) => return unavailable(err.clone()),
    };
    coded(status, e.code(), &e.to_string())
}

/// `GET /internal/personas`.
async fn personas(State(state): State<AppState>) -> Response {
    Json(state.svc.persona_catalog()).into_response()
}

/// `GET /internal/team/{handle}`.
async fn read_team(State(state): State<AppState>, Path(handle): Path<String>) -> Response {
    match state.svc.team(&handle, Utc::now()).await {
        Ok(team) => Json(team).into_response(),
        Err(e) => refusal(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    via: String,
}

/// `PUT /internal/team/{handle}`.
async fn change_team(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(handle): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(b) = serde_json::from_slice::<ChangeBody>(&body) else {
        // Recorded through the rules, so a malformed body counts and is audited like any
        // other refused change.
        let e = state
            .svc
            .change_team(client, &handle, &[], &[], "", Utc::now())
            .await
            .err()
            .unwrap_or(TeamRefusal::BadRequest(String::new()));
        return match e {
            TeamRefusal::BadRequest(_) => coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body is {add?: [key], remove?: [key], via: \"onboarding\" | \"bots_page\" | \"picker\"}",
            ),
            other => refusal(other),
        };
    };
    match state
        .svc
        .change_team(client, &handle, &b.add, &b.remove, &b.via, Utc::now())
        .await
    {
        Ok(team) => Json(team).into_response(),
        Err(e) => refusal(e),
    }
}

#[derive(Deserialize)]
struct ViewerParams {
    user: Option<String>,
}

/// `GET /internal/admin/personas?user=<viewer>`.
async fn admin_personas(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ViewAuditLog, Permission::ManageMembers];
    let window = chrono::Duration::minutes(crate::service::PERSONAS_READ_WINDOW_MINUTES);
    if let Err(refused) = viewer_coalesced(
        &state,
        client,
        q.user.as_deref(),
        PERSONAS_READ,
        &need,
        window,
    )
    .await
    {
        return *refused;
    }
    match state.svc.admin_personas(Utc::now()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => unavailable(e),
    }
}

/// `POST /internal/admin/personas/drafts/{id}/release?user=<admin>`: an Admin releases a draft
/// held because its bot was withdrawn (`docs/bot-marketplace-spec.md` §6, "Removal"). A write:
/// the tool server must run in assertion `require` (`503 assertion_required` otherwise), and
/// the assertion's subject is `user` (checked by [`identify_admin`]). `200` the item; `403
/// forbidden` (not an active Admin), `404 not_found`, `409 not_held`.
async fn release_held_draft(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
    Query(q): Query<ViewerParams>,
) -> Response {
    if !writes_allowed(&state.people.assertions) {
        return coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "releasing a held draft needs signed user assertions (PMOS_ASSERTION_MODE=require); this tool server runs without them",
        );
    }
    let Some(admin) = q.user.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "`user` (the admin) is required",
        );
    };
    use crate::service::ReleaseRefusal;
    match state
        .svc
        .release_held_draft(client, admin, &id, Utc::now())
        .await
    {
        Ok(item) => Json(state.svc.approval_json(&item).await).into_response(),
        Err(ReleaseRefusal::Store(e)) => unavailable(e),
        Err(e) => {
            let status = match e {
                ReleaseRefusal::Forbidden | ReleaseRefusal::OwnDraft => StatusCode::FORBIDDEN,
                ReleaseRefusal::NotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::CONFLICT,
            };
            coded(status, e.code(), &e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_names_one_handle() {
        assert_eq!(path_handle("/internal/team/ana"), Some("ana"));
        assert_eq!(path_handle("/team/ana/"), Some("ana"));
        assert_eq!(path_handle("/internal/team/"), None);
        assert_eq!(path_handle("/internal/team/ana/x"), None);
        assert_eq!(path_handle("/internal/personas"), None);
    }

    #[test]
    fn the_body_is_strict() {
        assert!(serde_json::from_str::<ChangeBody>(r#"{"add":["chief"],"via":"picker"}"#).is_ok());
        assert!(serde_json::from_str::<ChangeBody>(
            r#"{"add":["chief"],"via":"picker","routines":"nudge"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ChangeBody>(r#"{"add":["chief"]}"#).is_err());
    }
}
