//! The admin and auditor reads: `GET /internal/admin/{audit,accounts,connectors,overview}`,
//! and the admin proposals (`crate::proposals`): `POST /internal/admin/proposals/open`,
//! `GET /internal/admin/proposals`, `GET /internal/admin/protection`, plus
//! `POST /internal/admin/events`, where `pmos-web` records what it refused or showed.
//!
//! **Who may call:** the orchestrator (`PMOS_SERVICE_TOKEN`) and `pmos-web` (`PMOS_WEB_TOKEN`).
//! Never the Slack bot or the console. The proposal and event routes are `pmos-web`'s only,
//! and need its signed user assertion in **every** assertion mode, `off` included: a call
//! without one is `401 bad_assertion`, and so is one whose subject isn't the `user=` it names
//! or whose body isn't the one signed. Opening a proposal is the only call here that writes
//! anywhere but the audit log: a new branch and a pull request in GitHub, as the admin.
//!
//! **Whose view:** every call names its viewer as `user=`. The tool server checks that viewer's
//! roles in its own `team.yaml` against the permission matrix (`pmos_contract::permissions`),
//! whatever the caller already checked, and for `pmos-web` the user assertion must name that
//! same person (`PMOS_ASSERTION_MODE`, as on the approvals routes). A viewer without the
//! permission gets `403 {"code":"forbidden"}`.
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
use crate::session::BOT_IDENTITY_PREFIX;
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
        .route("/admin/events", axum::routing::post(record_event))
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
        "/admin/proposals" | "/admin/proposals/open" | "/admin/protection" | "/admin/events"
    )
}

/// `pmos-web`'s assertion on a [`needs_assertion`] route, whatever the mode: present, valid,
/// bound to this exact request (body included, up to [`MAX_PROPOSAL_BODY`]), and its subject
/// the `user=` the call names. Returns the request with its body put back.
async fn require_assertion(
    assertions: &crate::assertion::Assertions,
    client: Client,
    req: Request,
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
    let Ok(bytes) = axum::body::to_bytes(body, MAX_PROPOSAL_BODY).await else {
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
        match require_assertion(assertions, client, req).await {
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

fn coded(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "code": code, "error": message }))).into_response()
}

/// Record one admin read.
async fn record(state: &AppState, client: Client, viewer: &str, tool: &str, outcome: Outcome) {
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
            detail: None,
        })
        .await;
}

/// The viewer `user` names, if they are a team member with any of `any_of`. A refusal is
/// recorded and answered `403 forbidden`; a missing `user` is `400`.
async fn viewer(
    state: &AppState,
    client: Client,
    user: Option<&str>,
    tool: &str,
    any_of: &[Permission],
) -> Result<String, Box<Response>> {
    let Some(user) = user.map(str::trim).filter(|u| !u.is_empty()) else {
        return Err(Box::new(coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "`user` (the viewer) is required",
        )));
    };
    let allowed = state
        .svc
        .team()
        .member(user)
        .is_some_and(|m| any_of.iter().any(|p| m.allows(*p)));
    if !allowed {
        record(state, client, user, tool, Outcome::Denied).await;
        tracing::info!(viewer = %user, tool, "admin read refused: role");
        return Err(Box::new(coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
        )));
    }
    record(state, client, user, tool, Outcome::Ok).await;
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
    Ok(AuditQuery {
        before,
        limit,
        actor: param(&q.actor),
        action: param(&q.action),
        outcome,
        from,
        to,
    })
}

/// The notes the tool server writes itself in `detail`, which carry no content. Anything else
/// (a rejection's free text, a connector's or a refusal's message, which can quote arguments)
/// is never served. The admin proposal and event notes are [`ADMIN_NOTES`].
const FIXED_DETAILS: [&str; 10] = [
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
];

/// The fixed notes of admin refusals and reads (`crate::proposals`, [`record_event`]).
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
/// invalid-draft count, a ledger rejection reason (a fixed label, not free text), or a Slack
/// draft's expiry note.
pub fn safe_detail(detail: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    let ok = FIXED_DETAILS.contains(&detail)
        || ADMIN_NOTES.contains(&detail)
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
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), tool, &[need]).await {
        return *refused;
    }
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
    let events: Vec<Value> = found.iter().map(event_json).collect();
    Json(json!({ "events": events, "next": next })).into_response()
}

// Team accounts.

#[derive(Deserialize)]
struct ViewerParams {
    user: Option<String>,
}

/// `{"linking": bool, "members": [{"handle", "accounts": {"atlassian": bool, ...}}]}`: which
/// accounts each team member has linked, as booleans. Never a token, scope or account id.
async fn accounts(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ViewerParams>,
) -> Response {
    let need = [Permission::ViewAuditLog, Permission::ManageMembers];
    if let Err(refused) = viewer(&state, client, q.user.as_deref(), "admin.accounts", &need).await {
        return *refused;
    }
    let team = state.svc.team().file();
    let mut members = Vec::new();
    for m in team
        .members
        .iter()
        .filter(|m| !m.handle.starts_with(BOT_IDENTITY_PREFIX))
    {
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
        members.push(json!({ "handle": m.handle, "accounts": accounts }));
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

/// `{"since", "pending_drafts", "approved", "rejected", "denied"}`: pending approval items of
/// every team member now, and audit events of each outcome since `since` (`null` when the
/// audit log can't be read back).
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
    let mut pending = 0usize;
    for m in state.svc.team().file().members {
        if m.handle.starts_with(BOT_IDENTITY_PREFIX) {
            continue;
        }
        match state
            .svc
            .stores()
            .approvals
            .list(&m.handle, Some(Status::Pending))
            .await
        {
            Ok(items) => pending += items.len(),
            Err(e) => return unavailable(e),
        }
    }
    let counts = match state.svc.stores().audit.count_outcomes(since).await {
        Ok(c) => c,
        Err(e) => return unavailable(e),
    };
    let count = |o: Outcome| {
        counts
            .as_ref()
            .map(|c| c.get(&o).copied().unwrap_or_default())
    };
    Json(json!({
        "since": since,
        "pending_drafts": pending,
        "approved": count(Outcome::Approved),
        "rejected": count(Outcome::Rejected),
        "denied": count(Outcome::Denied),
    }))
    .into_response()
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
    let allowed =
        state.svc.team().member(user).is_some_and(|m| {
            m.allows(Permission::ViewPolicies) || m.allows(Permission::ViewContent)
        });
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

/// The actions `pmos-web` may record ([`record_event`]).
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

/// `POST /internal/admin/events?user=<viewer>`: record what `pmos-web` refused the viewer
/// (a role refusal on an admin route, a proposal its own checks refused) or an admin's read
/// of a personal or restricted file, which `pmos-web` records **before** it shows the text.
/// The viewer must be a team member; a sensitive read needs `edit-content`. Never file text,
/// and a path only as `content:personal:<handle>` (a restricted file's name can itself be
/// sensitive, so it is never sent).
async fn record_event(
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
    let member = state.svc.team().member(user);
    let allowed = match outcome {
        Outcome::Ok => member.is_some_and(|m| m.allows(Permission::EditContent)),
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
        ] {
            assert_eq!(safe_detail(ok).as_deref(), Some(ok));
        }
        for no in [
            "reason: the launch date is wrong, it's March 3",
            "invalid draft (x of 2)",
            "jira answered 400: summary 'Secret project' is too long",
            "undecided after 24 h: something else",
            "",
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
