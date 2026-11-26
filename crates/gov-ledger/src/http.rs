//! The Ledger's listener: where gates ship their receipts (spec §7.3, "Gate link").
//!
//! One route takes a shipment. It answers only a request signed by an enrolled gate key; a
//! request from anyone else gets a bare 401 and leaves no trace in the store.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gov_core::{Envelope, Id, SigningKey};
use serde::Deserialize;
use serde_json::json;

use crate::ledger::{Ledger, LedgerError};
use crate::store::Store;

/// The most receipts in one shipment, and the largest request body.
pub const MAX_BATCH: usize = 1000;
const MAX_BODY: usize = 8 * 1024 * 1024;

/// What the health route reports besides being up (review finding GR-87).
#[derive(Debug, Default)]
pub struct Health {
    /// Checkpoints that failed in a row. Zero once one is published.
    pub checkpoint_failures: AtomicU64,
    /// When the last checkpoint was published, in seconds since the epoch; zero for never.
    pub last_checkpoint_at: AtomicI64,
}

pub struct Listener<S> {
    pub health: Health,
    pub ledger: Ledger<S>,
    /// The Ledger's signing key, for its answers.
    pub key: SigningKey,
    pub issuer: Id,
    pub clock: fn() -> i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shipment {
    request: Envelope,
    receipts: Vec<Envelope>,
}

pub fn router<S: Store + 'static>(listener: Arc<Listener<S>>) -> Router {
    Router::new()
        .route("/v1/receipts", post(receipts::<S>))
        .route("/healthz", get(health::<S>))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(listener)
}

/// Up, and publishing checkpoints. A Ledger whose checkpoints keep failing answers 503: its
/// receipts are arriving and nothing is witnessing them.
async fn health<S: Store + 'static>(State(l): State<Arc<Listener<S>>>) -> Response {
    let failures = l.health.checkpoint_failures.load(Ordering::Relaxed);
    let body = Json(json!({
        "checkpoint_failures": failures,
        "last_checkpoint_at": l.health.last_checkpoint_at.load(Ordering::Relaxed),
    }));
    if failures > 0 {
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
    } else {
        body.into_response()
    }
}

async fn receipts<S: Store + 'static>(
    State(l): State<Arc<Listener<S>>>,
    Json(shipment): Json<Shipment>,
) -> Response {
    if shipment.receipts.len() > MAX_BATCH {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let now = (l.clock)();
    match l
        .ledger
        .take_shipment(
            &shipment.request,
            &shipment.receipts,
            &l.key,
            &l.issuer,
            now,
        )
        .await
    {
        Ok((ack, refused)) => Json(json!({"ack": ack, "refused": refused})).into_response(),
        // Who is asking is not established: say nothing more than that.
        Err(LedgerError::Refused("unknown_sender")) => StatusCode::UNAUTHORIZED.into_response(),
        Err(LedgerError::Refused(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
