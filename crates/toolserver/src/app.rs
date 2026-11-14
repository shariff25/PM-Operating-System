//! The HTTP application: `/mcp` for agent sandboxes, `/internal` for the orchestrator and
//! the approval inbox, `/healthz` for the platform-owner checks.
//!
//! - `/mcp` needs a session token (`Authorization: Bearer pmos_s_…`).
//! - `/internal` needs the service token. Only the orchestrator, the Slack app and the web
//!   workspace hold it; the sandbox network policy never routes to it.

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::approvals::{DecideError, Decision, Status};
use crate::mcp::PmosMcp;
use crate::oauth::{OAuth, OAuthError};
use crate::service::{DecisionError, ReindexError, SessionError, ToolError, ToolService};
use crate::store::StoreError;
use crate::vault::Provider;

#[derive(Clone)]
pub struct AppConfig {
    pub service_token: String,
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
    content_root: Arc<std::path::Path>,
    oauth: Option<Arc<OAuth>>,
}

pub fn router(svc: Arc<ToolService>, config: AppConfig, cancel: CancellationToken) -> Router {
    let state = AppState {
        svc: svc.clone(),
        service_token: config.service_token.into(),
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

    let internal = Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{token}", axum::routing::delete(revoke_session))
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_service_token,
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
    req.headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
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

/// Constant-time comparison, so the service token can't be guessed byte by byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
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

async fn require_service_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    match bearer(&req) {
        Some(token) if same(token, &state.service_token) => next.run(req).await,
        _ => error(StatusCode::UNAUTHORIZED, "service token required"),
    }
}

#[derive(Deserialize)]
struct NewSession {
    user: String,
    skill: String,
    run_id: String,
    ttl_minutes: Option<i64>,
}

async fn create_session(State(state): State<AppState>, Json(body): Json<NewSession>) -> Response {
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
        Err(SessionError::Store(e)) => unavailable(e),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn revoke_session(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    match state.svc.revoke_session(&token).await {
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
    match oauth.vault().delete(&user, provider).await {
        Ok(true) => {
            tracing::info!(user = %user, provider = %provider, "account unlinked");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, "no such linked account"),
        Err(e) => unavailable(e),
    }
}
