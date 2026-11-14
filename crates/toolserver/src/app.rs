//! The HTTP application: `/mcp` for agent sandboxes, `/internal` for the orchestrator and
//! the approval inbox, `/healthz` for the platform-owner checks.
//!
//! - `/mcp` needs a session token (`Authorization: Bearer pmos_s_…`).
//! - `/internal` needs a service token, and each internal client has its own:
//!   - `PMOS_SERVICE_TOKEN`: the orchestrator and platform-owner tools. Every route.
//!   - `PMOS_BOT_TOKEN`: the Slack bot. Only sessions for its skills (`PMOS_BOT_SKILLS`) and
//!     calls within them; never approvals, grants, linking or reindexing.
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
use chrono::{Duration, Utc};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::approvals::{DecideError, Decision, Status};
use crate::audit::Client;
use crate::mcp::PmosMcp;
use crate::oauth::{OAuth, OAuthError};
use crate::service::{DecisionError, ReindexError, SessionError, ToolError, ToolService};
use crate::store::StoreError;
use crate::vault::Provider;

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
}

pub fn router(svc: Arc<ToolService>, config: AppConfig, cancel: CancellationToken) -> Router {
    let state = AppState {
        svc: svc.clone(),
        service_token: config.service_token.into(),
        bot: config.bot.map(Arc::new),
        content_root: config.content_root.into(),
        oauth: config.oauth.clone(),
    };

    let mcp_svc = svc.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(PmosMcp::new(mcp_svc.clone())),
        LocalSessionManager::default().into(),
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
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", get(get_approval))
        .route("/approvals/{id}/decision", post(decide))
        .route("/reindex", post(reindex))
        .route("/oauth/{provider}/start", get(oauth_start))
        .route("/credentials", get(list_credentials))
        .route(
            "/credentials/{user}/{provider}",
            axum::routing::delete(revoke_credential),
        )
        .layer(middleware::from_fn(orchestrator_only));

    // Any internal client; the bot is limited to its own skills inside each handler.
    let internal = Router::new()
        .route("/sessions", post(create_session))
        .route("/session", axum::routing::delete(revoke_session))
        .route("/call", post(call_tool))
        .merge(admin)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            identify_client,
        ));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // Public: the PM's browser lands here from Atlassian or Slack.
        .route("/oauth/{provider}/callback", get(oauth_callback))
        .nest("/internal", internal)
        .nest("/mcp", mcp)
        .with_state(state)
}

fn bearer(req: &Request) -> Option<&str> {
    pmos_runtime::bearer_value(req.headers().get(header::AUTHORIZATION)?.to_str().ok()?)
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

/// May `client` work with sessions for `skill`? The bot only for its own skills.
fn client_may_use(state: &AppState, client: Client, skill: &str) -> bool {
    match client {
        Client::Orchestrator => true,
        Client::Bot => state
            .bot
            .as_ref()
            .is_some_and(|b| b.skills.iter().any(|s| s == skill)),
        Client::Mcp => false,
    }
}

/// The session named by the `X-Pmos-Session` header, if `client` may use it.
async fn header_session(
    state: &AppState,
    client: Client,
    headers: &axum::http::HeaderMap,
) -> Result<crate::session::Session, Response> {
    let token = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "missing X-Pmos-Session header"))?;
    match state.svc.session(token, Utc::now()).await {
        Ok(Some(session)) if client_may_use(state, client, &session.skill) => Ok(session),
        // A session the client may not use looks the same as one that doesn't exist.
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "no such session")),
        Err(e) => Err(unavailable(e)),
    }
}

#[derive(Deserialize)]
struct NewSession {
    user: String,
    skill: String,
    run_id: String,
    ttl_minutes: Option<i64>,
}

async fn create_session(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Json(body): Json<NewSession>,
) -> Response {
    if !client_may_use(&state, client, &body.skill) {
        return error(
            StatusCode::FORBIDDEN,
            format!("this token may not start `{}` sessions", body.skill),
        );
    }
    let ttl = body
        .ttl_minutes
        .map(|m| Duration::minutes(m.clamp(1, 24 * 60)));
    match state
        .svc
        .create_session(&body.user, &body.skill, &body.run_id, ttl, Utc::now())
        .await
    {
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
        Err(e @ SessionError::UnknownSkill(_)) => error(StatusCode::NOT_FOUND, e.to_string()),
        Err(e @ SessionError::NotAMember(_)) => error(StatusCode::FORBIDDEN, e.to_string()),
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
        Err(r) => return r,
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
        Err(e @ ToolError::NotAllowed(_)) => error(StatusCode::FORBIDDEN, e.to_string()),
        Err(ToolError::Unavailable(e)) => unavailable(StoreError(e)),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ApprovalQuery {
    user: String,
    status: Option<Status>,
}

async fn list_approvals(State(state): State<AppState>, Query(q): Query<ApprovalQuery>) -> Response {
    match state.svc.approvals(&q.user, q.status).await {
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
}

async fn decide(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DecisionBody>,
) -> Response {
    match state
        .svc
        .decide(&id, &body.decided_by, body.decision, body.args, Utc::now())
        .await
    {
        Ok(item) => Json(item).into_response(),
        Err(DecisionError::Decide(DecideError::Store(e))) => unavailable(e),
        Err(DecisionError::Decide(e)) => {
            let status = match e {
                DecideError::NotFound => StatusCode::NOT_FOUND,
                DecideError::Forbidden => StatusCode::FORBIDDEN,
                DecideError::NotPending => StatusCode::CONFLICT,
                DecideError::MissingArgs => StatusCode::BAD_REQUEST,
                DecideError::Store(_) => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(status, e.to_string())
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
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

/// The URL to send a PM to so they can link their account.
async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<UserQuery>,
) -> Response {
    let (oauth, provider) = match linking(&state, &provider) {
        Ok(x) => x,
        Err((status, message)) => return error(status, message),
    };
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

/// Unlink an account (the PM disconnects, or a platform owner revokes).
async fn revoke_credential(
    State(state): State<AppState>,
    Path((user, provider)): Path<(String, String)>,
) -> Response {
    let (oauth, provider) = match linking(&state, &provider) {
        Ok(x) => x,
        Err((status, message)) => return error(status, message),
    };
    match oauth.unlink_account(&user, provider).await {
        Ok(true) => {
            tracing::info!(user = %user, provider = %provider, "account unlinked");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, "no such linked account"),
        Err(e) => unavailable(e),
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
        Err(r) => return r,
    };
    match state
        .svc
        .call_as(client, &session, &body.tool, body.args, Utc::now())
        .await
    {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(e) => {
            let status = match &e {
                ToolError::UnknownTool(_) | ToolError::NotAllowed(_) | ToolError::Forbidden(_) => {
                    StatusCode::FORBIDDEN
                }
                ToolError::InvalidArgs(_) => StatusCode::BAD_REQUEST,
                ToolError::NotFound(_) => StatusCode::NOT_FOUND,
                ToolError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
                ToolError::Connector(_) => StatusCode::BAD_GATEWAY,
            };
            error(status, e.to_string())
        }
    }
}
