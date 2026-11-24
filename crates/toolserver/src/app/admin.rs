//! The admin and auditor reads: `GET /internal/admin/{audit,accounts,connectors,overview}`,
//! and the admin proposals (`crate::proposals`): `POST /internal/admin/proposals/open`,
//! `GET /internal/admin/proposals`, `GET /internal/admin/protection`, plus
//! `POST /internal/admin/actions`, where `pmos-web` records what it refused or showed.
//!
//! **Who may call:** the orchestrator (`PMOS_SERVICE_TOKEN`) and `pmos-web` (`PMOS_WEB_TOKEN`).
//! Never the Slack bot or the console. The proposal and admin-action routes are `pmos-web`'s
//! only, and need its signed user assertion in **every** assertion mode, `off` included: a
//! call without one is `401 bad_assertion`, and so is one whose subject isn't the `user=` it
//! names or whose body isn't the one signed. Opening a proposal is the only call here that
//! writes anywhere but the audit log: a new branch and a pull request in GitHub, as the admin.
//! `POST /internal/admin/events` (`pmos-web` only) records its sign-ins, sign-outs and refused
//! sign-ins, from a fixed set of actions and codes, never free text ([`sign_in_event`]); it
//! keeps its own rules (a refused sign-in names nobody, so it carries no assertion).
//!
//! **Whose view:** every call names its viewer as `user=`. The tool server checks that viewer's
//! roles in the user database, read for this request (`users::access`), against the
//! permission matrix (`pmos_contract::permissions`), whatever the caller already checked, and
//! for `pmos-web` the user assertion must name that same person (`PMOS_ASSERTION_MODE`, as on
//! the approvals routes). A deactivated person's kept roles grant nothing. A viewer without the
//! permission gets `403 {"code":"forbidden"}`.
//!
//! **Admin and Auditor on one person** (the pilot exception, `docs/user-database-spec.md` §6):
//! their audit-log reads and exports are recorded with the detail `dual_role admin+auditor`;
//! once the exception has lapsed they are refused `403 role_conflict` (admin powers stay).
//!
//! **Metadata only:** an audit event is served as a projection that never carries the event's
//! `sources` list, its source titles (Jira summaries and page titles can carry content), its
//! arguments hash or a free-text detail: `detail` is served only when it is one of a fixed set
//! of notes the tool server writes itself ([`safe_detail`]); anything else (a rejection's
//! free text, a connector's error message) is `null`.
//!
//! **Audited:** every call, allowed or refused, records an event of its own (`admin.audit`,
//! `admin.audit_export`, `admin.accounts`, `admin.connectors`, `admin.overview`) naming the
//! viewer and the client, with no query values.

use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, Utc};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{bearer, check_assertion, client_for, error, time_param, unavailable, AppState};
use crate::approvals::Status;
use crate::assertion::Mode;
use crate::audit::{AuditEvent, AuditQuery, Client, Cursor, Outcome, StoredEvent, MAX_PAGE};
use crate::ledger::Reason;
use crate::users::access::{may_use, permits};
use crate::users::{IdentityKind, User, UserRole};
use crate::vault::Provider;

/// The page size when `limit` is left out.
pub const DEFAULT_PAGE: usize = 100;

/// The run id admin reads are recorded under.
pub const ADMIN_RUN: &str = "admin";

pub(super) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/admin/audit", get(audit))
        .route("/admin/accounts", get(accounts))
        .route("/admin/connectors", get(connectors))
        .route("/admin/overview", get(overview))
        .route("/admin/proposals", get(list_proposals))
        .route(
            "/admin/proposals/open",
            axum::routing::post(open_proposal).layer(DefaultBodyLimit::max(MAX_PROPOSAL_BODY)),
        )
        .route("/admin/protection", get(protection))
        // MCP servers (`docs/mcp-servers-spec.md` §5): Policy Admin's read-only view.
        .route("/admin/mcp", get(mcp_servers))
        .route("/admin/policy/proofs", get(policy_proofs))
        .route("/admin/sandbox", get(sandbox_denials))
        .route("/admin/actions", axum::routing::post(record_action))
        .route("/admin/events", axum::routing::post(sign_in_event))
        .layer(middleware::from_fn_with_state(state, identify_admin_client))
}

/// The largest proposal the tool server takes: 50 content files of 256 KiB and the policy
/// files they regenerate, as JSON.
pub const MAX_PROPOSAL_BODY: usize = 24 * 1024 * 1024;

/// The routes that need `pmos-web`'s signed user assertion in every mode.
fn needs_assertion(path: &str) -> bool {
    let path = path.strip_prefix("/internal").unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);
    matches!(
        path,
        "/admin/proposals" | "/admin/proposals/open" | "/admin/protection" | "/admin/actions"
    )
}

/// `pmos-web`'s assertion on a [`needs_assertion`] route, whatever the mode: present, valid,
/// bound to this exact request (body included, up to `max_body` bytes), and its subject
/// the `user=` the call names. Returns the request with its body put back.
pub(super) async fn require_assertion(
    assertions: &crate::assertion::Assertions,
    client: Client,
    req: Request,
    max_body: usize,
) -> Result<Request, Box<Response>> {
    use crate::assertion::Refusal;
    let uri = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.clone())
        .unwrap_or_else(|| req.uri().clone());
    let Some(header) = req.headers().get(pmos_assertion::HEADER).cloned() else {
        return Err(super::refuse_assertion(client, Refusal::Missing));
    };
    let Ok(header) = header.to_str() else {
        return Err(super::refuse_assertion(client, Refusal::Header));
    };
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, max_body).await else {
        return Err(super::refuse_assertion(client, Refusal::Body));
    };
    let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let who = assertions
        .verify(
            client.as_str(),
            header,
            parts.method.as_str(),
            target,
            &bytes,
            pmos_assertion::unix_now(),
        )
        .map_err(|why| super::refuse_assertion(client, why))?;
    if super::named_user(&uri).as_deref() != Some(who.subject()) {
        return Err(super::refuse_assertion(client, Refusal::SubjectMismatch));
    }
    Ok(Request::from_parts(parts, axum::body::Body::from(bytes)))
}

/// The orchestrator and `pmos-web` only; `pmos-web`'s user assertion is checked as on the
/// approvals routes (its subject must be the `user=` the call names).
async fn identify_admin_client(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(c @ (Client::Orchestrator | Client::Web)) => c,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "this token may not use the admin routes",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let mut req = req;
    let assertions = &state.people.assertions;
    if needs_assertion(req.uri().path()) {
        if client != Client::Web {
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web, acting for a signed-in person, may use this route",
            );
        }
        match require_assertion(assertions, client, req, MAX_PROPOSAL_BODY).await {
            Ok(checked) => req = checked,
            Err(refused) => return *refused,
        }
    } else if client == Client::Web && assertions.mode() != Mode::Off {
        match check_assertion(assertions, client, req).await {
            Ok((checked, _)) => req = checked,
            Err(refused) => return *refused,
        }
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

pub(super) fn coded(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "code": code, "error": message }))).into_response()
}

