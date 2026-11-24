//! The sandbox-audit collector's two routes (`docs/openshell-prover-and-sandbox-audit-spec.md`
//! §B.2, slice B1): `POST /internal/sandbox-audit/batch` and `POST /internal/sandbox-audit/
//! heartbeat`.
//!
//! **Who may call:** only `PMOS_SANDBOX_AUDIT_TOKEN`, compared in constant time; every other
//! token (the orchestrator's included) is `403`, no token `401`. And that token reaches nothing
//! else: `client_for` doesn't know it, so every other route answers it `401` (the route matrix
//! in `tests/sandbox_audit.rs`). It appends sandbox decisions through these checks and can't
//! read, edit or delete audit events. Without the setting both routes are `404`, as if absent.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::Utc;
use serde_json::json;

use super::{bearer, client_for, error, unavailable, AppState};
use crate::openshell::sandbox::{Batch, Heartbeat, MAX_BODY};
use crate::openshell::{self, IngestError};

pub(super) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/sandbox-audit/batch", post(batch))
        .route("/sandbox-audit/heartbeat", post(heartbeat))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(state, only_the_collector))
}

async fn only_the_collector(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(token) = state.svc.sandbox_audit_token() else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    match bearer(&req) {
        Some(t) if pmos_runtime::token_eq(t, &token) => next.run(req).await,
        Some(t) if client_for(&state, Some(t)).is_some() => error(
            StatusCode::FORBIDDEN,
            "only the sandbox-audit collector may use this endpoint",
        ),
        _ => error(
            StatusCode::UNAUTHORIZED,
            "the collector's token is required",
        ),
    }
}

fn answer(r: Result<openshell::sandbox::Ingested, IngestError>) -> Response {
    match r {
        Ok(done) => Json(json!({
            "stored": done.stored.len(),
            "duplicates": done.duplicates,
            "replayed": done.replayed,
            "gaps": done.gaps,
        }))
        .into_response(),
        Err(IngestError::Invalid(why)) => {
            tracing::warn!(error = %why, "a sandbox-audit batch was refused");
            error(StatusCode::BAD_REQUEST, why)
        }
        Err(IngestError::Store(e)) => unavailable(e),
    }
}

async fn batch(State(state): State<AppState>, body: Bytes) -> Response {
    let batch: Batch = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "not a sandbox-audit batch (unknown or missing fields)",
            )
        }
    };
    answer(openshell::ingest(&state.svc, batch, Utc::now()).await)
}

async fn heartbeat(State(state): State<AppState>, body: Bytes) -> Response {
    let hb: Heartbeat = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return error(StatusCode::BAD_REQUEST, "not a heartbeat"),
    };
    answer(openshell::heartbeat(&state.svc, hb, Utc::now()).await)
}
