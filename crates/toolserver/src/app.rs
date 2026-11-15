//! The HTTP application: `/mcp` for agent sandboxes, `/internal` for the orchestrator and
//! the approval inbox, `/healthz` for the platform-owner checks.
//!
//! - `/mcp` needs a session token (`Authorization: Bearer pmos_s_…`).
//! - `/internal` needs a service token, and each internal client has its own:
//!   - `PMOS_SERVICE_TOKEN`: the orchestrator, the bot runtime and platform-owner tools.
//!     Every route, and the only token that starts bot sessions.
//!   - `PMOS_BOT_TOKEN`: the Slack bot. Only sessions for its skills (`PMOS_BOT_SKILLS`) and
//!     calls within them, reading and deciding one approval item (Slack approval cards),
//!     onboarding (member requests and decisions, memory repos, setup status), link URLs
//!     for team members, a team member's pending approvals (the App Home Inbox) and
//!     unlinking a team member's account (the App Home Accounts), and reading the feedback
//!     ledger (the triage digest); never other inbox listings, grants, revoking sessions,
//!     listing credentials, reindexing or the ledger summary.
//!
//!   The sandbox network policy never routes to `/internal`.
//! - Session tokens travel in headers (`Authorization` on `/mcp`, `X-Pmos-Session` on
//!   `/internal`), never in URLs, so access logs and proxies don't record them.

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, Utc};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::approvals::{DecideError, Decision, Status, Verdict};
use crate::audit::{AuditEvent, Client, Outcome};
use crate::ledger::{LedgerOutcome, LedgerQuery, Reason, MAX_LIMIT};
use crate::mcp::PmosMcp;
use crate::members::RequestStatus;
use crate::oauth::{OAuth, OAuthError};
use crate::onboarding::{DecisionInput, Onboarding, Refusal};
use crate::service::{
    DecisionError, ReindexError, SessionError, ToolError, ToolService, AUDIENCE_BLOCKED,
    AUDIENCE_UNAVAILABLE, NEEDS_EXTERNAL_CONFIRM,
};
use crate::session::BOT_IDENTITY_PREFIX;
use crate::store::StoreError;
use crate::vault::Provider;
use pmos_contract::bots::BotScope;

/// What the Slack bot's token may do.
#[derive(Clone, Debug)]
pub struct BotAccess {
    pub token: String,
    /// Skills the bot may create sessions for, such as `docs`.
    pub skills: Vec<String>,
}

/// The header that carries a session token on `/internal`.
pub const SESSION_HEADER: &str = "x-pmos-session";

#[derive(Clone)]
pub struct AppConfig {
    pub service_token: String,
    /// The Slack bot's own token and scope, if the bot is deployed.
    pub bot: Option<BotAccess>,
    /// The content repo checkout `POST /internal/reindex` reads.
    pub content_root: std::path::PathBuf,
    /// `Host` values the MCP endpoint accepts (DNS-rebinding protection).
    pub allowed_hosts: Vec<String>,
    /// Account linking, when live connectors are on. `None` with fake connectors.
    pub oauth: Option<Arc<OAuth>>,
}

#[derive(Clone)]
struct AppState {
    svc: Arc<ToolService>,
    service_token: Arc<str>,
    bot: Option<Arc<BotAccess>>,
    content_root: Arc<std::path::Path>,
    oauth: Option<Arc<OAuth>>,
    onboarding: Arc<Onboarding>,
}

/// The app, with onboarding but no GitHub: member requests can be made and rejected, and
/// approving one answers `not_configured`.
pub fn router(svc: Arc<ToolService>, config: AppConfig, cancel: CancellationToken) -> Router {
    router_with(svc, config, None, cancel)
}

