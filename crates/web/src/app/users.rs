//! The Team screen's routes (`docs/web-api.md`, "Admin: users and roles"; slice U5a):
//! `/api/admin/users…` and `/api/admin/reviewers/{reviewer}`, mirroring the tool server's Admin
//! API (`GET|POST /internal/admin/users`, `…/{handle}/roles`, `…/fields`,
//! `…/sign-in-name/confirm`, `…/deactivate`, `…/reactivate`, `…/identities/{kind}`,
//! `/internal/admin/reviewers/{reviewer}`).
//!
//! **The tool server decides.** Every call names the signed-in person as `user=` and carries a
//! user assertion for them, bound to the exact request (body included); the tool server checks
//! them in its own tables and applies every rule (self, last admin, host only, the cap,
//! `expected_version`, …). What `pmos-web` checks first is a convenience: a list needs
//! `manage-members` or `view-audit-log`, a write `manage-members`, so a person without them gets
//! `403 forbidden` with no upstream call. Writes also pass the origin and CSRF checks every
//! `pmos-web` write does.
//!
//! **Answers are re-picked field by field** ([`pick_user`]): nothing the tool server might add
//! reaches the browser unless it is in the contract, and the three address fields only for a
//! viewer with `manage-members` (the tool server already leaves them out for an auditor).
//! **Refusals keep the tool server's code** so the app can word them; the sentence is
//! `pmos-web`'s own ([`USER_CODES`]), never the tool server's text.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use pmos_contract::permissions::Permission;
use serde_json::{json, Map, Value};

use super::{ApiError, ApiResult, AppState, Signed};
use crate::upstream::UpstreamError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(list).post(create))
        .route("/admin/users/{handle}/roles", put(set_roles))
        .route("/admin/users/{handle}/fields", put(set_fields))
        .route(
            "/admin/users/{handle}/sign-in-name/confirm",
            post(confirm_sign_in_name),
        )
        .route("/admin/users/{handle}/deactivate", post(deactivate))
        .route("/admin/users/{handle}/reactivate", post(reactivate))
        .route("/admin/users/{handle}/identities/{kind}", delete(unlink))
        .route("/admin/reviewers/{reviewer}", put(assign_reviewer))
}

/// The refusals the tool server's Admin API answers with, the status `pmos-web` relays them
/// with, and `pmos-web`'s own sentence. Any other code is `502 upstream_unavailable`.
pub const USER_CODES: [(&str, u16, &str); 21] = [
    ("bad_request", 400, "That request wasn't in a shape PM OS takes. Nothing changed."),
    ("invalid", 422, "One of those values isn't valid. Nothing changed."),
    ("not_found", 404, "There's no such person in PM OS. Nothing changed."),
    ("forbidden", 403, "Only an active admin can change people in PM OS. Nothing changed."),
    ("host_only", 403, "That change needs someone with host access (pmos-admin users). Nothing changed."),
    ("self_change", 403, "Nobody changes their own roles, status or addresses. Ask another admin. Nothing changed."),
    ("exists", 409, "That handle is already taken; handles are never reused. Nothing changed."),
    ("taken", 409, "That address or account belongs to someone else in PM OS. Nothing changed."),
    ("unchanged", 409, "That's already how it is. Nothing changed."),
    ("stale", 409, "Someone changed the team since this page loaded. Reload it and try again. Nothing changed."),
    ("last_admin", 409, "PM OS needs at least one active admin. Nothing changed."),
    ("dual_role", 409, "That would leave someone holding Admin and Auditor without the pilot exception. Nothing changed."),
    ("evals_account", 409, "The evals account holds pm only and is never linked. Nothing changed."),
    ("not_active", 409, "That person is deactivated. Nothing changed."),
    ("linked", 409, "That account is linked, so its account id decides. Unlink it first. Nothing changed."),
    ("owns_bots", 409, "That person owns team bots. Give them another owner first. Nothing changed."),
    ("not_reviewer", 409, "That person doesn't hold the reviewer role. Nothing changed."),
    ("not_pm", 409, "Reviewers are assigned PMs only, and one of those doesn't hold the pm role. Nothing changed."),
    ("deactivation_limit", 429, "PM OS has stopped deactivations for an hour. Someone with host access can continue. Nothing changed."),
    ("assertion_required", 503, "Changing people needs signed requests, and this PM OS isn't running with them. Nothing changed."),
    ("unavailable", 503, "PM OS storage isn't answering. Nothing changed. Try again in a minute."),
];

/// Every field of a user the contract has, but the addresses.
pub const USER_FIELDS: [&str; 24] = [
    "handle",
    "display_name",
    "status",
    "roles",
    "created_at",
    "deactivated_at",
    "grace_ends_at",
    "dual_role",
    "role_conflict",
    "has_sign_in_name",
    "sign_in_name_confirmed",
    "sign_in_unconfirmed",
    "entra",
    "entra_needs_host",
    "slack_linked",
    "entra_linked",
    "atlassian_linked",
    "github_linked",
    "accounts",
    "host_only",
    "bot",
    "team_bots",
    "reviews",
    "reviewed_by",
];

