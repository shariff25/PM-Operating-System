//! The user database's Admin API (`docs/user-database-spec.md` §6, §8; slice U5), and the
//! console's admin listing.
//!
//! | Route | Token | Does |
//! | --- | --- | --- |
//! | `GET /internal/admin/users?user=<viewer>` | `pmos-web` | Every user, as an Admin (`manage-members`) or an Auditor (`view-audit-log`) may see them ([`user_json`]). |
//! | `POST /internal/admin/users?user=<admin>` | `pmos-web` | Invite: create an active user, sign-in name confirmed; never `admin`. |
//! | `PUT /internal/admin/users/{handle}/roles?user=<admin>` | `pmos-web` | The whole role set, with `expected_version`. |
//! | `PUT /internal/admin/users/{handle}/fields?user=<admin>` | `pmos-web` | Sign-in name, Atlassian email, GitHub login, display name, with `expected_version`. |
//! | `POST /internal/admin/users/{handle}/sign-in-name/confirm?user=<admin>` | `pmos-web` | Confirm (or replace and confirm) the sign-in name (§4). |
//! | `POST /internal/admin/users/{handle}/deactivate`, `/reactivate` `?user=<admin>` | `pmos-web` | §6 "Deactivation"; capped, never restores `admin`. |
//! | `DELETE /internal/admin/users/{handle}/identities/{kind}?user=<admin>` | `pmos-web` | Unlink `entra`, `atlassian` or `github`. |
//! | `PUT /internal/admin/reviewers/{reviewer}?user=<admin>` | `pmos-web` | The PMs assigned to a reviewer (§8). |
//! | `GET /internal/console/members?user=<admin>` | console | Handles of the active users who may hold drafts, for an admin's Approvals screen. |
//!
//! **Only `pmos-web`'s token reaches the Admin API**: the orchestrator's, the console's and the
//! Slack bot's get `403` (the Slack bot keeps its own `pm`-only join approval and capped
//! `remove`, `crate::onboarding`). Every write:
//!
//! - **refuses to run on the development override**: unless the service runs in assertion
//!   `require` (and so not with `PMOS_ASSERTION_INSECURE_OFF=1`), it answers
//!   `503 assertion_required` before anything else;
//! - **needs `pmos-web`'s signed user assertion** bound to the request, body included, whose
//!   subject is the `user=` it names (the actor), else `401 bad_assertion`;
//! - **checks the actor in the tool server's own tables** (an active admin), never the
//!   assertion's roles; the rules are `crate::users::rules`, the same ones the host commands
//!   and the Slack client go through;
//! - **is audited whether allowed or refused**: the store writes `role_changes` and the audit
//!   event in the write's transaction, and a `denied` event with the refusal's code when it
//!   refuses; a refusal before the store (a body that doesn't parse, the override, a bad
//!   assertion, a team bot still owned) is recorded here. Never an address, an `oid` or a token.
//!
//! **What an Auditor sees** in the list: handles, display names, roles, status, flags and
//! booleans. Never an address (sign-in name, Atlassian email, GitHub login), an `oid` or a Slack
//! id: those three fields are left out of an auditor's answer, not nulled. Nobody gets an `oid`,
//! a Slack id or another account id here.
//!
//! **The console's listing** is as narrow as its need: handles only, of active users who may use
//! PM OS (`pm`, `reviewer` or `admin`), for a viewer who is an active admin in the database, with
//! the console's user assertion for that viewer as on its approvals reads.

use std::collections::BTreeSet;

use axum::extract::{OriginalUri, Path, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use pmos_contract::onboarding::personal_bot_name;
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::admin::{coded, require_assertion, viewer, EXCEPTION_WARN_DAYS};
use super::{bearer, check_assertion, client_for, error, AppState};
use crate::assertion::Mode;
use crate::audit::{AuditEvent, Client, Outcome};
use crate::store::StoreError;
use crate::users::access::{may_use, permits};
use crate::users::{
    credential_grace, Actor, Directory, Field, IdentityKind, NewUser, Op, Refusal, User, UserError,
    UserRole,
};
use crate::vault::Provider;

/// The largest body a user write takes.
pub const MAX_USER_BODY: usize = 16 * 1024;

/// The run id user writes are recorded under (as the store records them).
const USERS_RUN: &str = "users";

/// Who a write refused before anyone was verified is recorded as.
const ANONYMOUS: &str = "anonymous";

pub(super) fn routes(state: AppState) -> Router<AppState> {
    let admin = Router::new()
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            identify_web_admin,
        ));
    let console = Router::new()
        .route("/console/members", get(console_members))
        .layer(middleware::from_fn_with_state(state, identify_console));
    admin.merge(console)
}