/// The app with `onboarding` set up (GitHub, the content repo, content sync), as `main`
/// builds it. `None` is the same as [`router`].
pub fn router_with(
    svc: Arc<ToolService>,
    config: AppConfig,
    onboarding: Option<Arc<Onboarding>>,
    cancel: CancellationToken,
) -> Router {
    let onboarding = onboarding
        .unwrap_or_else(|| Arc::new(Onboarding::new(svc.clone(), config.oauth.clone(), None)));
    let state = AppState {
        svc: svc.clone(),
        service_token: config.service_token.into(),
        bot: config.bot.map(Arc::new),
        content_root: config.content_root.into(),
        oauth: config.oauth.clone(),
        onboarding,
    };

    let mcp_svc = svc.clone();
    // An MCP session lives as long as the PM OS session, so a long model step between tool
    // calls (rmcp's default idle limit is 5 minutes) doesn't drop it mid-run.
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(std::time::Duration::from_secs(
        (crate::service::DEFAULT_SESSION_TTL_MINUTES * 60) as u64,
    ));
    let mcp = StreamableHttpService::new(
        move || Ok(PmosMcp::new(mcp_svc.clone())),
        sessions.into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(config.allowed_hosts)
            .with_cancellation_token(cancel.child_token()),
    );
    let mcp = Router::new()
        .fallback_service(mcp)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    // Orchestrator only.
    let admin = Router::new()
        .route("/grants", post(create_grant))
        .route("/reindex", post(reindex))
        .route("/ledger/summary", get(ledger_summary))
        .route("/credentials", get(list_credentials))
        .layer(middleware::from_fn(orchestrator_only));

    // Any internal client; the bot is limited to its own skills inside each handler. Both
    // may read and decide one approval item: the Slack bot does so from approval cards. Both
    // run onboarding, and the bot gets link URLs for team members only. Both read the
    // ledger: the Slack bot for the daily triage digest. The Slack bot may also list a team
    // member's pending approvals and unlink a team member's account, for the App Home
    // (`docs/slack-mentions-and-home-spec.md` §3); each handler limits it.
    let internal = Router::new()
        .route("/approvals", get(list_approvals))
        .route(
            "/credentials/{user}/{provider}",
            axum::routing::delete(revoke_credential),
        )
        .route("/sessions", post(create_session))
        .route("/session", axum::routing::delete(revoke_session))
        .route("/call", post(call_tool))
        .route("/approvals/{id}", get(get_approval))
        .route("/approvals/{id}/decision", post(decide))
        .route("/ledger", get(ledger))
        .route("/oauth/{provider}/start", get(oauth_start))
        .route("/memory/repo", post(set_memory_repo).get(get_memory_repo))
        .route(
            "/members/requests",
            post(create_member_request).get(list_member_requests),
        )
        .route(
            "/members/requests/{id}/decision",
            post(decide_member_request),
        )
        .route("/members/{handle}/remove", post(remove_member))
        .route("/onboarding", get(onboarding_status))
        .merge(admin)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            identify_client,
        ));

    Router::new()
        // Liveness: the process answers. Readiness: its dependencies do too.
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        // Public: the PM's browser lands here from Atlassian or Slack.
        .route("/oauth/{provider}/callback", get(oauth_callback))
        .nest("/internal", internal)
        .nest("/mcp", mcp)
        .with_state(state)
}

fn bearer(req: &Request) -> Option<&str> {
    pmos_runtime::bearer_value(req.headers().get(header::AUTHORIZATION)?.to_str().ok()?)
}

/// `200` when storage and every connector dependency answer, `503` otherwise, with each
/// check's result. No auth: it says only which part is down, not why in detail.
async fn readyz(State(state): State<AppState>) -> Response {
    let checks = state.svc.readiness().await;
    let ok = checks.iter().all(|(_, r)| r.is_ok());
    for (name, result) in &checks {
        if let Err(e) = result {
            tracing::warn!(check = %name, error = %e, "not ready");
        }
    }
    let body: serde_json::Map<String, Value> = checks
        .into_iter()
        .map(|(name, r)| (name, json!(if r.is_ok() { "ok" } else { "down" })))
        .collect();
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(json!({ "ready": ok, "checks": body }))).into_response()
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// Storage is down. The detail goes to the log, not the caller.
fn unavailable(e: StoreError) -> Response {
    tracing::error!(error = %e, "storage unavailable");
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "PM OS storage is unavailable",
    )
}

async fn require_session(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(token) = bearer(&req) else {
        return error(StatusCode::UNAUTHORIZED, "missing or expired PM OS session");
    };
    let session = match state.svc.session(token, Utc::now()).await {
        Ok(Some(session)) => session,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "missing or expired PM OS session"),
        Err(e) => return unavailable(e),
    };
    req.extensions_mut().insert(session);
    next.run(req).await
}

