//! The admin and auditor reads: `GET /internal/admin/{audit,accounts,connectors,overview}`.
//!
//! **Who may call:** the orchestrator (`PMOS_SERVICE_TOKEN`) and `pmos-web` (`PMOS_WEB_TOKEN`).
//! Never the Slack bot or the console. These routes only read; there is no write here.
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

use axum::extract::{Query, Request, State};
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
        .layer(middleware::from_fn_with_state(state, identify_admin_client))
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
    if client == Client::Web && assertions.mode() != Mode::Off {
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
/// is never served.
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

/// `detail` if it is metadata only: one of [`FIXED_DETAILS`], an invalid-draft count, a
/// ledger rejection reason (a fixed label, not free text), or a Slack draft's expiry note.
pub fn safe_detail(detail: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    let ok = FIXED_DETAILS.contains(&detail)
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
