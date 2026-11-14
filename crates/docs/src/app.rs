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
use chrono::Utc;
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
    pub syncer: Syncer,
    pub last: Mutex<Option<Result<SyncReport, String>>>,
    running: AtomicBool,
}

impl Runtime {
    pub fn new(store: Arc<dyn DocsStore>, syncer: Syncer) -> Self {
        Self {
            store,
            syncer,
            last: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Run one sync unless one is already running. Returns false if one was.
    pub async fn sync_once(&self) -> bool {
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        let result = self.syncer.run(Utc::now()).await;
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
        *self.last.lock().expect("status lock") = Some(result.map_err(|e| e.to_string()));
        self.running.store(false, Ordering::SeqCst);
        true
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub token: String,
    pub base: String,
    pub allowed_hosts: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<Runtime>,
    token: Arc<str>,
}

pub fn router(runtime: Arc<Runtime>, config: AppConfig, cancel: CancellationToken) -> Router {
    let state = AppState {
        runtime: runtime.clone(),
        token: config.token.into(),
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
        .route("/healthz", get(|| async { "ok" }))
        .merge(protected)
        .with_state(state)
}

/// Constant-time comparison.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| same(t, &state.token));
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
    if state.runtime.is_running() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"status": "already running"})),
        )
            .into_response();
    }
    let runtime = state.runtime.clone();
    tokio::spawn(async move {
        runtime.sync_once().await;
    });
    (StatusCode::ACCEPTED, Json(json!({"status": "started"}))).into_response()
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
    Json(json!({
        "pages": pages,
        "syncing": state.runtime.is_running(),
        "last_sync": match last {
            None => json!(null),
            Some(Ok(report)) => json!(report),
            Some(Err(error)) => json!({"error": error}),
        },
    }))
    .into_response()
}