/// The addresses: only for a viewer with `manage-members`, and only when the tool server sent
/// them.
pub const ADDRESS_FIELDS: [&str; 3] = ["sign_in_name", "atlassian_email", "github_login"];

/// The account kinds in `accounts`.
const ACCOUNT_KINDS: [&str; 3] = ["atlassian", "slack", "github"];

fn copy(from: &Value, keys: &[&str]) -> Value {
    let mut out = Map::new();
    for k in keys {
        if let Some(v) = from.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// One user as the browser gets it: [`USER_FIELDS`], and [`ADDRESS_FIELDS`] when `addresses`.
pub fn pick_user(u: &Value, addresses: bool) -> Value {
    let mut out = copy(u, &USER_FIELDS);
    if let Some(a) = u.get("accounts").filter(|a| a.is_object()) {
        out["accounts"] = copy(a, &ACCOUNT_KINDS);
    }
    if addresses {
        for k in ADDRESS_FIELDS {
            if let Some(v) = u.get(k) {
                out[k] = v.clone();
            }
        }
    }
    out
}

fn bad_reply() -> ApiError {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
}

/// A tool server refusal, as `pmos-web` answers it: the same code, its own sentence.
fn refused(status: u16, body: &Value) -> ApiError {
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    match USER_CODES.iter().find(|(c, ..)| *c == code) {
        Some((code, status, sentence)) => ApiError::new(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::CONFLICT),
            code,
            *sentence,
        ),
        None => {
            tracing::warn!(
                status,
                "the tool server refused a user call with an unknown code"
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

/// A call to the tool server's Admin API, for the signed-in person.
async fn call(
    state: &AppState,
    signed: &Signed,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<&Value>,
) -> Result<(u16, Value), ApiError> {
    let write = method != reqwest::Method::GET;
    match signed
        .toolserver(state)
        .admin_users(signed.handle(), method, segments, body)
        .await
    {
        Ok((status, v)) if (200..300).contains(&status) => Ok((status, v)),
        Ok((status, v)) => Err(refused(status, &v)),
        // Sent, but the answer was lost: the change may have been made.
        Err(UpstreamError::Unknown { why, .. }) if write => {
            tracing::warn!(why = %why, "a user write's answer was lost");
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "unknown_outcome",
                "PM OS can't tell whether that change was made. Reload the team list to see.",
            ))
        }
        Err(e) => Err(ApiError::upstream(e)),
    }
}

fn need(signed: &Signed, any_of: &[Permission]) -> Result<(), ApiError> {
    if any_of.iter().any(|p| signed.member.allows(*p)) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only an admin can change people in PM OS.",
        ))
    }
}

/// A handle from the URL: what a handle may be, else nothing is there.
fn handle_param(h: &str) -> Result<&str, ApiError> {
    if pmos_contract::model::valid_handle(h) {
        Ok(h)
    } else {
        Err(ApiError::not_found())
    }
}

/// A write's body: a JSON object (or nothing, where `empty_ok`).
fn body_object(bytes: &Bytes, empty_ok: bool) -> Result<Option<Value>, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return if empty_ok {
            Ok(None)
        } else {
            Err(ApiError::bad_request("This route takes a JSON object."))
        };
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v @ Value::Object(_)) => Ok(Some(v)),
        _ => Err(ApiError::bad_request("This route takes a JSON object.")),
    }
}

/// `{version, user, note?}` after a write, re-picked.
fn written(status: u16, v: &Value) -> ApiResult {
    if !v.get("version").is_some_and(Value::is_i64) {
        return Err(bad_reply());
    }
    let mut out = json!({
        "version": v["version"],
        "user": v.get("user").filter(|u| u.is_object()).map(|u| pick_user(u, true)),
    });
    if let Some(n) = v.get("note").filter(|n| n.is_string()) {
        out["note"] = n.clone();
    }
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    Ok((status, Json(out)).into_response())
}

fn write_log(signed: &Signed, route: &str) {
    tracing::info!(user = %signed.member.handle, route, "admin user write");
}

/// `GET /api/admin/users`: every user, as the viewer's role may see them.
async fn list(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(
        &signed,
        &[Permission::ManageMembers, Permission::ViewAuditLog],
    )
    .map_err(|_| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only an admin or an auditor can see this.",
        )
    })?;
    tracing::info!(user = %signed.member.handle, route = "/api/admin/users", "admin read");
    let (_, v) = call(&state, &signed, reqwest::Method::GET, &["users"], None).await?;
    Ok(Json(pick_list(
        &v,
        signed.member.allows(Permission::ManageMembers),
    )?)
    .into_response())
}