// --- Who may call ----------------------------------------------------------------------------

/// The audit action a write route records, and its target, from the request's method and path
/// (for refusals made before the store sees the write).
fn write_action(method: &Method, path: &str) -> (&'static str, String) {
    let path = path.strip_prefix("/internal").unwrap_or(path);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let target = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
    let action = match (method, parts.as_slice()) {
        (&Method::POST, ["admin", "users"]) => "users.create",
        (_, ["admin", "users", _, "roles"]) => "users.roles",
        (_, ["admin", "users", _, "fields"]) | (_, ["admin", "users", _, "sign-in-name", _]) => {
            "users.fields"
        }
        (_, ["admin", "users", _, "deactivate"]) => "users.deactivate",
        (_, ["admin", "users", _, "reactivate"]) => "users.reactivate",
        (_, ["admin", "users", _, "identities", _]) => "users.identity_unlink",
        (_, ["admin", "reviewers", _]) => "reviewers.assign",
        _ => "users.write",
    };
    let target = if action == "users.create" {
        "*".to_string()
    } else {
        target
    };
    (action, target)
}

/// A `denied` audit event for a write refused before the store saw it.
async fn record_refused(state: &AppState, actor: &str, action: &str, target: &str, code: &str) {
    state
        .svc
        .audit(AuditEvent {
            at: Utc::now(),
            client: Client::Web,
            run_id: USERS_RUN.to_string(),
            user: actor.to_string(),
            skill: None,
            bot: None,
            tool: action.to_string(),
            outcome: Outcome::Denied,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: (!target.is_empty()).then(|| target.to_string()),
            detail: Some(code.to_string()),
        })
        .await;
}

/// Writes run only in assertion `require`: never on the development override (§6).
pub fn writes_allowed(assertions: &crate::assertion::Assertions) -> bool {
    assertions.mode() == Mode::Require && assertions.insecure_warning().is_none()
}

/// `pmos-web` only. A read is checked as the other admin reads are (its assertion, by
/// `PMOS_ASSERTION_MODE`); a write needs `require` and a valid assertion for the actor.
async fn identify_web_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Web) => Client::Web,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web, acting for a signed-in admin, may use these routes",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let assertions = &state.people.assertions;
    let mut req = req;
    if req.method() == Method::GET {
        if assertions.mode() != Mode::Off {
            match check_assertion(assertions, client, req).await {
                Ok((checked, _)) => req = checked,
                Err(refused) => return *refused,
            }
        }
    } else {
        let path = req
            .extensions()
            .get::<OriginalUri>()
            .map(|u| u.0.path().to_string())
            .unwrap_or_else(|| req.uri().path().to_string());
        let (action, target) = write_action(req.method(), &path);
        if !writes_allowed(assertions) {
            record_refused(&state, ANONYMOUS, action, &target, "assertion_required").await;
            tracing::warn!(
                action,
                "user write refused: the tool server isn't running in assertion require"
            );
            return coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "assertion_required",
                "user and role changes need signed user assertions (PMOS_ASSERTION_MODE=require); this tool server runs without them",
            );
        }
        match require_assertion(assertions, client, req, MAX_USER_BODY).await {
            Ok(checked) => req = checked,
            Err(refused) => {
                record_refused(&state, ANONYMOUS, action, &target, "bad_assertion").await;
                return *refused;
            }
        }
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

/// The console only, its assertion checked as on its approvals reads.
async fn identify_console(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Console) => Client::Console,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "this token may not use this endpoint",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let assertions = &state.people.assertions;
    let mut req = req;
    if assertions.mode() != Mode::Off {
        match check_assertion(assertions, client, req).await {
            Ok((checked, _)) => req = checked,
            Err(refused) => return *refused,
        }
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

// --- Answers ---------------------------------------------------------------------------------

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn store_down(e: StoreError) -> Response {
    tracing::error!(error = %e, "user database unavailable");
    coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "PM OS storage is unavailable",
    )
}