/// Record one admin read.
async fn record(state: &AppState, client: Client, viewer: &str, tool: &str, outcome: Outcome) {
    record_with(state, client, viewer, tool, outcome, None).await
}

/// Record one admin read, with a fixed-format detail.
async fn record_with(
    state: &AppState,
    client: Client,
    viewer: &str,
    tool: &str,
    outcome: Outcome,
    detail: Option<&str>,
) {
    state
        .svc
        .audit(AuditEvent {
            at: Utc::now(),
            client,
            run_id: ADMIN_RUN.to_string(),
            user: viewer.to_string(),
            skill: None,
            bot: None,
            tool: tool.to_string(),
            outcome,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: detail.map(String::from),
            persona: None,
        })
        .await;
}

/// The viewer `user` names, if they are an active user with any of `any_of`. A refusal is
/// recorded and answered `403 forbidden`; a missing `user` is `400`.
pub(super) async fn viewer(
    state: &AppState,
    client: Client,
    user: Option<&str>,
    tool: &str,
    any_of: &[Permission],
) -> Result<String, Box<Response>> {
    viewer_marked(state, client, user, tool, any_of, false, None).await
}

/// [`viewer`], but an **allowed** read is recorded at most once per viewer per `window`
/// (refusals always are): for a view the app reads on every page open, such as the admin's
/// bots view, which the audit log's own page reads for its filter (gate-fix-4).
pub(super) async fn viewer_coalesced(
    state: &AppState,
    client: Client,
    user: Option<&str>,
    tool: &'static str,
    any_of: &[Permission],
    window: chrono::Duration,
) -> Result<String, Box<Response>> {
    viewer_marked(
        state,
        client,
        user,
        tool,
        any_of,
        false,
        Some((tool, window)),
    )
    .await
}

/// [`viewer`]; with `audit_read`, a read of the audit log: by a person holding Admin and
/// Auditor it is recorded with [`crate::users::detail::DUAL_ROLE`] while the pilot exception
/// is in force, and refused `403 role_conflict` once it has lapsed.
async fn viewer_marked(
    state: &AppState,
    client: Client,
    user: Option<&str>,
    tool: &str,
    any_of: &[Permission],
    audit_read: bool,
    coalesce: Option<(&'static str, chrono::Duration)>,
) -> Result<String, Box<Response>> {
    let Some(user) = user.map(str::trim).filter(|u| !u.is_empty()) else {
        return Err(Box::new(coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "`user` (the viewer) is required",
        )));
    };
    let dir = match state.svc.people().directory().await {
        Ok(dir) => dir,
        Err(e) => return Err(Box::new(unavailable(e))),
    };
    let found = dir.get(user);
    let allowed = found.is_some_and(|u| any_of.iter().any(|p| permits(u, *p)));
    let dual = audit_read && found.is_some_and(|u| u.active() && u.dual_role());
    if allowed && dual && !dir.exception_in_force(Utc::now()) {
        record_with(
            state,
            client,
            user,
            tool,
            Outcome::Denied,
            Some(crate::users::Refusal::DualRole.code()),
        )
        .await;
        tracing::info!(viewer = %user, tool, "audit read refused: the pilot exception has lapsed");
        return Err(Box::new(coded(
            StatusCode::FORBIDDEN,
            "role_conflict",
            "this person holds admin and auditor and the pilot exception has ended: someone with host access removes one role",
        )));
    }
    if !allowed {
        record(state, client, user, tool, Outcome::Denied).await;
        tracing::info!(viewer = %user, tool, "admin read refused: role");
        return Err(Box::new(coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        )));
    }
    let detail = dual.then_some(crate::users::detail::DUAL_ROLE);
    let record_it = coalesce
        .is_none_or(|(kind, window)| state.svc.first_in_window(kind, user, window, Utc::now()));
    if record_it {
        record_with(state, client, user, tool, Outcome::Ok, detail).await;
    }
    Ok(user.to_string())
}

// The audit log.

#[derive(Deserialize, Default)]
struct AuditParams {
    user: Option<String>,
    before: Option<String>,
    limit: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    from: Option<String>,
    to: Option<String>,
    /// A marketplace bot's key: only the events of its sessions, drafts and decisions.
    persona: Option<String>,
    /// `export`: the CSV export, which needs [`Permission::ExportAudit`].
    purpose: Option<String>,
}

fn param(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// The search the parameters ask for, or why they don't make one.
fn audit_query(q: &AuditParams) -> Result<AuditQuery, String> {
    let before = match param(&q.before) {
        None => None,
        Some(c) => Some(Cursor::decode(&c).ok_or("`before` isn't a cursor this server gave")?),
    };
    let limit = match param(&q.limit) {
        None => DEFAULT_PAGE,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_PAGE).contains(n))
            .ok_or(format!("`limit` must be from 1 to {MAX_PAGE}"))?,
    };
    let outcome = match param(&q.outcome) {
        None => None,
        Some(o) => Some(Outcome::parse(&o).ok_or(format!("unknown outcome `{o}`"))?),
    };
    let from = time_param("from", q.from.as_deref())?;
    let to = time_param("to", q.to.as_deref())?;
    if let (Some(f), Some(t)) = (from, to) {
        if f > t {
            return Err("`from` must not be after `to`".into());
        }
    }
    let persona = match param(&q.persona) {
        None => None,
        Some(p) if pmos_contract::bots::valid_name(&p) => Some(p),
        Some(_) => return Err("`persona` must be a bot's key, such as chief".into()),
    };
    Ok(AuditQuery {
        before,
        limit,
        actor: param(&q.actor),
        action: param(&q.action),
        outcome,
        from,
        to,
        persona,
    })
}

/// The notes the tool server writes itself in `detail`, which carry no content. Anything else
/// (a rejection's free text, a connector's or a refusal's message, which can quote arguments)
/// is never served. The admin proposal and event notes are [`ADMIN_NOTES`].
const FIXED_DETAILS: [&str; 11] = [
    "edited before approval",
    "approved in the harness",
    "approved in the harness; writing",
    "approved in the inbox; writing",
    "already drafted",
    "unlinked",
    "nothing was linked",
    "storage unavailable",
    "unlink for someone who isn't a team member",
    "decision by someone not in team.yaml",
    crate::service::NOT_A_MEMBER_DECISION,
];

/// The fixed notes of admin refusals and reads (`crate::proposals`, [`record_action`]).
pub const ADMIN_NOTES: [&str; 9] = [
    "role",
    "invalid",
    "stale base",
    "policy mismatch",
    "rate limited",
    "github not linked",
    "not configured",
    "personal file",
    "restricted context",
];