/// Which internal client is calling, from its token.
async fn identify_client(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let client = match bearer(&req) {
        Some(t) if pmos_runtime::token_eq(t, &state.service_token) => Client::Orchestrator,
        Some(t)
            if state
                .bot
                .as_ref()
                .is_some_and(|b| pmos_runtime::token_eq(t, &b.token)) =>
        {
            Client::Bot
        }
        _ => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    req.extensions_mut().insert(client);
    next.run(req).await
}

async fn orchestrator_only(req: Request, next: Next) -> Response {
    match req.extensions().get::<Client>() {
        Some(Client::Orchestrator) => next.run(req).await,
        _ => error(
            StatusCode::FORBIDDEN,
            "this token may not use this endpoint",
        ),
    }
}

/// May `client` work with sessions for `skill`? The orchestrator for any skill and every
/// bot session (`skill: None`); the Slack bot only for its own skills.
fn client_may_use(state: &AppState, client: Client, skill: Option<&str>) -> bool {
    match (client, skill) {
        (Client::Orchestrator, _) => true,
        (Client::Bot, Some(skill)) => state
            .bot
            .as_ref()
            .is_some_and(|b| b.skills.iter().any(|s| s == skill)),
        (Client::Bot, None) | (Client::Mcp, _) => false,
    }
}

/// The session named by the `X-Pmos-Session` header, if `client` may use it.
async fn header_session(
    state: &AppState,
    client: Client,
    headers: &axum::http::HeaderMap,
) -> Result<crate::session::Session, Box<Response>> {
    let token = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            Box::new(error(
                StatusCode::BAD_REQUEST,
                "missing X-Pmos-Session header",
            ))
        })?;
    match state.svc.session(token, Utc::now()).await {
        Ok(Some(session)) if client_may_use(state, client, session.skill.as_deref()) => Ok(session),
        // A session the client may not use looks the same as one that doesn't exist.
        Ok(_) => Err(Box::new(error(StatusCode::NOT_FOUND, "no such session"))),
        Err(e) => Err(Box::new(unavailable(e))),
    }
}

/// `{user, skill, run_id, ttl_minutes?}` or `{user, bot, run_id, ttl_minutes?}`.
#[derive(Deserialize)]
struct NewSession {
    user: String,
    skill: Option<String>,
    bot: Option<String>,
    run_id: String,
    ttl_minutes: Option<i64>,
}

async fn create_session(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Json(body): Json<NewSession>,
) -> Response {
    if body.skill.is_some() == body.bot.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "send either `skill` or `bot`, not both",
        );
    }
    if !client_may_use(&state, client, body.skill.as_deref()) {
        let what = match (&body.skill, &body.bot) {
            (Some(skill), _) => format!("`{skill}` sessions"),
            (None, _) => "bot sessions".to_string(),
        };
        return error(
            StatusCode::FORBIDDEN,
            format!("this token may not start {what}"),
        );
    }
    let ttl = body
        .ttl_minutes
        .map(|m| Duration::minutes(m.clamp(1, 24 * 60)));
    let created = match (&body.skill, &body.bot) {
        (Some(skill), _) => {
            state
                .svc
                .create_session(&body.user, skill, &body.run_id, ttl, Utc::now())
                .await
        }
        (None, Some(bot)) => {
            state
                .svc
                .create_bot_session(&body.user, bot, &body.run_id, ttl, Utc::now())
                .await
        }
        (None, None) => unreachable!("checked above"),
    };
    match created {
        Ok(session) => {
            let tools: Vec<_> = state
                .svc
                .list_tools(&session)
                .iter()
                .map(|t| json!({"name": t.harness_name, "kind": t.kind}))
                .collect();
            (
                StatusCode::CREATED,
                Json(json!({
                    "token": session.token,
                    "session": session,
                    "tools": tools,
                })),
            )
                .into_response()
        }
        Err(e @ (SessionError::UnknownSkill(_) | SessionError::UnknownBot(_))) => {
            error(StatusCode::NOT_FOUND, e.to_string())
        }
        Err(e @ (SessionError::NotAMember(_) | SessionError::NotOwner { .. })) => {
            error(StatusCode::FORBIDDEN, e.to_string())
        }
        Err(SessionError::Store(e)) => unavailable(e),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn revoke_session(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    headers: axum::http::HeaderMap,
) -> Response {
    let session = match header_session(&state, client, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match state.svc.revoke_session(&session.token).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "no such session"),
        Err(e) => unavailable(e),
    }
}

