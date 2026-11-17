//! The admin area and the auditor's view: `GET /api/admin/{audit, audit/export, team,
//! connectors, overview}` (`docs/web-api.md`, Admin and auditor). All read-only.
//!
//! **Who:** roles come from `team.yaml` through the permission matrix
//! (`pmos_contract::permissions`): an `admin` sees everything; an `auditor` sees the audit log,
//! its export, the team and the overview, never connectors; anyone else gets
//! `403 {"code":"forbidden"}`. The tool server checks the viewer's role again on its side
//! (each call names the viewer and carries a user assertion for them) and records every read
//! in the audit log; pmos-web also logs each read (handle and route, never query values but
//! the names of the filters used).
//!
//! **Metadata only:** audit events are read from the tool server's own metadata projection
//! and re-picked here field by field, so nothing else can pass through: never a draft body,
//! Slack text, a prompt, a result, a source title or a token.

use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use pmos_contract::onboarding::personal_bot_name;
use pmos_contract::permissions::Permission;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{load_team, ApiError, ApiResult, AppState, Signed};
use crate::upstream::UpstreamError;

/// The default and largest page of `GET /api/admin/audit`.
pub const DEFAULT_PAGE: usize = 50;
pub const MAX_PAGE: usize = 100;
/// The most rows one export has; the response says when there were more.
pub const MAX_EXPORT_ROWS: usize = 10_000;
/// The page size the export reads the tool server with.
pub const EXPORT_PAGE: usize = 1000;
/// Set on every export: `true` when rows were left out past [`MAX_EXPORT_ROWS`].
pub const TRUNCATED_HEADER: &str = "x-pmos-truncated";
/// Set on every export: how many rows it has.
pub const ROWS_HEADER: &str = "x-pmos-row-count";

/// Every outcome an audit event can have.
pub const OUTCOMES: [&str; 10] = [
    "ok", "denied", "pending", "approved", "rejected", "granted", "started", "error", "failed",
    "expired",
];

/// The CSV columns, in order: the fields of an event in `GET /api/admin/audit`.
pub const CSV_COLUMNS: [&str; 14] = [
    "at",
    "actor",
    "client",
    "action",
    "system",
    "target",
    "outcome",
    "detail",
    "sources",
    "titled_sources",
    "skill",
    "bot",
    "task_id",
    "id",
];

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/audit", get(audit))
        .route("/admin/audit/export", get(export))
        .route("/admin/team", get(team))
        .route("/admin/connectors", get(connectors))
        .route("/admin/overview", get(overview))
}

fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "Only an admin or an auditor can see this.",
    )
}