/// `detail` if it is metadata only: one of [`FIXED_DETAILS`] or [`ADMIN_NOTES`], an
/// invalid-draft count, a ledger rejection reason (a fixed label, not free text), a Slack
/// draft's expiry note, a purge batch's count and cutoff, or a sign-in event's provider,
/// refusal code and attempt hash.
pub fn safe_detail(detail: &str) -> Option<String> {
    // A decision after a reviewer's recommendation adds fixed parts (`crate::reviews`) after
    // its own detail, `; `-separated: each part must be safe on its own.
    if detail.contains("; ")
        && detail.split("; ").any(crate::reviews::is_safe_part)
        && detail
            .split("; ")
            .all(|p| crate::reviews::is_safe_part(p) || safe_detail(p).is_some())
    {
        return Some(detail.to_string());
    }
    let digits = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    let ok = FIXED_DETAILS.contains(&detail)
        || crate::reviews::is_safe_part(detail)
        || ADMIN_NOTES.contains(&detail)
        || crate::audit::parse_purge_detail(detail).is_some()
        || crate::users::detail::is_safe(detail)
        || crate::users::detail::is_purge_detail(detail)
        || crate::password::is_safe_detail(detail)
        || crate::team::is_safe_detail(detail)
        || crate::openshell::sandbox::is_safe_detail(detail)
        || is_sign_in_detail(detail)
        || detail
            .strip_prefix("reason: ")
            .is_some_and(|r| Reason::parse(r).is_some())
        || detail
            .strip_prefix("invalid draft (")
            .map(|r| {
                r.strip_suffix(": no attempts left in this session")
                    .unwrap_or(r)
            })
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|r| r.split_once(" of "))
            .is_some_and(|(n, m)| digits(n) && digits(m))
        || detail
            .strip_prefix("undecided after ")
            .and_then(|r| {
                r.strip_suffix(" h: nothing was posted, and the draft's text was removed")
            })
            .is_some_and(digits);
    ok.then(|| detail.to_string())
}

/// One event as an admin or auditor sees it: metadata only (see the module docs).
pub fn event_json(stored: &StoredEvent) -> Value {
    let e = &stored.event;
    json!({
        "id": format!("ev_{}", stored.id),
        "at": e.at,
        "actor": e.user,
        "client": e.client.as_str(),
        "action": e.tool,
        "system": e.tool.split('.').next().unwrap_or_default(),
        "target": e.output_ref,
        "skill": e.skill,
        "bot": e.bot,
        "persona": e.persona,
        "task_id": e.run_id,
        "outcome": e.outcome.as_str(),
        "detail": e.detail.as_deref().and_then(safe_detail),
        "sources": e.sources.len(),
        "titled_sources": e.source_titles.len(),
    })
}

/// `{"events": [...], "next": "<cursor>" | null}`, newest first.
async fn audit(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<AuditParams>,
) -> Response {
    let export = match param(&q.purpose).as_deref() {
        None => false,
        Some("export") => true,
        Some(_) => {
            return coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "`purpose` is `export` or left out",
            )
        }
    };
    let (tool, need) = if export {
        ("admin.audit_export", Permission::ExportAudit)
    } else {
        ("admin.audit", Permission::ViewAuditLog)
    };
    let query = match audit_query(&q) {
        Ok(query) => query,
        Err(why) => return coded(StatusCode::BAD_REQUEST, "bad_request", &why),
    };
    let who =
        match viewer_marked(&state, client, q.user.as_deref(), tool, &[need], true, None).await {
            Ok(who) => who,
            Err(refused) => return *refused,
        };
    // An Auditor (without Admin) sees one reason for every refused password step, and no
    // target, so the log can't tell them which sign-in names exist (L15). Admins see each.
    let admin_view = match state.svc.people().directory().await {
        Ok(dir) => dir.get(&who).is_some_and(|u| u.has(UserRole::Admin)),
        Err(e) => return unavailable(e),
    };
    let page = query.limit;
    let mut wide = query.clone();
    wide.limit = page + 1;
    let mut found = match state.svc.stores().audit.search(&wide).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return coded(
                StatusCode::NOT_IMPLEMENTED,
                "not_available",
                "this tool server's audit log can't be read back",
            )
        }
        Err(e) => return unavailable(e),
    };
    let more = found.len() > page;
    found.truncate(page);
    let next = more
        .then(|| found.last().map(|s| s.cursor().encode()))
        .flatten();
    // A sandbox decision's own columns (`crate::openshell::sandbox`): an Admin sees host,
    // address, port, method and path; an Auditor the rule, reason, port and a destination class
    // only (spec §B.7, SBA-11).
    let sandbox_ids: Vec<i64> = found
        .iter()
        .filter(|e| e.event.client == Client::SandboxAudit)
        .map(|e| e.id)
        .collect();
    let sandbox: std::collections::HashMap<i64, crate::openshell::sandbox::Fields> =
        match state.svc.openshell().sandbox_fields(&sandbox_ids).await {
            Ok(f) => f.into_iter().collect(),
            Err(e) => return unavailable(e),
        };
    let events: Vec<Value> = found
        .iter()
        .map(|e| {
            let mut v = if admin_view {
                event_json(e)
            } else {
                auditor_event_json(e)
            };
            if let Some(f) = sandbox.get(&e.id) {
                v["sandbox"] = if admin_view {
                    crate::openshell::sandbox::fields_json(f)
                } else {
                    crate::openshell::sandbox::auditor_fields_json(f)
                };
            }
            v
        })
        .collect();
    Json(json!({ "events": events, "next": next })).into_response()
}

/// The password refusals whose reason, or target, would tell whether a sign-in name exists.
const NAME_REVEALING: [(&str, &str); 2] = [
    ("password.sign_in", "sign_in_failed"),
    ("password.setup_begin", "setup_failed"),
];

/// [`event_json`] as an Auditor sees it (L15): a refused password step, or a refused setup's
/// first step before its code matched, shows one reason and no target, whatever happened.
pub fn auditor_event_json(stored: &StoredEvent) -> Value {
    let mut v = event_json(stored);
    let e = &stored.event;
    let before_match = |d: &str| {
        matches!(
            d,
            "unknown_name"
                | "no_password"
                | "wrong_password"
                | "deactivated"
                | "locked"
                | "host_only"
                | "wrong_code"
                | "expired"
        )
    };
    if let Some((_, one)) = NAME_REVEALING.iter().find(|(a, _)| *a == e.tool) {
        let reveals = e.outcome == Outcome::Denied
            && (e.tool == "password.sign_in" || e.detail.as_deref().is_some_and(before_match));
        if reveals {
            v["detail"] = json!(one);
            v["target"] = Value::Null;
        }
    }
    v
}

// Team accounts.

#[derive(Deserialize)]
struct ViewerParams {
    user: Option<String>,
}