#[derive(Deserialize)]
struct NewGrant {
    session_token: String,
    tool: String,
    #[serde(default)]
    args: Value,
}

async fn create_grant(State(state): State<AppState>, Json(body): Json<NewGrant>) -> Response {
    let now = Utc::now();
    let session = match state.svc.session(&body.session_token, now).await {
        Ok(Some(session)) => session,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no such session"),
        Err(e) => return unavailable(e),
    };
    match state.svc.grant(&session, &body.tool, &body.args, now).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e @ (ToolError::NotAllowed(_) | ToolError::NotInRole(_) | ToolError::Forbidden(_))) => {
            error(StatusCode::FORBIDDEN, e.to_string())
        }
        Err(ToolError::Unavailable(e)) => unavailable(StoreError(e)),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ApprovalQuery {
    user: Option<String>,
    status: Option<String>,
}

/// The most pending items the Slack bot gets in one listing.
pub const BOT_PENDING_LIMIT: usize = 50;

/// A PM's approval items.
///
/// - **The orchestrator:** `{"items": [...]}`, every item of `user`'s, newest first,
///   optionally only those with `status`.
/// - **The Slack bot** (the App Home Inbox): `{"approvals": [...], "total": n}`, only
///   `pending` items whatever `status` says, oldest first, at most [`BOT_PENDING_LIMIT`];
///   `total` counts them all. `user` must be a team member.
async fn list_approvals(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Query(q): Query<ApprovalQuery>,
) -> Response {
    let Some(user) = q.user.filter(|u| !u.trim().is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "`user` is required");
    };
    if client == Client::Bot {
        // As for sessions: enforced with live connectors, open in dev and tests.
        if !state.svc.team().allows(&user) {
            return error(
                StatusCode::FORBIDDEN,
                "this token may list approvals for team members only",
            );
        }
        return match state.svc.pending_approvals(&user, BOT_PENDING_LIMIT).await {
            Ok((approvals, total)) => {
                Json(json!({ "approvals": approvals, "total": total })).into_response()
            }
            Err(e) => unavailable(e),
        };
    }
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => match Status::parse(s) {
            Some(status) => Some(status),
            None => return error(StatusCode::BAD_REQUEST, format!("unknown status `{s}`")),
        },
    };
    match state.svc.approvals(&user, status).await {
        Ok(items) => Json(json!({ "items": items })).into_response(),
        Err(e) => unavailable(e),
    }
}

async fn get_approval(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.svc.approval(&id).await {
        Ok(Some(item)) => Json(item).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such approval item"),
        Err(e) => unavailable(e),
    }
}

#[derive(Deserialize)]
struct DecisionBody {
    decided_by: String,
    decision: Decision,
    args: Option<Value>,
    /// Only with `reject`: one of the ledger's reasons.
    #[serde(default)]
    reason: Option<String>,
    /// For a Slack write to an externally shared channel: the PM confirmed posting there.
    #[serde(default)]
    confirm_external: bool,
}

/// An error with a machine-readable `code`, and any extra fields.
fn coded(status: StatusCode, code: &str, message: String, extra: Value) -> Response {
    let mut body = json!({ "code": code, "error": message });
    if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
        body.extend(extra);
    }
    (status, Json(body)).into_response()
}