/// The HTTP status of a refused user write.
pub fn refusal_status(r: Refusal) -> StatusCode {
    match r {
        Refusal::Invalid | Refusal::ImportRefused => StatusCode::UNPROCESSABLE_ENTITY,
        Refusal::NotFound => StatusCode::NOT_FOUND,
        Refusal::Forbidden
        | Refusal::HostOnly
        | Refusal::SelfChange
        | Refusal::AdminNotFromSlack
        | Refusal::RoleNotFromSlack
        | Refusal::ProtectedNotFromSlack
        | Refusal::NotFromSlack => StatusCode::FORBIDDEN,
        Refusal::DeactivationLimit => StatusCode::TOO_MANY_REQUESTS,
        Refusal::Isolation => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::CONFLICT,
    }
}

/// A refused write, `{code, error}`; a storage failure, `503 unavailable`. The refusal's
/// message names handles and fields, never an address's value.
fn user_error(e: UserError) -> Response {
    match e {
        UserError::Refused { refusal, message } => {
            tracing::info!(code = refusal.code(), "user write refused");
            let code = match refusal {
                Refusal::Isolation => "unavailable",
                r => r.code(),
            };
            no_store(coded(refusal_status(refusal), code, &message))
        }
        UserError::Store(e) => store_down(e),
    }
}

fn bad_request(why: &str) -> Response {
    coded(StatusCode::BAD_REQUEST, "bad_request", why)
}

/// The `user=` a call names: the viewer of a read, the actor of a write (its assertion's
/// subject, already checked).
#[derive(Deserialize)]
struct Named {
    user: Option<String>,
}

fn named(q: &Named) -> Option<&str> {
    q.user.as_deref().map(str::trim).filter(|u| !u.is_empty())
}

// --- The list --------------------------------------------------------------------------------

/// When a deactivated person's linked accounts are deleted and held drafts closed, unless
/// they are reactivated first.
fn grace_ends(u: &User) -> Option<DateTime<Utc>> {
    u.deactivated_at.map(|at| at + credential_grace())
}

/// One user as the admin screens see them. `addresses`: an Admin's view (the sign-in name,
/// Atlassian email and GitHub login); an Auditor's never has them.
pub fn user_json(
    u: &User,
    dir: &Directory,
    accounts: Option<&[Provider]>,
    team_bots: Vec<String>,
    now: DateTime<Utc>,
    addresses: bool,
) -> Value {
    let linked = |k: IdentityKind| u.identity(k).is_some();
    let in_force = dir.exception_in_force(now);
    let mut v = json!({
        "handle": u.handle,
        "display_name": u.display_name,
        "status": u.status.as_str(),
        "roles": u.role_set().iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        "created_at": u.created_at,
        "deactivated_at": u.deactivated_at,
        "grace_ends_at": grace_ends(u),
        "dual_role": u.dual_role(),
        "role_conflict": u.active() && u.dual_role() && !in_force,
        "has_sign_in_name": u.sign_in_name.is_some(),
        "sign_in_name_confirmed": u.sign_in_name_confirmed_at.is_some(),
        "sign_in_unconfirmed": u.active() && u.sign_in_name.is_some() && u.sign_in_name_confirmed_at.is_none(),
        "entra": u.entra_state(),
        "entra_needs_host": u.entra_state() == "needs_host",
        "slack_linked": linked(IdentityKind::Slack),
        "entra_linked": linked(IdentityKind::Entra),
        "atlassian_linked": linked(IdentityKind::Atlassian),
        "github_linked": linked(IdentityKind::Github),
        "accounts": accounts.map(|a| json!({
            "atlassian": a.contains(&Provider::Atlassian),
            "slack": a.contains(&Provider::Slack),
            "github": a.contains(&Provider::Github),
        })),
        "host_only": u.protected(),
        "bot": personal_bot_name(&u.handle),
        "team_bots": team_bots,
        "reviews": dir.reviews(&u.handle),
        "reviewed_by": dir.reviewers_of(&u.handle),
    });
    if addresses {
        v["sign_in_name"] = json!(u.sign_in_name);
        v["atlassian_email"] = json!(u.atlassian_email);
        v["github_login"] = json!(u.github_login);
    }
    v
}

/// What the Team page shows about a person's password sign-in (docs/password-sign-in.md):
/// three booleans, never anything secret, for an Admin's and an Auditor's view alike.
pub fn with_password(v: &mut Value, s: crate::password::store::PasswordStatus) {
    v["password_set"] = json!(s.password_set);
    v["second_step_enrolled"] = json!(s.second_step_enrolled);
    v["setup_code_pending"] = json!(s.setup_code_pending);
}