/// The list's answer, re-picked.
pub fn pick_list(v: &Value, addresses: bool) -> Result<Value, ApiError> {
    let (Some(version), Some(users)) = (
        v.get("version").filter(|x| x.is_i64()),
        v.get("users").and_then(Value::as_array),
    ) else {
        return Err(bad_reply());
    };
    let view = match v.get("view").and_then(Value::as_str) {
        Some("admin") if addresses => "admin",
        _ => "auditor",
    };
    Ok(json!({
        "version": version,
        "view": view,
        "users": users.iter().map(|u| pick_user(u, view == "admin")).collect::<Vec<_>>(),
        "deactivations": v.get("deactivations").map(|d| copy(d, &["last_hour", "limit", "remaining", "grace_hours"])),
        "pilot_exception": v.get("pilot_exception").filter(|e| e.is_object()).map(|e| copy(e, &["ends", "ends_at", "ending_soon", "lapsed"])),
        "linking": v.get("linking").and_then(Value::as_bool),
    }))
}

/// The list for the legacy `GET /api/admin/team` (`app/admin.rs`), read the same way.
pub(super) async fn list_for_team(state: &AppState, signed: &Signed) -> Result<Value, ApiError> {
    let (_, v) = call(state, signed, reqwest::Method::GET, &["users"], None).await?;
    pick_list(&v, false)
}

async fn write(
    state: &AppState,
    signed: &Signed,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<Value>,
    route: &str,
) -> ApiResult {
    need(signed, &[Permission::ManageMembers])?;
    write_log(signed, route);
    let (status, v) = call(state, signed, method, segments, body.as_ref()).await?;
    written(status, &v)
}

/// `POST /api/admin/users`: invite.
async fn create(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    let body = body_object(&body, false)?;
    write(
        &state,
        &signed,
        reqwest::Method::POST,
        &["users"],
        body,
        "/api/admin/users",
    )
    .await
}

async fn set_roles(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    let body = body_object(&body, false)?;
    write(
        &state,
        &signed,
        reqwest::Method::PUT,
        &["users", handle, "roles"],
        body,
        "/api/admin/users/{handle}/roles",
    )
    .await
}

async fn set_fields(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    let body = body_object(&body, false)?;
    write(
        &state,
        &signed,
        reqwest::Method::PUT,
        &["users", handle, "fields"],
        body,
        "/api/admin/users/{handle}/fields",
    )
    .await
}

async fn confirm_sign_in_name(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    let body = body_object(&body, false)?;
    write(
        &state,
        &signed,
        reqwest::Method::POST,
        &["users", handle, "sign-in-name", "confirm"],
        body,
        "/api/admin/users/{handle}/sign-in-name/confirm",
    )
    .await
}

async fn deactivate(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    let body = body_object(&body, true)?;
    write(
        &state,
        &signed,
        reqwest::Method::POST,
        &["users", handle, "deactivate"],
        body,
        "/api/admin/users/{handle}/deactivate",
    )
    .await
}

async fn reactivate(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
    body: Bytes,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    let body = body_object(&body, true)?;
    write(
        &state,
        &signed,
        reqwest::Method::POST,
        &["users", handle, "reactivate"],
        body,
        "/api/admin/users/{handle}/reactivate",
    )
    .await
}

async fn unlink(
    State(state): State<AppState>,
    signed: Signed,
    Path((handle, kind)): Path<(String, String)>,
) -> ApiResult {
    let handle = handle_param(&handle)?;
    if !matches!(kind.as_str(), "entra" | "atlassian" | "github") {
        return Err(ApiError::not_found());
    }
    write(
        &state,
        &signed,
        reqwest::Method::DELETE,
        &["users", handle, "identities", &kind],
        None,
        "/api/admin/users/{handle}/identities/{kind}",
    )
    .await
}

async fn assign_reviewer(
    State(state): State<AppState>,
    signed: Signed,
    Path(reviewer): Path<String>,
    body: Bytes,
) -> ApiResult {
    let reviewer = handle_param(&reviewer)?;
    let body = body_object(&body, false)?;
    write(
        &state,
        &signed,
        reqwest::Method::PUT,
        &["reviewers", reviewer],
        body,
        "/api/admin/reviewers/{reviewer}",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_is_re_picked_and_addresses_go_to_admins_only() {
        let u = json!({"handle": "ana", "display_name": "Ana", "roles": ["pm"],
                       "sign_in_name": "ana@example.test", "oid": "x", "slack_id": "U0ANA",
                       "accounts": {"atlassian": true, "slack": false, "github": false, "token": "t"}});
        let admin = pick_user(&u, true);
        assert_eq!(admin["sign_in_name"], "ana@example.test");
        assert!(admin.get("oid").is_none() && admin.get("slack_id").is_none());
        assert!(admin["accounts"].get("token").is_none());
        let auditor = pick_user(&u, false);
        assert!(auditor.get("sign_in_name").is_none());
        assert_eq!(auditor["handle"], "ana");
    }

    #[test]
    fn every_relayed_code_has_a_sentence() {
        for (code, status, sentence) in USER_CODES {
            assert!(sentence.len() > 10, "{code}");
            assert!(StatusCode::from_u16(status).is_ok(), "{code}");
            assert!(!sentence.contains('—'), "{code}");
        }
    }
}