async fn decide(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
    Json(body): Json<DecisionBody>,
) -> Response {
    let reason = match body.reason.as_deref() {
        None => None,
        Some(r) => match Reason::parse(r) {
            Some(reason) => Some(reason),
            None => {
                let known: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("unknown reason `{r}`: use one of {}", known.join(", ")),
                );
            }
        },
    };
    let verdict = Verdict {
        decision: body.decision,
        args: body.args,
        reason,
        confirm_external: body.confirm_external,
    };
    match state
        .svc
        .decide_verdict(client, &id, &body.decided_by, verdict, Utc::now())
        .await
    {
        Ok(item) => Json(item).into_response(),
        Err(DecisionError::Decide(DecideError::Store(e))) => unavailable(e),
        Err(e @ DecisionError::Decide(DecideError::Expired)) => {
            coded(StatusCode::CONFLICT, "expired", e.to_string(), json!({}))
        }
        Err(ref e @ DecisionError::AudienceBlocked(a)) => coded(
            StatusCode::CONFLICT,
            AUDIENCE_BLOCKED,
            e.to_string(),
            json!({"outsiders": a.outsiders, "members": a.members, "external": a.external}),
        ),
        Err(ref e @ DecisionError::NeedsExternalConfirm(a)) => coded(
            StatusCode::CONFLICT,
            NEEDS_EXTERNAL_CONFIRM,
            e.to_string(),
            json!({"members": a.members, "external": a.external}),
        ),
        Err(e @ DecisionError::AudienceUnavailable(_)) => coded(
            StatusCode::SERVICE_UNAVAILABLE,
            AUDIENCE_UNAVAILABLE,
            e.to_string(),
            json!({}),
        ),
        Err(DecisionError::Decide(e)) => {
            let status = match e {
                DecideError::NotFound => StatusCode::NOT_FOUND,
                DecideError::Forbidden => StatusCode::FORBIDDEN,
                DecideError::NotPending | DecideError::Expired => StatusCode::CONFLICT,
                DecideError::MissingArgs => StatusCode::BAD_REQUEST,
                DecideError::Store(_) => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(status, e.to_string())
        }
        Err(e @ DecisionError::InvalidArgs(_)) => error(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e @ DecisionError::ReasonNotAllowed) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
        Err(e @ DecisionError::NotAMember(_)) => error(StatusCode::FORBIDDEN, e.to_string()),
        Err(
            e @ (DecisionError::UnknownTool(_)
            | DecisionError::NotAllowed(_)
            | DecisionError::BotNotAllowed(_)),
        ) => error(StatusCode::CONFLICT, e.to_string()),
        Err(e @ DecisionError::Internal(_)) => {
            tracing::error!(error = %e, "approved write task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

// ----- the feedback ledger (docs/improvement-loop-spec.md §1) -------------------------------

#[derive(Deserialize)]
struct LedgerParams {
    since: Option<String>,
    until: Option<String>,
    bot: Option<String>,
    /// Comma-separated outcomes.
    outcome: Option<String>,
    min_edit: Option<String>,
    limit: Option<String>,
}

fn unprocessable(message: impl Into<String>) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, message)
}

/// An optional RFC 3339 time parameter; empty is the same as absent.
fn time_param(name: &str, value: Option<&str>) -> Result<Option<DateTime<Utc>>, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| format!("`{name}` must be an RFC 3339 time")),
    }
}

/// The ledger query from the parameters, or why they don't make one (`422`).
fn ledger_query(q: &LedgerParams) -> Result<LedgerQuery, String> {
    let since = time_param("since", q.since.as_deref())?;
    let until = time_param("until", q.until.as_deref())?;
    let mut outcomes = Vec::new();
    for name in q
        .outcome
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|o| !o.is_empty())
    {
        let outcome = LedgerOutcome::parse(name).ok_or_else(|| {
            let known: Vec<&str> = LedgerOutcome::ALL.iter().map(|o| o.as_str()).collect();
            format!("unknown outcome `{name}`: use {}", known.join(", "))
        })?;
        outcomes.push(outcome);
    }
    let param = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
    };
    let min_edit = match param(&q.min_edit) {
        None => None,
        Some(v) => Some(
            v.parse::<f64>()
                .ok()
                .filter(|d| (0.0..=1.0).contains(d))
                .ok_or("`min_edit` must be a number from 0 to 1")?,
        ),
    };
    let limit = match param(&q.limit) {
        None => MAX_LIMIT,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or(format!("`limit` must be from 1 to {MAX_LIMIT}"))?,
    };
    Ok(LedgerQuery {
        since,
        until,
        bot: param(&q.bot),
        outcomes,
        min_edit,
        limit: Some(limit),
        // Set by the service from PMOS_LEDGER_EXPIRE_DAYS.
        expire_before: Utc::now(),
    })
}

