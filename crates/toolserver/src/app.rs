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
use crate::service::{DecisionError, SessionError, ToolError, ToolService};

#[derive(Clone)]
pub struct AppConfig {
    pub service_token: String,
    /// `Host` values the MCP endpoint accepts (DNS-rebinding protection).
    pub allowed_hosts: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    svc: Arc<ToolService>,
    service_token: Arc<str>,
}

pub fn router(svc: Arc<ToolService>, config: AppConfig, cancel: CancellationToken) -> Router {
    let state = AppState {
        svc: svc.clone(),
        service_token: config.service_token.into(),
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_service_token,
        ));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
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

/// Constant-time comparison, so the service token can't be guessed byte by byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn require_session(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(session) = bearer(&req).and_then(|t| state.svc.session(t, Utc::now())) else {
        return error(StatusCode::UNAUTHORIZED, "missing or expired PM OS session");
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
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn revoke_session(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if state.svc.revoke_session(&token) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(StatusCode::NOT_FOUND, "no such session")
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
    let Some(session) = state.svc.session(&body.session_token, now) else {
        return error(StatusCode::NOT_FOUND, "no such session");
    };
    match state.svc.grant(&session, &body.tool, &body.args, now) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e @ ToolError::NotAllowed(_)) => error(StatusCode::FORBIDDEN, e.to_string()),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ApprovalQuery {
    user: String,
    status: Option<Status>,
}

async fn list_approvals(State(state): State<AppState>, Query(q): Query<ApprovalQuery>) -> Response {
    Json(json!({ "items": state.svc.approvals(&q.user, q.status) })).into_response()
}

async fn get_approval(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.svc.approval(&id) {
        Some(item) => Json(item).into_response(),
        None => error(StatusCode::NOT_FOUND, "no such approval item"),
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
        Err(DecisionError::Decide(e)) => {
            let status = match e {
                DecideError::NotFound => StatusCode::NOT_FOUND,
                DecideError::Forbidden => StatusCode::FORBIDDEN,
                DecideError::NotPending => StatusCode::CONFLICT,
                DecideError::MissingArgs => StatusCode::BAD_REQUEST,
            };
            error(status, e.to_string())
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