/// `{"linking": bool, "members": [{"handle", "status", "roles", "accounts": {"atlassian":
/// bool, ...}}]}`: every user in the database, with which accounts they have linked, as
/// booleans. Never a token, scope, address or account id.
async fn accounts(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ViewAuditLog, Permission::ManageMembers];
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), "admin.accounts", &need).await {
        return *refused;
    }
    let dir = match state.svc.people().directory().await {
        Ok(dir) => dir,
        Err(e) => return unavailable(e),
    };
    let mut members = Vec::new();
    for m in dir.users.values() {
        let linked = match state.svc.stores().credentials.list(&m.handle).await {
            Ok(linked) => linked,
            Err(e) => return unavailable(e),
        };
        let accounts: serde_json::Map<String, Value> = Provider::ALL
            .iter()
            .map(|p| {
                (
                    p.as_str().to_string(),
                    json!(linked.iter().any(|c| c.provider == *p)),
                )
            })
            .collect();
        members.push(json!({
            "handle": m.handle,
            "status": m.status.as_str(),
            "roles": m.roles.keys().map(|r| r.as_str()).collect::<Vec<_>>(),
            "accounts": accounts,
        }));
    }
    Json(json!({ "linking": state.oauth.is_some(), "members": members })).into_response()
}

// Connectors.

/// A readiness failure's class, never its text: `timeout`, `auth`, `unreachable` or `error`.
pub fn error_class(why: &str) -> &'static str {
    let w = why.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| w.contains(n));
    if has(&["timed out", "timeout", "within"]) {
        "timeout"
    } else if has(&["401", "403", "unauthori", "forbidden", "token"]) {
        "auth"
    } else if has(&["connect", "didn't answer", "dns", "refused", "unreachable"]) {
        "unreachable"
    } else {
        "error"
    }
}

/// `{"connectors": [{"name", "mode", "health", "error_class", "checked_at"}]}`, checked now.
async fn connectors(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ManageConnectors];
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), "admin.connectors", &need).await
    {
        return *refused;
    }
    let checked_at = Utc::now();
    let out: Vec<Value> = state
        .svc
        .connector_status()
        .await
        .into_iter()
        .map(|(name, mode, check)| {
            if let Err(why) = &check {
                tracing::warn!(connector = %name, error = %why, "connector not ready");
            }
            json!({
                "name": name,
                "mode": mode,
                "health": if check.is_ok() { "ok" } else { "down" },
                "error_class": check.err().map(|w| error_class(&w)),
                "checked_at": checked_at,
            })
        })
        .collect();
    Json(json!({ "connectors": out })).into_response()
}

// Overview.

/// How far back the overview's counts go.
pub const OVERVIEW_DAYS: i64 = 7;

/// `{"since", "pending_drafts", "held_drafts", "approved", "rejected", "denied", …, "users"}`:
/// pending approval items of every active user now (a deactivated owner's are `held_drafts`),
/// audit events of each outcome since `since` (`null` when the audit log can't be read back),
/// and the user database's flags ([`users_flags`]).
async fn overview(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ViewAuditLog, Permission::ManageMembers];
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), "admin.overview", &need).await {
        return *refused;
    }
    let since: DateTime<Utc> = Utc::now() - Duration::days(OVERVIEW_DAYS);
    let dir = match state.svc.people().directory().await {
        Ok(dir) => dir,
        Err(e) => return unavailable(e),
    };
    let (mut pending, mut held) = (0usize, 0usize);
    for m in dir.users.values() {
        match state
            .svc
            .stores()
            .approvals
            .list(&m.handle, Some(Status::Pending))
            .await
        {
            Ok(items) if m.active() => pending += items.len(),
            Ok(items) => held += items.len(),
            Err(e) => return unavailable(e),
        }
    }
    let conflicts = match identity_conflicts(&state, since).await {
        Ok(n) => n,
        Err(e) => return unavailable(e),
    };
    let links = match first_links(&state, since).await {
        Ok(l) => l,
        Err(e) => return unavailable(e),
    };
    let mut users = users_flags(&state, &dir, conflicts, Utc::now());
    // U1: every first link a person made through pmos-web (who, when, which identity
    // provider), never the `oid`. `null` when the audit log can't be read back.
    users["first_links"] = json!(links);
    // PM2, LF: every reset code issued for someone who had a password, in the overview's
    // window, read by time (not from a window of recent rows): who, by whom, when, and when
    // the person's notice was dismissed (handles only). M2: every setup code void after too
    // many wrong guesses, so the admin issues a new one.
    let pw = match state
        .svc
        .stores()
        .users
        .passwords()
        .password_overview(since)
        .await
    {
        Ok(pw) => pw,
        Err(e) => return unavailable(e),
    };
    users["password_resets"] = json!(pw
        .resets
        .iter()
        .map(|r| json!({
            "handle": r.handle,
            "by": r.by,
            "at": r.at,
            "dismissed_at": r.dismissed_at,
        }))
        .collect::<Vec<_>>());
    let counts = match state.svc.stores().audit.count_outcomes(since).await {
        Ok(c) => c,
        Err(e) => return unavailable(e),
    };
    let count = |o: Outcome| {
        counts
            .as_ref()
            .map(|c| c.get(&o).copied().unwrap_or_default())
    };
    let retention = match state.svc.stores().audit.retention().await {
        Ok(r) => r.unwrap_or_default(),
        Err(e) => return unavailable(e),
    };
    let sandbox = match crate::openshell::overview(&state.svc, Utc::now()).await {
        Ok(s) => s,
        Err(e) => return unavailable(e),
    };
    Json(json!({
        "sandbox": sandbox,
        "since": since,
        "pending_drafts": pending,
        "held_drafts": held,
        "users": users,
        "approved": count(Outcome::Approved),
        "rejected": count(Outcome::Rejected),
        "denied": count(Outcome::Denied),
        "retention_days": state.svc.audit_retention_days(),
        "oldest_event_at": retention.oldest_event_at,
        "last_purge": retention
            .last_purge
            .map(|p| json!({ "at": p.at, "deleted": p.deleted })),
    }))
    .into_response()
}

/// The pilot exception's end is flagged this many days ahead (§6).
pub const EXCEPTION_WARN_DAYS: i64 = 14;

/// `users.identity_conflict` events since `since` (slice U4c records them): how many, and the
/// users whose sign-in name was presented (their targets, handles only). `None` when the audit
/// log can't be read back.
async fn identity_conflicts(
    state: &AppState,
    since: DateTime<Utc>,
) -> Result<Option<(usize, Vec<String>)>, crate::store::StoreError> {
    let query = AuditQuery {
        before: None,
        limit: MAX_PAGE,
        actor: None,
        action: Some("users.identity_conflict".into()),
        outcome: None,
        from: Some(since),
        to: None,
        persona: None,
    };
    Ok(state
        .svc
        .stores()
        .audit
        .search(&query)
        .await?
        .map(|events| {
            let mut who: Vec<String> = events
                .iter()
                .filter_map(|e| e.event.output_ref.clone())
                .filter(|h| crate::session::valid_handle(h))
                .collect();
            who.sort();
            who.dedup();
            (events.len(), who)
        }))
}