/// The cap and grace the screen shows (§6).
fn limits(state: &AppState, dir: &Directory) -> Value {
    let cap = state.svc.stores().users.rules().deactivations_per_hour as usize;
    let recent = dir.capped_deactivations.len();
    json!({
        "last_hour": recent,
        "limit": cap,
        "remaining": cap.saturating_sub(recent),
        "grace_hours": crate::users::CREDENTIAL_GRACE_HOURS,
    })
}

fn exception_json(dir: &Directory, now: DateTime<Utc>) -> Value {
    let in_force = dir.exception_in_force(now);
    match &dir.exception {
        None => Value::Null,
        Some(e) => json!({
            "ends": crate::users::detail::date(e.expires_at),
            "ends_at": e.expires_at,
            "ending_soon": in_force && e.expires_at - now <= chrono::Duration::days(EXCEPTION_WARN_DAYS),
            "lapsed": !in_force,
        }),
    }
}

/// Each user's linked accounts (credentials), as providers; `None` when they can't be read.
async fn accounts_of(state: &AppState, handle: &str) -> Result<Vec<Provider>, StoreError> {
    Ok(state
        .svc
        .stores()
        .credentials
        .list(handle)
        .await?
        .into_iter()
        .map(|c| c.provider)
        .collect())
}

/// `GET /internal/admin/users?user=<viewer>`.
async fn list(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<Named>,
) -> Response {
    let need = [Permission::ManageMembers, Permission::ViewAuditLog];
    let who = match viewer(&state, client, named(&q), "admin.users", &need).await {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let dir = match state.svc.people().directory().await {
        Ok(d) => d,
        Err(e) => return store_down(e),
    };
    let admin_view = dir
        .get(&who)
        .is_some_and(|u| permits(u, Permission::ManageMembers));
    let now = Utc::now();
    let passwords = match state.svc.stores().users.passwords().password_statuses(now).await {
        Ok(p) => p,
        Err(e) => return store_down(e),
    };
    let mut users = Vec::new();
    for u in dir.users.values() {
        let accounts = match accounts_of(&state, &u.handle).await {
            Ok(a) => a,
            Err(e) => return store_down(e),
        };
        let bots = crate::onboarding::owned_team_bots(&state.svc, &u.handle);
        let mut v = user_json(u, &dir, Some(&accounts), bots, now, admin_view);
        with_password(&mut v, passwords.get(&u.handle).copied().unwrap_or_default());
        users.push(v);
    }
    no_store(
        Json(json!({
            "version": dir.version,
            "view": if admin_view { "admin" } else { "auditor" },
            "users": users,
            "deactivations": limits(&state, &dir),
            "pilot_exception": exception_json(&dir, now),
            "linking": state.oauth.is_some(),
        }))
        .into_response(),
    )
}

/// After a write: the target as an admin sees them now, and the directory's new version.
async fn written(
    state: &AppState,
    handle: &str,
    status: StatusCode,
    note: Option<String>,
) -> Response {
    let dir = match state.svc.people().directory().await {
        Ok(d) => d,
        Err(e) => return store_down(e),
    };
    let user = match dir.get(handle) {
        Some(u) => {
            let accounts = accounts_of(state, handle).await.ok();
            let bots = crate::onboarding::owned_team_bots(&state.svc, handle);
            let mut v = user_json(u, &dir, accounts.as_deref(), bots, Utc::now(), true);
            let status = state
                .svc
                .stores()
                .users
                .passwords()
                .password_status(handle, Utc::now())
                .await;
            match status {
                Ok(s) => with_password(&mut v, s),
                Err(e) => return store_down(e),
            }
            v
        }
        None => Value::Null,
    };
    let mut body = json!({ "version": dir.version, "user": user });
    if let Some(n) = note {
        body["note"] = json!(n);
    }
    no_store((status, Json(body)).into_response())
}

// --- Writes ----------------------------------------------------------------------------------

/// The actor (`user=`, the assertion's subject) and the parsed body, or the refusal, recorded.
async fn parse_write<T: serde::de::DeserializeOwned + Default>(
    state: &AppState,
    q: &Named,
    body: &[u8],
    action: &str,
    target: &str,
    empty_ok: bool,
) -> Result<(String, T), Box<Response>> {
    let Some(actor) = named(q) else {
        record_refused(state, ANONYMOUS, action, target, "bad_request").await;
        return Err(Box::new(bad_request("`user` (the admin) is required")));
    };
    let empty = body.iter().all(u8::is_ascii_whitespace);
    let parsed = if empty && empty_ok {
        Ok(T::default())
    } else {
        serde_json::from_slice::<T>(body)
    };
    match parsed {
        Ok(v) => Ok((actor.to_string(), v)),
        Err(_) => {
            record_refused(state, actor, action, target, "bad_request").await;
            Err(Box::new(bad_request(
                "the body isn't the JSON this route takes (see docs/web-api.md)",
            )))
        }
    }
}

fn roles(names: &[String]) -> Result<BTreeSet<UserRole>, String> {
    names
        .iter()
        .map(|r| {
            UserRole::parse(r)
                .ok_or_else(|| format!("`{r}` isn't a role (admin, pm, reviewer, auditor)"))
        })
        .collect()
}

/// A value the body got wrong, recorded as `invalid`.
async fn invalid(state: &AppState, actor: &str, action: &str, target: &str, why: &str) -> Response {
    record_refused(state, actor, action, target, Refusal::Invalid.code()).await;
    coded(StatusCode::UNPROCESSABLE_ENTITY, "invalid", why)
}

async fn apply(
    state: &AppState,
    actor: &str,
    op: Op,
    expected: Option<i64>,
) -> Result<(), Box<Response>> {
    state
        .svc
        .people()
        .apply_expecting(&Actor::web(actor), op, Utc::now(), expected)
        .await
        .map(drop)
        .map_err(|e| Box::new(user_error(e)))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    handle: String,
    #[serde(default)]
    display_name: String,
    sign_in_name: String,
    atlassian_email: String,
    #[serde(default)]
    github_login: Option<String>,
    #[serde(default)]
    slack_id: Option<String>,
    roles: Vec<String>,
}

/// `POST /internal/admin/users?user=<admin>`: invite.
async fn create(
    State(state): State<AppState>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.create";
    let (actor, b) = match parse_write::<CreateBody>(&state, &q, &body, action, "*", false).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let target = b.handle.trim().to_string();
    let roles = match roles(&b.roles) {
        Ok(r) if !r.is_empty() => r,
        Ok(_) => return invalid(&state, &actor, action, &target, "give at least one role").await,
        Err(why) => return invalid(&state, &actor, action, &target, &why).await,
    };
    let op = Op::CreateUser {
        user: NewUser {
            handle: target.clone(),
            display_name: b.display_name,
            sign_in_name: Some(b.sign_in_name),
            atlassian_email: Some(b.atlassian_email),
            github_login: b.github_login.filter(|g| !g.trim().is_empty()),
            slack_id: b.slack_id.filter(|s| !s.trim().is_empty()),
            roles,
        },
    };
    if let Err(r) = apply(&state, &actor, op, None).await {
        return *r;
    }
    tracing::info!(admin = %actor, "user invited");
    written(&state, &target, StatusCode::CREATED, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RolesBody {
    roles: Vec<String>,
    expected_version: i64,
}

/// `PUT /internal/admin/users/{handle}/roles?user=<admin>`: the whole set.
async fn set_roles(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.roles";
    let (actor, b) = match parse_write::<RolesBody>(&state, &q, &body, action, &handle, false).await
    {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let roles = match roles(&b.roles) {
        Ok(r) => r,
        Err(why) => return invalid(&state, &actor, action, &handle, &why).await,
    };
    let op = Op::SetRoles {
        handle: handle.clone(),
        roles,
    };
    if let Err(r) = apply(&state, &actor, op, Some(b.expected_version)).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, "roles changed");
    written(&state, &handle, StatusCode::OK, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FieldsBody {
    #[serde(default)]
    sign_in_name: Option<String>,
    #[serde(default)]
    atlassian_email: Option<String>,
    #[serde(default)]
    github_login: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    expected_version: i64,
}

/// `PUT /internal/admin/users/{handle}/fields?user=<admin>`.
async fn set_fields(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.fields";
    let (actor, b) =
        match parse_write::<FieldsBody>(&state, &q, &body, action, &handle, false).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let values: Vec<(Field, String)> = [
        (Field::SignInName, b.sign_in_name),
        (Field::AtlassianEmail, b.atlassian_email),
        (Field::GithubLogin, b.github_login),
        (Field::DisplayName, b.display_name),
    ]
    .into_iter()
    .filter_map(|(f, v)| v.map(|v| (f, v)))
    .collect();
    let op = Op::SetFields {
        handle: handle.clone(),
        values,
    };
    if let Err(r) = apply(&state, &actor, op, Some(b.expected_version)).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, "fields changed");
    written(&state, &handle, StatusCode::OK, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    sign_in_name: String,
    #[serde(default)]
    expected_version: Option<i64>,
}

/// `POST /internal/admin/users/{handle}/sign-in-name/confirm?user=<admin>` (§4): the stored
/// name confirmed, or replaced and confirmed.
async fn confirm_sign_in_name(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.fields";
    let (actor, b) =
        match parse_write::<ConfirmBody>(&state, &q, &body, action, &handle, false).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let op = Op::SetFields {
        handle: handle.clone(),
        values: vec![(Field::SignInName, b.sign_in_name)],
    };
    if let Err(r) = apply(&state, &actor, op, b.expected_version).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, "sign-in name confirmed");
    written(&state, &handle, StatusCode::OK, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StatusBody {
    #[serde(default)]
    expected_version: Option<i64>,
}

/// `POST /internal/admin/users/{handle}/deactivate?user=<admin>` (§6): capped; refused while
/// the person owns a team bot.
async fn deactivate(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.deactivate";
    let (actor, b) = match parse_write::<StatusBody>(&state, &q, &body, action, &handle, true).await
    {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let owned = crate::onboarding::owned_team_bots(&state.svc, &handle);
    // Only for an admin acting on someone else who is active: anyone else gets the rules'
    // own refusal (not an admin, oneself, already deactivated).
    let admin = matches!(state.svc.people().is_admin(&actor).await, Ok(true));
    if !owned.is_empty() && admin && actor != handle {
        if let Ok(Some(u)) = state.svc.people().user(&handle).await {
            if u.active() {
                record_refused(&state, &actor, action, &handle, "owns_bots").await;
                return coded(
                    StatusCode::CONFLICT,
                    "owns_bots",
                    &format!(
                        "`{handle}` owns {}; give them another owner first",
                        owned.join(", ")
                    ),
                );
            }
        }
    }
    let op = Op::Deactivate {
        handle: handle.clone(),
    };
    if let Err(r) = apply(&state, &actor, op, b.expected_version).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, "user deactivated");
    let note = format!(
        "`{handle}` is deactivated: their sessions ended and their linked accounts are suspended. \
         Reactivating them within {}h restores their accounts and held drafts (never admin). \
         Their personal bot's sandbox keeps idling until `pmos-admin pause {}`.",
        crate::users::CREDENTIAL_GRACE_HOURS,
        personal_bot_name(&handle)
    );
    written(&state, &handle, StatusCode::OK, Some(note)).await
}

/// `POST /internal/admin/users/{handle}/reactivate?user=<admin>` (§6): never restores `admin`.
async fn reactivate(
    State(state): State<AppState>,
    Path(handle): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "users.reactivate";
    let (actor, b) = match parse_write::<StatusBody>(&state, &q, &body, action, &handle, true).await
    {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let op = Op::Reactivate {
        handle: handle.clone(),
    };
    if let Err(r) = apply(&state, &actor, op, b.expected_version).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, "user reactivated");
    written(&state, &handle, StatusCode::OK, None).await
}

/// `DELETE /internal/admin/users/{handle}/identities/{kind}?user=<admin>`: `entra`,
/// `atlassian` or `github`.
async fn unlink(
    State(state): State<AppState>,
    Path((handle, kind)): Path<(String, String)>,
    Query(q): Query<Named>,
) -> Response {
    let action = "users.identity_unlink";
    let Some(actor) = named(&q).map(String::from) else {
        record_refused(&state, ANONYMOUS, action, &handle, "bad_request").await;
        return bad_request("`user` (the admin) is required");
    };
    let kind = match IdentityKind::parse(&kind) {
        Some(k @ (IdentityKind::Entra | IdentityKind::Atlassian | IdentityKind::Github)) => k,
        _ => {
            return invalid(
                &state,
                &actor,
                action,
                &handle,
                "the identity is `entra`, `atlassian` or `github`",
            )
            .await
        }
    };
    let op = Op::Unlink {
        handle: handle.clone(),
        kind,
    };
    if let Err(r) = apply(&state, &actor, op, None).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %handle, kind = kind.as_str(), "identity unlinked");
    written(&state, &handle, StatusCode::OK, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReviewersBody {
    pms: Vec<String>,
    #[serde(default)]
    expected_version: Option<i64>,
}

/// `PUT /internal/admin/reviewers/{reviewer}?user=<admin>` (§8): the whole set of PMs.
async fn assign_reviewer(
    State(state): State<AppState>,
    Path(reviewer): Path<String>,
    Query(q): Query<Named>,
    body: axum::body::Bytes,
) -> Response {
    let action = "reviewers.assign";
    let (actor, b) =
        match parse_write::<ReviewersBody>(&state, &q, &body, action, &reviewer, false).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let pms: BTreeSet<String> = b.pms.iter().map(|p| p.trim().to_string()).collect();
    if pms.len() != b.pms.len() {
        return invalid(&state, &actor, action, &reviewer, "each PM at most once").await;
    }
    let op = Op::AssignReviewer {
        reviewer: reviewer.clone(),
        pms,
    };
    if let Err(r) = apply(&state, &actor, op, b.expected_version).await {
        return *r;
    }
    tracing::info!(admin = %actor, target = %reviewer, "reviewer assignments changed");
    written(&state, &reviewer, StatusCode::OK, None).await
}

// --- The console's listing -------------------------------------------------------------------

/// `GET /internal/console/members?user=<admin>`: `{"handles": [...]}`, the active users who may
/// use PM OS (and so hold drafts), for an active admin's Approvals screen. Recorded as
/// `admin.console_members`, allowed or refused.
async fn console_members(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<Named>,
) -> Response {
    let Some(user) = named(&q) else {
        return bad_request("`user` (the admin) is required");
    };
    let dir = match state.svc.people().directory().await {
        Ok(d) => d,
        Err(e) => return store_down(e),
    };
    let admin = dir
        .get(user)
        .is_some_and(|u| u.active() && u.has(UserRole::Admin));
    let event = |outcome| AuditEvent {
        at: Utc::now(),
        client,
        run_id: super::admin::ADMIN_RUN.to_string(),
        user: user.to_string(),
        skill: None,
        bot: None,
        tool: "admin.console_members".to_string(),
        outcome,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: None,
        detail: None,
    };
    if !admin {
        state.svc.audit(event(Outcome::Denied)).await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only an active admin may list the team's handles",
        );
    }
    state.svc.audit(event(Outcome::Ok)).await;
    let handles: Vec<&str> = dir
        .users
        .values()
        .filter(|u| may_use(u))
        .map(|u| u.handle.as_str())
        .collect();
    no_store(Json(json!({ "handles": handles })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_routes_name_their_action_and_target() {
        assert_eq!(
            write_action(&Method::POST, "/internal/admin/users"),
            ("users.create", "*".into())
        );
        assert_eq!(
            write_action(&Method::PUT, "/internal/admin/users/ana/roles"),
            ("users.roles", "ana".into())
        );
        assert_eq!(
            write_action(
                &Method::POST,
                "/internal/admin/users/ana/sign-in-name/confirm"
            ),
            ("users.fields", "ana".into())
        );
        assert_eq!(
            write_action(
                &Method::DELETE,
                "/internal/admin/users/ana/identities/entra"
            ),
            ("users.identity_unlink", "ana".into())
        );
        assert_eq!(
            write_action(&Method::PUT, "/internal/admin/reviewers/rev"),
            ("reviewers.assign", "rev".into())
        );
    }

    #[test]
    fn refusals_have_the_documented_statuses() {
        for (r, s) in [
            (Refusal::Invalid, 422),
            (Refusal::NotFound, 404),
            (Refusal::Forbidden, 403),
            (Refusal::HostOnly, 403),
            (Refusal::SelfChange, 403),
            (Refusal::DeactivationLimit, 429),
            (Refusal::LastAdmin, 409),
            (Refusal::Stale, 409),
            (Refusal::Taken, 409),
            (Refusal::Exists, 409),
            (Refusal::Unchanged, 409),
            (Refusal::DualRole, 409),
            (Refusal::Linked, 409),
            (Refusal::NotActive, 409),
            (Refusal::NotReviewer, 409),
            (Refusal::NotPm, 409),
            (Refusal::EvalsAccount, 409),
        ] {
            assert_eq!(refusal_status(r).as_u16(), s, "{}", r.code());
        }
    }
}