/// The viewer has at least one of `any_of`.
fn need_any(signed: &Signed, any_of: &[Permission]) -> Result<(), ApiError> {
    if any_of.iter().any(|p| signed.member.allows(*p)) {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// A tool server answer for an admin read: its own role refusal is `403 forbidden`, a bad
/// parameter `400 bad_request`; anything else as for every upstream failure.
fn upstream(e: UpstreamError) -> ApiError {
    match e {
        UpstreamError::Refused {
            status: 403,
            code: Some(code),
            ..
        } if code == "forbidden" => forbidden(),
        UpstreamError::Refused {
            status: 400, error, ..
        } => ApiError::bad_request(
            error.unwrap_or_else(|| "The tool server didn't take these parameters.".into()),
        ),
        other => ApiError::upstream(other),
    }
}

fn read_log(signed: &Signed, route: &str, filters: &[&str]) {
    tracing::info!(user = %signed.member.handle, route, filters = ?filters, "admin read");
}

// The audit log.

#[derive(Deserialize, Default)]
struct AuditParams {
    before: Option<String>,
    limit: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

/// One audit event as the app gets it. Only these fields are ever served.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuditRow {
    pub id: String,
    pub at: String,
    pub actor: String,
    pub client: String,
    pub action: String,
    pub system: String,
    #[serde(default)]
    pub target: Option<String>,
    pub outcome: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub sources: u64,
    #[serde(default)]
    pub titled_sources: u64,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub bot: Option<String>,
    #[serde(default)]
    pub task_id: String,
}

#[derive(Deserialize)]
struct AuditPage {
    #[serde(default)]
    events: Vec<AuditRow>,
    #[serde(default)]
    next: Option<String>,
}

fn trimmed(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

fn token_like(v: &str, max: usize) -> bool {
    v.len() <= max
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn time(name: &str, v: Option<&str>) -> Result<Option<String>, ApiError> {
    match v {
        None => Ok(None),
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|t| {
                Some(
                    t.with_timezone(&Utc)
                        .to_rfc3339_opts(SecondsFormat::AutoSi, true),
                )
            })
            .map_err(|_| {
                ApiError::bad_request(format!(
                    "`{name}` must be an RFC 3339 time, such as 2026-11-16T00:00:00Z."
                ))
            }),
    }
}

/// Query parameters for the tool server, by name.
type UpstreamParams = Vec<(&'static str, String)>;

/// The filters, checked, as the tool server's parameters (`before` and `limit` aside), and
/// the names of those used, for the log.
fn filters(q: &AuditParams) -> Result<(UpstreamParams, Vec<&'static str>), ApiError> {
    let mut out = Vec::new();
    if let Some(actor) = trimmed(&q.actor) {
        if !token_like(actor, 64) {
            return Err(ApiError::bad_request("`actor` must be a handle."));
        }
        out.push(("actor", actor.to_string()));
    }
    if let Some(action) = trimmed(&q.action) {
        if !token_like(action, 100) {
            return Err(ApiError::bad_request(
                "`action` must be a tool name, such as jira.create_issue.",
            ));
        }
        out.push(("action", action.to_string()));
    }
    if let Some(outcome) = trimmed(&q.outcome) {
        if !OUTCOMES.contains(&outcome) {
            return Err(ApiError::bad_request(format!(
                "`outcome` is one of {}.",
                OUTCOMES.join(", ")
            )));
        }
        out.push(("outcome", outcome.to_string()));
    }
    let from = time("from", trimmed(&q.from))?;
    let to = time("to", trimmed(&q.to))?;
    if let (Some(f), Some(t)) = (&from, &to) {
        let (f, t) = (
            DateTime::parse_from_rfc3339(f).ok(),
            DateTime::parse_from_rfc3339(t).ok(),
        );
        if f > t {
            return Err(ApiError::bad_request("`from` must not be after `to`."));
        }
    }
    if let Some(f) = from {
        out.push(("from", f));
    }
    if let Some(t) = to {
        out.push(("to", t));
    }
    let names = out.iter().map(|(k, _)| *k).collect();
    Ok((out, names))
}

fn bad_params() -> ApiError {
    ApiError::bad_request("These query parameters don't parse.")
}

type Params = Result<Query<AuditParams>, axum::extract::rejection::QueryRejection>;

async fn read_page(
    state: &AppState,
    signed: &Signed,
    params: &[(&str, String)],
) -> Result<AuditPage, ApiError> {
    let query: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let value = state
        .0
        .toolserver
        .admin(&signed.member.handle, "audit", &query)
        .await
        .map_err(upstream)?;
    serde_json::from_value(value).map_err(|_| {
        ApiError::upstream(UpstreamError::Failed {
            service: "tool server",
            why: "sent a reply pmos-web can't read".into(),
        })
    })
}

/// `GET /api/admin/audit`: `{"events": [...], "next_cursor": "<opaque>" | null}`.
async fn audit(State(state): State<AppState>, signed: Signed, q: Params) -> ApiResult {
    need_any(&signed, &[Permission::ViewAuditLog])?;
    let Ok(Query(q)) = q else {
        return Err(bad_params());
    };
    let (mut params, names) = filters(&q)?;
    let limit = match trimmed(&q.limit) {
        None => DEFAULT_PAGE,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_PAGE).contains(n))
            .ok_or_else(|| {
                ApiError::bad_request(format!("`limit` must be from 1 to {MAX_PAGE}."))
            })?,
    };
    params.push(("limit", limit.to_string()));
    if let Some(before) = trimmed(&q.before) {
        if !token_like(before, 128) {
            return Err(ApiError::bad_request(
                "`before` must be a `next_cursor` this API gave.",
            ));
        }
        params.push(("before", before.to_string()));
    }
    read_log(&signed, "/api/admin/audit", &names);
    let page = read_page(&state, &signed, &params).await?;
    Ok(Json(json!({ "events": page.events, "next_cursor": page.next })).into_response())
}

/// A CSV cell: a cell that a spreadsheet would read as a formula (starting with `=`, `+`,
/// `-`, `@`, a tab or a carriage return) gets a leading `'`; every cell is quoted, with `"`
/// doubled.
pub fn csv_cell(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

fn csv_row(cells: &[String]) -> String {
    let mut line = cells
        .iter()
        .map(|c| csv_cell(c))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

fn row_cells(r: &AuditRow) -> Vec<String> {
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    vec![
        r.at.clone(),
        r.actor.clone(),
        r.client.clone(),
        r.action.clone(),
        r.system.clone(),
        opt(&r.target),
        r.outcome.clone(),
        opt(&r.detail),
        r.sources.to_string(),
        r.titled_sources.to_string(),
        opt(&r.skill),
        opt(&r.bot),
        r.task_id.clone(),
        r.id.clone(),
    ]
}

/// `GET /api/admin/audit/export`: the same events as CSV, newest first, at most
/// [`MAX_EXPORT_ROWS`].
async fn export(State(state): State<AppState>, signed: Signed, q: Params) -> ApiResult {
    need_any(&signed, &[Permission::ExportAudit])?;
    let Ok(Query(q)) = q else {
        return Err(bad_params());
    };
    let (base, names) = filters(&q)?;
    read_log(&signed, "/api/admin/audit/export", &names);
    let mut body = csv_row(&CSV_COLUMNS.map(String::from));
    let mut rows = 0usize;
    let mut cursor: Option<String> = None;
    let truncated = loop {
        let mut params = base.clone();
        params.push(("purpose", "export".into()));
        params.push(("limit", EXPORT_PAGE.min(MAX_EXPORT_ROWS - rows).to_string()));
        if let Some(c) = &cursor {
            params.push(("before", c.clone()));
        }
        let page = read_page(&state, &signed, &params).await?;
        for r in &page.events {
            body.push_str(&csv_row(&row_cells(r)));
        }
        rows += page.events.len();
        match page.next {
            None => break false,
            Some(_) if rows >= MAX_EXPORT_ROWS => break true,
            // A page with nothing in it can't move the cursor: stop rather than loop.
            Some(_) if page.events.is_empty() => break false,
            Some(next) => cursor = Some(next),
        }
    };
    if truncated {
        tracing::info!(user = %signed.member.handle, rows, "audit export truncated");
    }
    let mut res = (StatusCode::OK, body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"pmos-audit.csv\""),
    );
    h.insert(
        TRUNCATED_HEADER,
        HeaderValue::from_static(if truncated { "true" } else { "false" }),
    );
    if let Ok(v) = HeaderValue::from_str(&rows.to_string()) {
        h.insert(ROWS_HEADER, v);
    }
    Ok(res)
}

// The team.

#[derive(Deserialize)]
struct AccountsReply {
    #[serde(default)]
    linking: bool,
    #[serde(default)]
    members: Vec<MemberAccounts>,
}

#[derive(Deserialize)]
struct MemberAccounts {
    handle: String,
    #[serde(default)]
    accounts: std::collections::BTreeMap<String, bool>,
}

/// The account kinds the app shows, as the tool server names them: Atlassian covers Jira and
/// Confluence.
const ACCOUNT_KINDS: [&str; 3] = ["atlassian", "slack", "github"];

/// `GET /api/admin/team`: every member in `team.yaml`, with which accounts they've linked as
/// booleans when the tool server answers (else `accounts: null`).
async fn team(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need_any(
        &signed,
        &[Permission::ViewAuditLog, Permission::ManageMembers],
    )?;
    read_log(&signed, "/api/admin/team", &[]);
    let file = load_team(&state)?;
    let linked = match state
        .0
        .toolserver
        .admin(&signed.member.handle, "accounts", &[])
        .await
    {
        Ok(v) => serde_json::from_value::<AccountsReply>(v).ok(),
        Err(e @ UpstreamError::Refused { status: 403, .. }) => return Err(upstream(e)),
        Err(e) => {
            tracing::warn!(error = ?e, "linked accounts unavailable");
            None
        }
    };
    let members: Vec<Value> = file
        .members
        .iter()
        .filter(|m| !m.handle.starts_with("bots."))
        .map(|m| {
            let accounts = linked.as_ref().map(|l| {
                let of = l.members.iter().find(|a| a.handle == m.handle);
                let map: serde_json::Map<String, Value> = ACCOUNT_KINDS
                    .iter()
                    .map(|k| {
                        let on = of.and_then(|a| a.accounts.get(*k)).copied() == Some(true);
                        (k.to_string(), json!(on))
                    })
                    .collect();
                map
            });
            json!({
                "handle": m.handle,
                // team.yaml has no display name: the handle, as in `GET /api/me`.
                "name": m.handle,
                "roles": m.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                "has_email": m.email.as_deref().is_some_and(|e| !e.trim().is_empty()),
                "bot": personal_bot_name(&m.handle),
                "accounts": accounts,
            })
        })
        .collect();
    Ok(Json(json!({
        "members": members,
        "accounts_available": linked.is_some(),
        "linking": linked.as_ref().map(|l| l.linking),
    }))
    .into_response())
}

// Connectors.

#[derive(Deserialize, Serialize)]
struct ConnectorRow {
    name: String,
    mode: String,
    health: String,
    #[serde(default)]
    error_class: Option<String>,
    checked_at: String,
}

#[derive(Deserialize)]
struct ConnectorsReply {
    #[serde(default)]
    connectors: Vec<ConnectorRow>,
}

/// `GET /api/admin/connectors`: admins only.
async fn connectors(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need_any(&signed, &[Permission::ManageConnectors])?;
    read_log(&signed, "/api/admin/connectors", &[]);
    let v = state
        .0
        .toolserver
        .admin(&signed.member.handle, "connectors", &[])
        .await
        .map_err(upstream)?;
    let reply: ConnectorsReply = serde_json::from_value(v).map_err(|_| {
        ApiError::upstream(UpstreamError::Failed {
            service: "tool server",
            why: "sent a reply pmos-web can't read".into(),
        })
    })?;
    Ok(Json(json!({ "connectors": reply.connectors })).into_response())
}

// Overview.

#[derive(Deserialize, Serialize)]
struct OverviewReply {
    since: String,
    pending_drafts: u64,
    #[serde(default)]
    approved: Option<u64>,
    #[serde(default)]
    rejected: Option<u64>,
    #[serde(default)]
    denied: Option<u64>,
    /// How many days audit events are kept (`PMOS_AUDIT_RETENTION_DAYS`); `null` from a tool
    /// server older than retention.
    #[serde(default)]
    retention_days: Option<u32>,
    /// The oldest event still in the audit log; `null` when it is empty or can't be read back.
    #[serde(default)]
    oldest_event_at: Option<String>,
    /// The newest purge batch; `null` before the first.
    #[serde(default)]
    last_purge: Option<LastPurge>,
}

#[derive(Deserialize, Serialize)]
struct LastPurge {
    at: String,
    deleted: u64,
}

/// `GET /api/admin/overview`: counts for the admin dashboard.
async fn overview(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need_any(
        &signed,
        &[Permission::ViewAuditLog, Permission::ManageMembers],
    )?;
    read_log(&signed, "/api/admin/overview", &[]);
    let v = state
        .0
        .toolserver
        .admin(&signed.member.handle, "overview", &[])
        .await
        .map_err(upstream)?;
    let reply: OverviewReply = serde_json::from_value(v).map_err(|_| {
        ApiError::upstream(UpstreamError::Failed {
            service: "tool server",
            why: "sent a reply pmos-web can't read".into(),
        })
    })?;
    Ok(Json(reply).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_cannot_become_formulas() {
        assert_eq!(csv_cell("ana"), "\"ana\"");
        assert_eq!(csv_cell("=1+1"), "\"'=1+1\"");
        assert_eq!(csv_cell("+1"), "\"'+1\"");
        assert_eq!(csv_cell("-1"), "\"'-1\"");
        assert_eq!(csv_cell("@SUM(A1)"), "\"'@SUM(A1)\"");
        assert_eq!(csv_cell("\tx"), "\"'\tx\"");
        assert_eq!(csv_cell("\rx"), "\"'\rx\"");
        assert_eq!(csv_cell("a\"b,c"), "\"a\"\"b,c\"");
        assert_eq!(csv_cell(""), "\"\"");
    }
}