/// First Entra links since `since` (U1, the review of `b469d5b`): the `users.identity_link`
/// events the first-link route recorded (client `web`, outcome `ok`, detail `entra`), newest
/// first, each `{handle, at, provider}`. Handles only: never an `oid` or an address, to an
/// admin or an Auditor. `None` when the audit log can't be read back.
async fn first_links(
    state: &AppState,
    since: DateTime<Utc>,
) -> Result<Option<Vec<Value>>, crate::store::StoreError> {
    let query = AuditQuery {
        before: None,
        limit: MAX_PAGE,
        actor: None,
        action: Some("users.identity_link".into()),
        outcome: Some(Outcome::Ok),
        from: Some(since),
        to: None,
        persona: None,
    };
    Ok(state
        .svc
        .stores()
        .audit
        .search(&query)
        .await?
        .map(|events| {
            let mut out: Vec<(DateTime<Utc>, String)> = events
                .iter()
                .filter(|e| {
                    e.event.client == Client::Web && e.event.detail.as_deref() == Some("entra")
                })
                .filter_map(|e| {
                    e.event
                        .output_ref
                        .clone()
                        .filter(|h| crate::session::valid_handle(h))
                        .map(|h| (e.event.at, h))
                })
                .collect();
            out.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
            out.into_iter()
                .map(|(at, handle)| json!({"handle": handle, "at": at, "provider": "entra"}))
                .collect()
        }))
}

/// The user database's flags on the overview (§4, §6, §2): handles and counts only, never an
/// address, a Slack id or an `oid`.
fn users_flags(
    state: &AppState,
    dir: &crate::users::Directory,
    conflicts: Option<(usize, Vec<String>)>,
    now: DateTime<Utc>,
) -> Value {
    let handles = |f: &dyn Fn(&User) -> bool| -> Vec<String> {
        dir.users
            .values()
            .filter(|u| f(u))
            .map(|u| u.handle.clone())
            .collect()
    };
    // §4: joined through the Slack bot client, Microsoft sign-in name not confirmed yet.
    let unconfirmed = handles(&|u| {
        u.active() && u.sign_in_name.is_some() && u.sign_in_name_confirmed_at.is_none()
    });
    let dual = handles(&|u| u.active() && u.dual_role());
    let in_force = dir.exception_in_force(now);
    let exception = dir.exception.as_ref().map(|e| {
        json!({
            "ends": crate::users::detail::date(e.expires_at),
            "ending_soon": in_force && e.expires_at - now <= Duration::days(EXCEPTION_WARN_DAYS),
            "lapsed": !in_force,
        })
    });
    let cap = state.svc.stores().users.rules().deactivations_per_hour as usize;
    let recent = dir.capped_deactivations.len();
    let roster = state.svc.roster();
    let owner_gone: Vec<String> = roster
        .bots()
        .filter(|b| !dir.get(&b.owner).is_some_and(may_use))
        .map(|b| b.name.clone())
        .collect();
    json!({
        "active": dir.users.values().filter(|u| u.active()).count(),
        "deactivated": dir.users.values().filter(|u| !u.active()).count(),
        "sign_in_unconfirmed": unconfirmed,
        "dual_role": dual,
        "pilot_exception": exception,
        // §6 "Lapsed": a red notice naming them until one role is gone.
        "role_conflicts": if in_force { Vec::new() } else { dual.clone() },
        "entra_needs_host": handles(&|u| u.active() && u.entra_state() == "needs_host"),
        "no_slack": handles(&|u| u.active() && u.identity(IdentityKind::Slack).is_none() && u.has(UserRole::Pm)),
        "identity_conflicts": conflicts.as_ref().map(|(n, _)| *n),
        // §7 step 3: "someone signed in with Microsoft as <handle>'s sign-in name".
        "identity_conflict_users": conflicts.map(|(_, who)| who),
        "resolve_miss_warning_threshold": crate::users::access::ResolveMisses::WARN_PER_HOUR,
        "deactivation_grace_hours": crate::users::CREDENTIAL_GRACE_HOURS,
        "resolve_miss_warning": state.svc.people().resolve_misses().warning(),
        "deactivations_last_hour": recent,
        "deactivation_limit": cap,
        "deactivation_limit_reached": recent >= cap,
        "bots_without_active_owner": owner_gone,
    })
}

// Sign-in events from `pmos-web`.

/// The actions `POST /internal/admin/events` records. Nothing else is accepted.
/// `users.identity_conflict` is `pmos-web` flagging a known Microsoft account that signed in
/// with a name that is another user's sign-in name (spec §7 step 3; slice U4c).
pub const SIGN_IN_ACTIONS: [&str; 4] = [
    "web.sign_in",
    "web.sign_out",
    "web.sign_in_refused",
    IDENTITY_CONFLICT,
];

/// The action an Entra identity conflict is recorded as, here and by the first-link route.
pub const IDENTITY_CONFLICT: &str = "users.identity_conflict";

/// The sign-in providers an event may name.
pub const SIGN_IN_PROVIDERS: [&str; 3] = ["entra", "dev", "password"];

/// Why a sign-in was refused, as `pmos-web` codes it (its signed-out page's `reason`).
/// `wrong_account` is from before the user database (no longer sent, still accepted).
pub const SIGN_IN_REFUSALS: [&str; 12] = [
    "not_a_member",
    "wrong_account",
    "expired",
    "cancelled",
    "unavailable",
    "failed",
    "deactivated",
    "sign_in_unconfirmed",
    "entra_needs_host",
    "entra_misconfigured",
    "users_unavailable",
    // Slice H10: the admin door refused someone who is neither Admin nor Auditor.
    "admin_door_not_allowed",
];

/// Who a refused sign-in is recorded as: nobody signed in.
pub const ANONYMOUS: &str = "anonymous";

/// The run id sign-in events are recorded under.
pub const SIGN_IN_RUN: &str = "web-auth";

/// The length of a refused sign-in's `attempt`: hex of a keyed hash of the name tried.
pub const ATTEMPT_HEX: usize = 12;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInRecord {
    action: String,
    provider: String,
    /// `web.sign_in_refused` only: one of [`SIGN_IN_REFUSALS`].
    #[serde(default)]
    reason: Option<String>,
    /// `web.sign_in_refused` only: [`ATTEMPT_HEX`] lowercase hex characters.
    #[serde(default)]
    attempt: Option<String>,
    /// `web.sign_in_refused` with `wrong_account`: the member whose email was used; and
    /// `users.identity_conflict` (required): the user whose sign-in name was presented.
    #[serde(default)]
    member: Option<String>,
}

