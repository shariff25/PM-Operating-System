//! HTTP: `/mcp` (MCP over Streamable HTTP), `/admin/sync` and `/admin/status`, `/healthz`.
//! Everything but `/healthz` needs the bearer token only the tool server and platform-owner
//! PMs hold.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::mcp::DocsMcp;
use crate::store::DocsStore;
use crate::sync::{SyncReport, Syncer};

/// Shared between the HTTP handlers and the background sync loop.
pub struct Runtime {
    pub store: Arc<dyn DocsStore>,
    pub syncer: Arc<Syncer>,
    pub last: Mutex<Option<Result<SyncReport, String>>>,
    /// When the last run in this process finished, and whether it succeeded.
    last_attempt: Mutex<Option<(DateTime<Utc>, bool)>>,
    running: AtomicBool,
}

/// Clears the running flag however the run ends.
struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// After a failed run, try again this soon rather than waiting a whole interval.
pub const RETRY_FAILED_AFTER: Duration = Duration::from_secs(60 * 60);

impl Runtime {
    pub fn new(store: Arc<dyn DocsStore>, syncer: Syncer) -> Self {
        Self {
            store,
            syncer: Arc::new(syncer),
            last: Mutex::new(None),
            last_attempt: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Run one sync unless one is already running. Returns false if one was.
    pub async fn sync_once(&self) -> bool {
        if !self.claim() {
            return false;
        }
        self.run_claimed().await;
        true
    }

    /// Mark a sync as running. False if one already is.
    fn claim(&self) -> bool {
        !self.running.swap(true, Ordering::SeqCst)
    }

    /// Run a sync claimed with [`Runtime::claim`]. The run is its own task, so a panic in it
    /// is recorded as a failed sync instead of taking the scheduler down.
    async fn run_claimed(&self) {
        let _running = Running(&self.running);
        let syncer = self.syncer.clone();
        let result = match tokio::spawn(async move { syncer.run(Utc::now()).await }).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(e) => Err(format!("the sync stopped unexpectedly: {e}")),
        };
        let finished = Utc::now();
        match &result {
            Ok(r) => tracing::info!(
                listed = r.listed,
                fetched = r.fetched,
                unchanged = r.unchanged,
                failed = r.failed,
                removed = r.removed,
                "docs sync finished"
            ),
            Err(e) => tracing::warn!(error = %e, "docs sync failed"),
        }
        let (ok, report) = match &result {
            Ok(r) => (true, json!(r)),
            Err(e) => (false, json!({ "error": e })),
        };
        if let Err(e) = self.store.record_sync(finished, ok, &report).await {
            tracing::warn!(error = %e, "couldn't record the sync run");
        }
        *self.last_attempt.lock().expect("status lock") = Some((finished, ok));
        *self.last.lock().expect("status lock") = Some(result);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Is a sync due? Yes if the index has never been synced, if the last success is older
    /// than `every`, or if the last run failed more than [`RETRY_FAILED_AFTER`] ago. With no
    /// schedule (`every` is `None`), only when it has never been synced.
    pub async fn due(&self, every: Option<Duration>, now: DateTime<Utc>) -> bool {
        let last_success = match self.store.last_success().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't read the last sync time; syncing");
                return true;
            }
        };
        let Some(last_success) = last_success else {
            return true;
        };
        let age = |t: DateTime<Utc>| (now - t).to_std().unwrap_or_default();
        if let Some((at, false)) = *self.last_attempt.lock().expect("status lock") {
            if at > last_success && age(at) >= RETRY_FAILED_AFTER {
                return true;
            }
        }
        every.is_some_and(|every| age(last_success) >= every)
    }

    pub async fn last_success(&self) -> Option<DateTime<Utc>> {
        self.store.last_success().await.ok().flatten()
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub token: String,
    pub base: String,
    pub allowed_hosts: Vec<String>,
    /// `/readyz` fails once the last successful sync is older than this (twice the sync
    /// interval). `None` with no schedule.
    pub stale_after: Option<Duration>,
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<Runtime>,
    token: Arc<str>,
    stale_after: Option<Duration>,
}

pub fn router(runtime: Arc<Runtime>, config: AppConfig, cancel: CancellationToken) -> Router {
    let state = AppState {
        runtime: runtime.clone(),
        token: config.token.into(),
        stale_after: config.stale_after,
    };
    let handler = DocsMcp::new(runtime.store.clone(), config.base);
    let mcp = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(config.allowed_hosts)
            .with_cancellation_token(cancel.child_token()),
    );
    let protected = Router::new()
        .nest_service("/mcp", mcp)
        .route("/admin/sync", post(trigger_sync))
        .route("/admin/status", get(status))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/readyz", get(readyz))
        .route("/healthz", get(|| async { "ok" }))
        .merge(protected)
        .with_state(state)
}

async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(pmos_runtime::bearer_value)
        .is_some_and(|t| pmos_runtime::token_eq(t, &state.token));
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "token required"})),
        )
            .into_response()
    }
}

async fn trigger_sync(State(state): State<AppState>) -> Response {
    // Claim before spawning, so two requests can't both be told "started".
    if !state.runtime.claim() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"status": "already running"})),
        )
            .into_response();
    }
    let runtime = state.runtime.clone();
    tokio::spawn(async move {
        runtime.run_claimed().await;
    });
    (StatusCode::ACCEPTED, Json(json!({"status": "started"}))).into_response()
}

/// `200` when the index answers, has pages and isn't stale; `503` otherwise. No auth: it
/// names which check failed, nothing more.
async fn readyz(State(state): State<AppState>) -> Response {
    let pages = tokio::time::timeout(Duration::from_secs(1), state.runtime.store.count()).await;
    let storage = matches!(pages, Ok(Ok(_)));
    let has_pages = matches!(pages, Ok(Ok(n)) if n > 0);
    let fresh = match (state.stale_after, state.runtime.last_success().await) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(limit), Some(at)) => (Utc::now() - at).to_std().unwrap_or_default() < limit,
    };
    let ok = storage && has_pages && fresh;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mark = |b: bool| if b { "ok" } else { "down" };
    (
        status,
        Json(json!({
            "ready": ok,
            "checks": {
                "storage": mark(storage),
                "pages": mark(has_pages),
                "fresh": mark(fresh),
            },
        })),
    )
        .into_response()
}

async fn status(State(state): State<AppState>) -> Response {
    let pages = match state.runtime.store.count().await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "docs storage error");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "storage unavailable"})),
            )
                .into_response();
        }
    };
    let last = state.runtime.last.lock().expect("status lock").clone();
    let last_success = state.runtime.last_success().await;
    Json(json!({
        "pages": pages,
        "syncing": state.runtime.is_running(),
        "last_success_at": last_success,
        "last_sync": match last {
            None => json!(null),
            Some(Ok(report)) => json!(report),
            Some(Err(error)) => json!({"error": error}),
        },
    }))
    .into_response()
}