/// `{rows: [...]}`, newest first. Labels and metadata only: no draft or message text.
async fn ledger(State(state): State<AppState>, Query(q): Query<LedgerParams>) -> Response {
    let query = match ledger_query(&q) {
        Ok(query) => query,
        Err(why) => return unprocessable(why),
    };
    match state.svc.ledger(query, Utc::now()).await {
        Ok(rows) => Json(json!({ "rows": rows })).into_response(),
        Err(e) => unavailable(e),
    }
}

#[derive(Deserialize)]
struct SummaryParams {
    since: Option<String>,
    bot: Option<String>,
}

/// Counts, label coverage, median edit distance and rejection reasons per bot and tool.
async fn ledger_summary(State(state): State<AppState>, Query(q): Query<SummaryParams>) -> Response {
    let since = match time_param("since", q.since.as_deref()) {
        Ok(since) => since,
        Err(why) => return unprocessable(why),
    };
    let bot = q.bot.filter(|b| !b.trim().is_empty());
    match state.svc.ledger_summary(since, bot, Utc::now()).await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => unavailable(e),
    }
}

/// Rebuild the context index after a merge to the content repo.
async fn reindex(State(state): State<AppState>) -> Response {
    match state.svc.reindex(&state.content_root, Utc::now()).await {
        Ok(report) => {
            tracing::info!(
                indexed = report.indexed,
                removed = report.removed,
                "context reindexed"
            );
            Json(report).into_response()
        }
        Err(e @ ReindexError::Lint(_)) => error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        Err(ReindexError::Store(e)) => unavailable(e),
    }
}

fn linking(
    state: &AppState,
    provider: &str,
) -> Result<(Arc<OAuth>, Provider), (StatusCode, &'static str)> {
    let provider = Provider::parse(provider).ok_or((StatusCode::NOT_FOUND, "unknown provider"))?;
    let oauth = state.oauth.clone().ok_or((
        StatusCode::NOT_FOUND,
        "account linking is off: PMOS_CONNECTORS is not `live`",
    ))?;
    Ok((oauth, provider))
}

#[derive(Deserialize)]
struct UserQuery {
    user: String,
}