fn is_attempt(a: &str) -> bool {
    a.len() == ATTEMPT_HEX && a.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A sign-in event's `detail`: `provider: <p>`, and for a refusal `; reason: <r>` and
/// optionally `; attempt: <hex>`. Fixed words and codes only.
pub fn sign_in_detail(provider: &str, reason: Option<&str>, attempt: Option<&str>) -> String {
    let mut d = format!("provider: {provider}");
    if let Some(r) = reason {
        d.push_str(&format!("; reason: {r}"));
    }
    if let Some(a) = attempt {
        d.push_str(&format!("; attempt: {a}"));
    }
    d
}

/// Is `detail` exactly what [`sign_in_detail`] writes, from the fixed sets?
fn is_sign_in_detail(detail: &str) -> bool {
    let mut parts = detail.split("; ");
    let Some(provider) = parts.next().and_then(|p| p.strip_prefix("provider: ")) else {
        return false;
    };
    if !SIGN_IN_PROVIDERS.contains(&provider) {
        return false;
    }
    match parts.next() {
        None => return true,
        Some(r) => match r.strip_prefix("reason: ") {
            Some(r) if SIGN_IN_REFUSALS.contains(&r) => {}
            _ => return false,
        },
    }
    match parts.next() {
        None => true,
        Some(a) => a.strip_prefix("attempt: ").is_some_and(is_attempt) && parts.next().is_none(),
    }
}

/// `POST /internal/admin/events[?user=<handle>]`: `pmos-web` records a sign-in, a sign-out
/// or a refused sign-in (`docs/web-api.md`, "Sign-in events"). `pmos-web`'s token only.
/// A sign-in or sign-out names the member as `user=` (with a user assertion for them, as on
/// every admin route); a refusal names no one and is recorded as [`ANONYMOUS`], with the
/// refusal's code, and at most a short keyed hash of the name tried, never the name.
/// Answers `503` when the event couldn't be recorded, so the caller knows.
async fn sign_in_event(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
    body: axum::body::Bytes,
) -> Response {
    if client != Client::Web {
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only pmos-web records sign-in events",
        );
    }
    let bad = |why: &str| coded(StatusCode::BAD_REQUEST, "bad_request", why);
    let Ok(r) = serde_json::from_slice::<SignInRecord>(&body) else {
        return bad("the body isn't a sign-in event");
    };
    let Some(action) = SIGN_IN_ACTIONS.iter().find(|a| **a == r.action) else {
        return bad("`action` isn't a sign-in event this server records");
    };
    let Some(provider) = SIGN_IN_PROVIDERS.iter().find(|p| **p == r.provider) else {
        return bad("`provider` is `entra`, `dev` or `password`");
    };
    let user = q.user.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let dir = match state.svc.people().directory().await {
        Ok(dir) => dir,
        Err(e) => return unavailable(e),
    };
    // Anyone active may sign in to the web app (an auditor reads the audit log there).
    let is_user = |h: &str| dir.get(h).is_some_and(|u| u.active());
    let event = |user: &str, outcome, output_ref: Option<String>, detail: String| AuditEvent {
        at: Utc::now(),
        client,
        run_id: SIGN_IN_RUN.to_string(),
        user: user.to_string(),
        skill: None,
        bot: None,
        tool: action.to_string(),
        outcome,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref,
        detail: Some(detail),
        persona: None,
    };
    let ev = if *action == IDENTITY_CONFLICT {
        // A known Microsoft account signed in with another user's sign-in name: recorded as
        // the first-link route records a conflict (actor `anonymous`, detail `entra`, the
        // user as target), so the overview counts it. No `user=`: nobody is asserted.
        if user.is_some() || r.reason.is_some() || r.attempt.is_some() || *provider != "entra" {
            return bad("an identity conflict is entra, with `member` only");
        }
        let Some(member) = r.member.as_deref().filter(|m| dir.get(m).is_some()) else {
            return bad("`member` must be a user");
        };
        AuditEvent {
            at: Utc::now(),
            client,
            run_id: SIGN_IN_RUN.to_string(),
            user: ANONYMOUS.to_string(),
            skill: None,
            bot: None,
            tool: IDENTITY_CONFLICT.to_string(),
            outcome: Outcome::Denied,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: Some(member.to_string()),
            detail: Some("entra".to_string()),
            persona: None,
        }
    } else if *action == "web.sign_in_refused" {
        if user.is_some() {
            return bad("a refused sign-in names no `user`");
        }
        let Some(reason) = r
            .reason
            .as_deref()
            .and_then(|x| SIGN_IN_REFUSALS.iter().find(|s| **s == x))
        else {
            return bad("`reason` isn't a refusal code this server records");
        };
        if r.attempt.as_deref().is_some_and(|a| !is_attempt(a)) {
            return bad("`attempt` is 12 lowercase hex characters");
        }
        let member = match r.member.as_deref() {
            None => None,
            Some(m) if *reason == "wrong_account" && is_user(m) => Some(format!("member:{m}")),
            Some(_) => return bad("`member` is only for `wrong_account`, and must be a member"),
        };
        event(
            ANONYMOUS,
            Outcome::Denied,
            member,
            sign_in_detail(provider, Some(reason), r.attempt.as_deref()),
        )
    } else {
        if r.reason.is_some() || r.attempt.is_some() || r.member.is_some() {
            return bad("only a refused sign-in has `reason`, `attempt` or `member`");
        }
        let Some(user) = user else {
            return bad("`user` (who signed in or out) is required");
        };
        if !is_user(user) {
            return coded(
                StatusCode::FORBIDDEN,
                "forbidden",
                "this person isn't a team member",
            );
        }
        event(
            user,
            Outcome::Ok,
            None,
            sign_in_detail(provider, None, None),
        )
    };
    if let Err(e) = state.svc.stores().audit.record(ev).await {
        tracing::error!(error = %e, action, "AUDIT UNAVAILABLE: sign-in event not recorded");
        return unavailable(e);
    }
    tracing::info!(action, provider, "sign-in event recorded");
    Json(json!({ "recorded": true })).into_response()
}

// Proposals (`crate::proposals`).

#[derive(Deserialize)]
struct ProposalParams {
    user: Option<String>,
    kind: Option<String>,
}

/// The `user=` the call names: on these routes always the assertion's verified subject.
fn named(user: Option<&str>) -> Result<&str, Box<Response>> {
    user.map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            Box::new(coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "`user` (the admin) is required",
            ))
        })
}

/// `POST /internal/admin/proposals/open?user=<admin>`: check a change set again and open it as
/// a pull request with the admin's own GitHub account (`crate::proposals::open`).
async fn open_proposal(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
    body: axum::body::Bytes,
) -> Response {
    let user = match named(q.user.as_deref()) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    crate::proposals::open(
        &state.svc,
        state.onboarding.oauth(),
        state.onboarding.proposals(),
        client,
        user,
        &body,
    )
    .await
}

/// `GET /internal/admin/proposals?user=<viewer>&kind=policy|content`: open proposals, read
/// with the viewer's own GitHub account. Needs `view-policies` or `view-content`, by kind.
async fn list_proposals(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ProposalParams>,
) -> Response {
    let (kind, tool, need) = match q.kind.as_deref() {
        Some("policy") => (
            pmos_policy::admin::proposals::Kind::Policy,
            "admin.policy_proposals",
            Permission::ViewPolicies,
        ),
        Some("content") => (
            pmos_policy::admin::proposals::Kind::Content,
            "admin.content_proposals",
            Permission::ViewContent,
        ),
        _ => {
            return coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "`kind` is `policy` or `content`",
            )
        }
    };
    let user = match viewer(&state, client, q.user.as_deref(), tool, &[need]).await {
        Ok(u) => u,
        Err(r) => return *r,
    };
    crate::proposals::list(
        state.onboarding.oauth(),
        state.onboarding.proposals(),
        &user,
        kind,
    )
    .await
}

/// `GET /internal/admin/protection?user=<viewer>`: whether the content branch requires
/// reviews, as the viewer's GitHub account reads it. Not recorded when allowed (the policy and
/// content screens ask on every load); a refusal is.
async fn protection(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let user = match named(q.user.as_deref()) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let allowed = match state.svc.people().user(user).await {
        Ok(u) => u.is_some_and(|m| {
            permits(&m, Permission::ViewPolicies) || permits(&m, Permission::ViewContent)
        }),
        Err(e) => return unavailable(e),
    };
    if !allowed {
        record(&state, client, user, "admin.protection", Outcome::Denied).await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        );
    }
    crate::proposals::protection(state.onboarding.oauth(), state.onboarding.proposals(), user).await
}

/// The MCP servers this tool server reaches, as Policy Admin shows them: per server its
/// operator, URL host, auth kind, scopes, data class, systems, approved tools with their class
/// and digest, and its last `tools/list` check; and `serving`, the systems of its record this
/// tool server routes to it (the rest stay on their built-in connectors). Metadata only: never
/// a token, and the tool names the server lists are the server's own words, shown to Admins
/// and Auditors only.
async fn mcp_servers(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ViewPolicies];
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), "admin.mcp", &need).await {
        return *refused;
    }
    let mut servers = crate::mcp_client::policy_view(state.svc.mcp_servers());
    crate::mcp_client::with_serving(&mut servers, |tool| state.svc.served_by(tool));
    Json(json!({ "servers": servers })).into_response()
}