/// The URL to send a PM to so they can link their account. With `user=bots.<name>`, the URL
/// a platform owner uses to link a team bot's own read-only account (service token only).
/// The Slack bot may ask only for team members.
async fn oauth_start(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(provider): Path<String>,
    Query(q): Query<UserQuery>,
) -> Response {
    if client != Client::Orchestrator && state.svc.team().member(&q.user).is_none() {
        return error(
            StatusCode::FORBIDDEN,
            "this token may get link URLs for team members only",
        );
    }
    let (oauth, provider) = match linking(&state, &provider) {
        Ok(x) => x,
        Err((status, message)) => return error(status, message),
    };
    if let Some(name) = q.user.strip_prefix(BOT_IDENTITY_PREFIX) {
        let team_bot = state
            .svc
            .roster()
            .bot(name)
            .is_some_and(|(bot, _)| bot.scope == BotScope::Team);
        if !team_bot {
            return error(
                StatusCode::NOT_FOUND,
                format!("no team bot `{name}`: only a team bot has accounts of its own"),
            );
        }
    }
    match oauth.authorize_url(&q.user, provider, Utc::now()) {
        Ok(url) => Json(json!({ "url": url })).into_response(),
        Err(e @ OAuthError::NotConfigured(_)) => error(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn page(status: StatusCode, title: &str, line: &str) -> Response {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{title}</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1 style=\"font-size:1.4rem\">{title}</h1><p>{line}</p></body>"
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// Where Atlassian and Slack send the PM back. Public, so it says as little as possible.
async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let (oauth, provider) = match linking(&state, &provider) {
        Ok(x) => x,
        Err((status, message)) => return error(status, message),
    };
    if q.error.is_some() {
        return page(
            StatusCode::BAD_REQUEST,
            "Not connected",
            "The connection was cancelled. You can close this tab and start again from PM OS.",
        );
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Not connected",
            "This link is incomplete.",
        );
    };
    match oauth.complete(provider, &code, &st, Utc::now()).await {
        Ok(_) => page(
            StatusCode::OK,
            &format!("{} connected", provider.label()),
            "PM OS will act as you, with your own permissions. You can close this tab.",
        ),
        Err(OAuthError::BadState) => page(
            StatusCode::BAD_REQUEST,
            "Link expired",
            "This sign-in link is invalid or has expired. Start again from PM OS.",
        ),
        Err(e @ OAuthError::WrongAccount(..)) => {
            tracing::warn!(provider = %provider, error = %e, "account linking refused");
            page(
                StatusCode::FORBIDDEN,
                "Not connected",
                "This sign-in link was made for someone else. Ask PM OS for your own link.",
            )
        }
        Err(e) => {
            tracing::warn!(provider = %provider, error = %e, "account linking failed");
            page(
                StatusCode::BAD_GATEWAY,
                "Not connected",
                "PM OS couldn't finish connecting. Try again, or ask a platform owner.",
            )
        }
    }
}

/// A PM's linked accounts, without tokens.
async fn list_credentials(State(state): State<AppState>, Query(q): Query<UserQuery>) -> Response {
    let Some(oauth) = state.oauth.clone() else {
        return Json(json!({ "accounts": [] })).into_response();
    };
    match oauth.vault().list(&q.user).await {
        Ok(accounts) => Json(json!({ "accounts": accounts })).into_response(),
        Err(e) => unavailable(e),
    }
}

/// What an unlink is audited as.
pub const UNLINK_TOOL: &str = "credentials.unlink";

/// Unlink an account (the PM disconnects, or a platform owner revokes). The Slack bot may
/// unlink a team member's account only (the App Home's **Disconnect**). Every answer but a
/// malformed one is audited, with the client.
async fn revoke_credential(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path((user, provider)): Path<(String, String)>,
) -> Response {
    let audit = |outcome: Outcome, detail: String| {
        let svc = state.svc.clone();
        let (user, provider) = (user.clone(), provider.clone());
        async move {
            svc.audit(AuditEvent {
                at: Utc::now(),
                client,
                run_id: format!("unlink-{user}-{provider}"),
                user,
                skill: None,
                bot: None,
                tool: UNLINK_TOOL.to_string(),
                outcome,
                sources: Vec::new(),
                args_hash: None,
                output_ref: Some(provider),
                detail: Some(detail),
            })
            .await;
        }
    };
    // As for sessions: enforced with live connectors (the only mode that links accounts),
    // and never a team bot's own account.
    if client != Client::Orchestrator && !state.svc.team().allows(&user) {
        audit(
            Outcome::Denied,
            "unlink for someone who isn't a team member".into(),
        )
        .await;
        return error(
            StatusCode::FORBIDDEN,
            "this token may unlink accounts of team members only",
        );
    }
    let (oauth, provider) = match linking(&state, &provider) {
        Ok(x) => x,
        Err((status, message)) => return error(status, message),
    };
    match oauth.unlink_account(&user, provider).await {
        Ok(true) => {
            tracing::info!(user = %user, provider = %provider, client = client.as_str(), "account unlinked");
            audit(Outcome::Ok, "unlinked".into()).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            audit(Outcome::Ok, "nothing was linked".into()).await;
            error(StatusCode::NOT_FOUND, "no such linked account")
        }
        Err(e) => {
            audit(Outcome::Error, "storage unavailable".into()).await;
            unavailable(e)
        }
    }
}

#[derive(Deserialize)]
struct ToolCall {
    tool: String,
    #[serde(default)]
    args: Value,
}

/// Run one tool call for a session, for trusted internal callers (the Slack bot, the
/// orchestrator). The same rules as MCP apply: the skill's allowlist, argument checks,
/// approvals for writes and the audit log.
async fn call_tool(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ToolCall>,
) -> Response {
    let session = match header_session(&state, client, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match state
        .svc
        .call_as(client, &session, &body.tool, body.args, Utc::now())
        .await
    {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(e) => {
            let status = match &e {
                ToolError::UnknownTool(_)
                | ToolError::NotAllowed(_)
                | ToolError::NotInRole(_)
                | ToolError::Forbidden(_) => StatusCode::FORBIDDEN,
                ToolError::InvalidArgs(_) => StatusCode::BAD_REQUEST,
                ToolError::NotFound(_) => StatusCode::NOT_FOUND,
                ToolError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
                ToolError::Connector(_) => StatusCode::BAD_GATEWAY,
            };
            error(status, e.to_string())
        }
    }
}

// ----- onboarding (docs/onboarding-spec.md §2–3) -------------------------------------------

fn refusal(r: Refusal) -> Response {
    (r.status, Json(r.body())).into_response()
}

#[derive(Deserialize)]
struct MemoryRepoBody {
    user: String,
    repo: String,
}

/// Check a repo with the PM's GitHub token and record it as their memory repo.
async fn set_memory_repo(
    State(state): State<AppState>,
    Json(body): Json<MemoryRepoBody>,
) -> Response {
    match state
        .onboarding
        .set_memory_repo(&body.user, &body.repo, Utc::now())
        .await
    {
        Ok(record) => Json(record).into_response(),
        Err(r) => refusal(r),
    }
}

async fn get_memory_repo(State(state): State<AppState>, Query(q): Query<UserQuery>) -> Response {
    match state.onboarding.memory_repo(&q.user).await {
        Ok(Some(record)) => Json(record).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no memory repo set", "code": "not_found"})),
        )
            .into_response(),
        Err(e) => unavailable(e),
    }
}

#[derive(Deserialize)]
struct MemberRequestBody {
    #[serde(default)]
    slack_id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
}

/// `201 {request}` for a new request, `200 {request}` for the one already pending.
async fn create_member_request(
    State(state): State<AppState>,
    Json(body): Json<MemberRequestBody>,
) -> Response {
    match state
        .onboarding
        .request(&body.slack_id, &body.email, &body.name, Utc::now())
        .await
    {
        Ok((created, request)) => {
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (status, Json(json!({ "request": request }))).into_response()
        }
        Err(r) => refusal(r),
    }
}

#[derive(Deserialize)]
struct RequestsQuery {
    status: Option<String>,
}

async fn list_member_requests(
    State(state): State<AppState>,
    Query(q): Query<RequestsQuery>,
) -> Response {
    let status =
        match q.status.as_deref() {
            None => None,
            Some(s) => match RequestStatus::parse(s) {
                Some(status) => Some(status),
                None => return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error": format!("unknown status `{s}`"), "code": "bad_request"})),
                )
                    .into_response(),
            },
        };
    match state.onboarding.requests(status).await {
        Ok(requests) => Json(json!({ "requests": requests })).into_response(),
        Err(e) => unavailable(e),
    }
}

#[derive(Deserialize)]
struct MemberDecisionBody {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    decided_by: String,
    handle: Option<String>,
    roles: Option<Vec<String>>,
}

async fn decide_member_request(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
    Json(body): Json<MemberDecisionBody>,
) -> Response {
    let input = DecisionInput {
        decision: body.decision,
        decided_by: body.decided_by,
        handle: body.handle,
        roles: body.roles,
    };
    match state
        .onboarding
        .decide(client, &id, input, Utc::now())
        .await
    {
        Ok(request) => Json(json!({ "request": request })).into_response(),
        Err(r) => refusal(r),
    }
}

#[derive(Deserialize)]
struct RemoveBody {
    #[serde(default)]
    decided_by: String,
}

async fn remove_member(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(handle): Path<String>,
    Json(body): Json<RemoveBody>,
) -> Response {
    match state
        .onboarding
        .remove(client, &handle, &body.decided_by, Utc::now())
        .await
    {
        Ok(commit) => Json(json!({ "handle": handle, "commit": commit })).into_response(),
        Err(r) => refusal(r),
    }
}

#[derive(Deserialize)]
struct OnboardingQuery {
    user: Option<String>,
    slack_id: Option<String>,
}

/// A PM's setup checklist, by handle, or by Slack id for someone not yet a member.
async fn onboarding_status(
    State(state): State<AppState>,
    Query(q): Query<OnboardingQuery>,
) -> Response {
    if q.user.is_none() && q.slack_id.is_none() {
        return error(StatusCode::BAD_REQUEST, "send `user` or `slack_id`");
    }
    match state
        .onboarding
        .status(q.user.as_deref(), q.slack_id.as_deref())
        .await
    {
        Ok(status) => Json(status).into_response(),
        Err(e) => unavailable(e),
    }
}