/// `GET /internal/admin/policy/proofs?user=<viewer>`: the newest host proof of each bot
/// (`docs/openshell-prover-and-sandbox-audit-spec.md` §A.5), for Policy Admin's Prover row.
/// `view-policies` (admins and auditors): a proof holds only policy terms (a status, a reason
/// code, a counterexample naming a path, host, port or route of the policy, hashes). Not
/// recorded when allowed (Policy Admin asks on every load, as for `protection`); a refusal is.
/// The tool server runs no prover: these are what `pmos-admin policy prove` recorded.
async fn policy_proofs(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let user = match named(q.user.as_deref()) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let allowed = match state.svc.people().user(user).await {
        Ok(u) => u.is_some_and(|m| permits(&m, Permission::ViewPolicies)),
        Err(e) => return unavailable(e),
    };
    if !allowed {
        record(&state, client, user, "admin.policy_proofs", Outcome::Denied).await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        );
    }
    let proofs = match state.svc.openshell().latest_proofs().await {
        Ok(p) => p,
        Err(e) => return unavailable(e),
    };
    // Each sandbox's last policy load OpenShell reported (Part B): a proof older than it is stale.
    let loads = match state
        .svc
        .openshell()
        .sandbox_rows(&crate::openshell::sandbox::SandboxQuery {
            tools: vec!["sandbox.policy_loaded".into()],
            limit: 1000,
            ..Default::default()
        })
        .await
    {
        Ok(rows) => {
            let mut m: std::collections::BTreeMap<String, DateTime<Utc>> = Default::default();
            for r in rows {
                if let Some(b) = r.event.bot {
                    let at = m.entry(b).or_insert(r.event.at);
                    *at = (*at).max(r.event.at);
                }
            }
            m
        }
        Err(e) => return unavailable(e),
    };
    Json(json!({
        "policy_loads": loads,
        "proofs": proofs,
        "not_proved": pmos_policy::prove::NOT_PROVED,
        "domains": pmos_policy::prove::DOMAINS,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct SandboxParams {
    user: Option<String>,
    bot: Option<String>,
}

/// `GET /internal/admin/sandbox?user=<viewer>[&bot=<bot>]`: each bot's latest sandbox denials and
/// findings (at most 20 a bot, the last 7 days), the policy loads Policy Admin compares proofs
/// with, and the collector's state (spec §B.5). `view-audit-log` or `view-policies` (admins and
/// auditors); an Auditor sees a destination class, not the host, address or path (§B.7). Not
/// recorded when allowed (Policy Admin reads it on every load); a refusal is.
async fn sandbox_denials(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<SandboxParams>,
) -> Response {
    use crate::openshell::sandbox::SandboxQuery;
    let user = match named(q.user.as_deref()) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let found = match state.svc.people().user(user).await {
        Ok(u) => u,
        Err(e) => return unavailable(e),
    };
    let allowed = found.as_ref().is_some_and(|m| {
        permits(m, Permission::ViewAuditLog) || permits(m, Permission::ViewPolicies)
    });
    if !allowed {
        record(&state, client, user, "admin.sandbox", Outcome::Denied).await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        );
    }
    let admin = found.as_ref().is_some_and(|m| m.has(UserRole::Admin));
    let bot = match q.bot.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        None => None,
        Some(b) if pmos_contract::bots::valid_name(b) => Some(b.to_string()),
        Some(_) => {
            return coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "`bot` must be a bot's name",
            )
        }
    };
    let now = Utc::now();
    let store = state.svc.openshell();
    let rows = match store
        .sandbox_rows(&SandboxQuery {
            bot: bot.clone(),
            since: Some(now - Duration::days(7)),
            tools: vec![
                "sandbox.deny".into(),
                "sandbox.deny_summary".into(),
                "sandbox.finding".into(),
                "sandbox.policy_loaded".into(),
            ],
            limit: 1000,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => return unavailable(e),
    };
    let mut latest: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    let mut loads: std::collections::BTreeMap<String, DateTime<Utc>> = Default::default();
    for r in &rows {
        let Some(b) = r.event.bot.clone() else {
            continue;
        };
        if r.event.tool == "sandbox.policy_loaded" {
            let at = loads.entry(b).or_insert(r.event.at);
            *at = (*at).max(r.event.at);
            continue;
        }
        let list = latest.entry(b).or_default();
        if list.len() < 20 {
            list.push(crate::openshell::row_json(r, admin));
        }
    }
    let collector = match store.collector_state(now).await {
        Ok(c) => c,
        Err(e) => return unavailable(e),
    };
    Json(json!({
        "configured": state.svc.sandbox_audit_token().is_some(),
        "collector": collector,
        "denials": latest,
        "policy_loads": loads,
        "view": if admin { "admin" } else { "auditor" },
    }))
    .into_response()
}

/// The actions `pmos-web` may record ([`record_action`]).
pub const EVENT_ACTIONS: [&str; 11] = [
    "admin.policy_read",
    "admin.policy_validate",
    "admin.policy_proposals",
    "admin.policy_proposal",
    "admin.content_read",
    "admin.content_file",
    "admin.content_validate",
    "admin.content_proposals",
    "admin.content_proposal",
    "admin.harness",
    SENSITIVE_READ,
];

/// An admin reading a PM's personal master file or restricted context.
pub const SENSITIVE_READ: &str = "admin.content_sensitive_read";

/// The most files one event counts.
pub const MAX_EVENT_COUNT: usize = 500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRecord {
    /// One of [`EVENT_ACTIONS`].
    action: String,
    /// `denied` (a refusal `pmos-web` made) or `ok` (only [`SENSITIVE_READ`]).
    outcome: String,
    /// One of [`ADMIN_NOTES`].
    detail: String,
    /// How many files it was about; recorded as that many `content` entries in `sources`,
    /// served as a count. Never a path.
    #[serde(default)]
    count: usize,
    /// `content:personal:<handle>` for a personal master file; otherwise none.
    #[serde(default)]
    target: Option<String>,
}

/// `POST /internal/admin/actions?user=<viewer>`: record what `pmos-web` refused the viewer
/// (a role refusal on an admin route, a proposal its own checks refused) or an admin's read
/// of a personal or restricted file, which `pmos-web` records **before** it shows the text.
/// The viewer must be a team member; a sensitive read needs `edit-content`. Never file text,
/// and a path only as `content:personal:<handle>` (a restricted file's name can itself be
/// sensitive, so it is never sent).
async fn record_action(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
    body: axum::body::Bytes,
) -> Response {
    let user = match named(q.user.as_deref()) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let Ok(r) = serde_json::from_slice::<EventRecord>(&body) else {
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body isn't an admin event",
        );
    };
    let outcome = match r.outcome.as_str() {
        "denied" => Outcome::Denied,
        "ok" if r.action == SENSITIVE_READ => Outcome::Ok,
        _ => {
            return coded(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "`outcome` is `denied`, or `ok` for a sensitive read",
            )
        }
    };
    let target_ok = match &r.target {
        None => true,
        Some(t) => t
            .strip_prefix("content:personal:")
            .is_some_and(crate::session::valid_handle),
    };
    if !EVENT_ACTIONS.contains(&r.action.as_str())
        || !ADMIN_NOTES.contains(&r.detail.as_str())
        || r.count > MAX_EVENT_COUNT
        || !target_ok
    {
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the event has an unknown action or note, too many files, or a bad target",
        );
    }
    let member = match state.svc.people().user(user).await {
        Ok(u) => u.filter(|u| u.active()),
        Err(e) => return unavailable(e),
    };
    let allowed = match outcome {
        Outcome::Ok => member.is_some_and(|m| permits(&m, Permission::EditContent)),
        _ => member.is_some(),
    };
    let mut event = crate::proposals::event(client, user, &r.action, outcome);
    if !allowed {
        event.outcome = Outcome::Denied;
        event.detail = Some("role".into());
        state.svc.audit(event).await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        );
    }
    event.detail = Some(r.detail.clone());
    event.sources = vec!["content".to_string(); r.count];
    event.output_ref = r.target.clone();
    // A sensitive read is shown only once recorded: say so when it can't be.
    if let Err(e) = state.svc.stores().audit.record(event).await {
        tracing::error!(error = %e, action = %r.action, "AUDIT EVENT NOT RECORDED");
        return coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "audit_unavailable",
            "the event couldn't be recorded",
        );
    }
    tracing::info!(viewer = %user, action = %r.action, outcome = outcome.as_str(), "admin event recorded");
    Json(json!({ "recorded": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_metadata_details_are_served() {
        for ok in [
            "edited before approval",
            "reason: tone",
            "invalid draft (1 of 2)",
            "invalid draft (2 of 2): no attempts left in this session",
            "undecided after 24 h: nothing was posted, and the draft's text was removed",
            "purged 1200 events older than 2026-08-19T03:00:00Z",
            "provider: entra",
            "provider: dev; reason: not_a_member",
            "provider: entra; reason: not_a_member; attempt: 3f9a1c2b4d5e",
            // A reviewer's recommendation, and a decision after one (slice U6a).
            "verdict approve revision 3",
            "own_draft",
            "after a recommendation to approve, on an earlier version",
            "edited before approval; after a recommendation to approve, on this version",
            "reason: tone; after a recommendation to make changes, on this version; after a recommendation to approve, on an earlier version",
        ] {
            assert_eq!(safe_detail(ok).as_deref(), Some(ok));
        }
        for no in [
            "reason: tone; Looks fine to me, ship it",
            "after a recommendation to approve, on this version; the note says hi",
            "verdict approve revision 3; note: ship it",
        ] {
            assert_eq!(safe_detail(no), None, "{no}");
        }
        for no in [
            "reason: the launch date is wrong, it's March 3",
            "invalid draft (x of 2)",
            "jira answered 400: summary 'Secret project' is too long",
            "undecided after 24 h: something else",
            "",
            "purged many events older than yesterday",
            "purged 3 events older than 2026-08-19T03:00:00Z and the secret",
            "provider: okta",
            "provider: entra; reason: ana@example.com",
            "provider: entra; reason: not_a_member; attempt: ana@example.com",
            "provider: entra; reason: not_a_member; attempt: 3F9A1C2B4D5E",
            "provider: entra; reason: not_a_member; attempt: 3f9a1c2b4d5e; extra",
        ] {
            assert_eq!(safe_detail(no), None, "{no}");
        }
    }

    #[test]
    fn errors_are_classed_not_quoted() {
        assert_eq!(
            error_class("the docs server didn't answer within 2 s"),
            "timeout"
        );
        assert_eq!(error_class("HTTP 401 Unauthorized"), "auth");
        assert_eq!(
            error_class("error sending request: connect refused"),
            "unreachable"
        );
        assert_eq!(error_class("bad JSON"), "error");
    }

    #[test]
    fn cursors_round_trip_and_reject_junk() {
        let c = Cursor {
            at: "2026-11-14T08:00:00.123456Z".parse().unwrap(),
            id: 42,
        };
        assert_eq!(Cursor::decode(&c.encode()), Some(c));
        assert_eq!(Cursor::decode("c1.!!"), None);
        assert_eq!(Cursor::decode("42"), None);
    }
}
