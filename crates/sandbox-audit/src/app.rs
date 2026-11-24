//! The collector's process: the OTLP receiver, the spool's sender, the heartbeat and the
//! flusher (`docs/openshell-prover-and-sandbox-audit-spec.md` §B.2).
//!
//! It is a receiver, not a reader with rights over OpenShell: it serves only `POST /v1/logs`
//! (OTLP/HTTP protobuf, uncompressed) with OpenShell's header token, mounts no Docker socket
//! and no OpenShell configuration, and runs no `openshell` command. It posts to the tool server
//! with its own token, which can append sandbox decisions and nothing else.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

use crate::limits::Limiter;
use crate::ocsf;
use crate::otlp;
use crate::spool::Spool;
use crate::wire::{Batch, Heartbeat, WireEvent};

/// The most events and bytes in one batch to the tool server (its own limits, §B.2).
pub const MAX_EVENTS: usize = 500;
pub const MAX_BATCH_BYTES: usize = 200 * 1024;
/// The largest OTLP request taken, and the most records in it.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
pub const MAX_RECORDS: usize = 20_000;
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(60);
pub const FLUSH_EVERY: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// What OpenShell sends in `authorization: Bearer …` (its `headers_file`).
    pub otlp_token: String,
    /// The tool server's `PMOS_SANDBOX_AUDIT_TOKEN`.
    pub toolserver_token: String,
    pub toolserver_url: String,
    pub spool_dir: PathBuf,
    pub spool_max: u64,
    /// `OPENSHELL_FORMAT_VERIFIED` from the pin; empty: none verified yet.
    pub format_verified: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("toolserver_url", &self.toolserver_url)
            .field("spool_dir", &self.spool_dir)
            .field("format_verified", &self.format_verified)
            .finish_non_exhaustive()
    }
}

fn secret(get: &impl Fn(&str) -> Option<String>, key: &str) -> Result<String, String> {
    let value = match get(&format!("{key}_FILE")).filter(|s| !s.is_empty()) {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| format!("{key}_FILE can't be read: {}", e.kind()))?
            .trim()
            .to_string(),
        None => get(key).unwrap_or_default(),
    };
    // The headers file OpenShell reads is `authorization=Bearer <token>`: take the token.
    let value = value
        .strip_prefix("authorization=Bearer ")
        .unwrap_or(&value)
        .to_string();
    if value.len() < 32 {
        return Err(format!(
            "{key} (or {key}_FILE) must hold at least 32 characters"
        ));
    }
    if value.contains(['\n', '\r', '"', '\\']) {
        return Err(format!("{key} holds a line break, quote or backslash"));
    }
    Ok(value)
}

impl Config {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let bind = get("PMOS_SANDBOX_AUDIT_BIND")
            .unwrap_or_else(|| "127.0.0.1:8780".into())
            .parse()
            .map_err(|_| "PMOS_SANDBOX_AUDIT_BIND isn't an address")?;
        let otlp_token = secret(&get, "PMOS_SANDBOX_AUDIT_OTLP_TOKEN")?;
        let toolserver_token = secret(&get, "PMOS_SANDBOX_AUDIT_TOKEN")?;
        if otlp_token == toolserver_token {
            return Err("the OTLP token and PMOS_SANDBOX_AUDIT_TOKEN must differ".into());
        }
        let toolserver_url = get("PMOS_TOOLSERVER_URL")
            .unwrap_or_else(|| "http://toolserver:8710".into())
            .trim_end_matches('/')
            .to_string();
        if !toolserver_url.starts_with("http://") && !toolserver_url.starts_with("https://") {
            return Err("PMOS_TOOLSERVER_URL must be an http(s) URL".into());
        }
        let spool_max = match get("PMOS_SANDBOX_AUDIT_SPOOL_MAX_BYTES") {
            Some(v) => v
                .parse()
                .map_err(|_| "PMOS_SANDBOX_AUDIT_SPOOL_MAX_BYTES isn't a number")?,
            None => crate::spool::DEFAULT_MAX_BYTES,
        };
        Ok(Self {
            bind,
            otlp_token,
            toolserver_token,
            toolserver_url,
            spool_dir: get("PMOS_SANDBOX_AUDIT_SPOOL")
                .unwrap_or_else(|| "/var/lib/pmos-sandbox-audit/spool".into())
                .into(),
            spool_max,
            format_verified: get("PMOS_SANDBOX_AUDIT_FORMAT_VERIFIED").filter(|v| !v.is_empty()),
        })
    }
}

/// A fresh run id: `r` and 24 hex characters from the OS's random source.
pub fn new_run() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).expect("the OS random source");
    format!(
        "r{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

pub struct Inner {
    pub run: String,
    pub next_seq: u64,
    pub acked_seq: u64,
    pub limiter: Limiter,
    pub spool: Spool,
}

#[derive(Clone)]
pub struct Collector {
    pub config: Arc<Config>,
    pub inner: Arc<Mutex<Inner>>,
    http: reqwest::Client,
}

/// Why events couldn't be spooled.
#[derive(Debug)]
pub enum SpoolError {
    Full,
    Io(std::io::Error),
}

impl Collector {
    pub fn new(config: Config) -> std::io::Result<Self> {
        let spool = Spool::open(&config.spool_dir, config.spool_max)?;
        let inner = Inner {
            run: new_run(),
            next_seq: 1,
            acked_seq: 0,
            limiter: Limiter::new(config.format_verified.clone()),
            spool,
        };
        Ok(Self {
            config: Arc::new(config),
            inner: Arc::new(Mutex::new(inner)),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .no_proxy()
                .build()
                .expect("an HTTP client"),
        })
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/logs", post(receive))
            .layer(DefaultBodyLimit::max(MAX_REQUEST))
            .with_state(self.clone())
    }

    /// Number, batch and spool `events`, all or nothing.
    fn spool_events(inner: &mut Inner, events: Vec<WireEvent>) -> Result<(), SpoolError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut seq = inner.next_seq;
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        let mut current: Vec<WireEvent> = Vec::new();
        let mut size = 0usize;
        let flush = |current: &mut Vec<WireEvent>, bodies: &mut Vec<Vec<u8>>, run: &str| {
            if current.is_empty() {
                return;
            }
            let batch = Batch {
                collector_run: run.to_string(),
                events: std::mem::take(current),
            };
            bodies.push(serde_json::to_vec(&batch).expect("a batch serializes"));
        };
        for mut e in events {
            e.seq = seq;
            seq += 1;
            let len = serde_json::to_vec(&e).map(|v| v.len()).unwrap_or(0) + 1;
            if current.len() >= MAX_EVENTS || size + len > MAX_BATCH_BYTES {
                flush(&mut current, &mut bodies, &inner.run);
                size = 0;
            }
            size += len;
            current.push(e);
        }
        flush(&mut current, &mut bodies, &inner.run);
        let total: u64 = bodies.iter().map(|b| b.len() as u64).sum();
        if !inner.spool.fits(total).map_err(SpoolError::Io)? {
            return Err(SpoolError::Full);
        }
        for b in &bodies {
            inner.spool.push(b).map_err(SpoolError::Io)?;
        }
        inner.next_seq = seq;
        Ok(())
    }

    /// Take one OTLP request's records in: classify, count, spool. On any failure nothing is
    /// kept (the counting state is put back), so OpenShell's retry is taken whole.
    pub fn take(&self, records: &[otlp::LogRecord], now: DateTime<Utc>) -> Result<(), SpoolError> {
        let mut inner = self.inner.lock().expect("collector lock");
        let before = inner.limiter.clone();
        let mut events = Vec::new();
        for r in records {
            events.extend(inner.limiter.take(ocsf::classify(r), now));
        }
        match Self::spool_events(&mut inner, events) {
            Ok(()) => Ok(()),
            Err(e) => {
                inner.limiter = before;
                Err(e)
            }
        }
    }

    /// End the minutes and windows that are over (`all`: every one, at shutdown) and spool the
    /// summaries. A full spool keeps them counted for the next try.
    pub fn flush_at(&self, now: DateTime<Utc>, all: bool) -> Result<(), SpoolError> {
        let mut inner = self.inner.lock().expect("collector lock");
        let before = inner.limiter.clone();
        let events = inner.limiter.flush(now, all);
        match Self::spool_events(&mut inner, events) {
            Ok(()) => Ok(()),
            Err(e) => {
                inner.limiter = before;
                Err(e)
            }
        }
    }

    /// Send the oldest spooled batch. `Ok(true)` when one was sent (or set aside), `Ok(false)`
    /// when the spool is empty, `Err` when the tool server didn't take it (retry later).
    pub async fn send_one(&self) -> Result<bool, String> {
        let next = {
            let inner = self.inner.lock().expect("collector lock");
            inner.spool.oldest().map_err(|e| e.to_string())?
        };
        let Some((path, batch)) = next else {
            return Ok(false);
        };
        let res = self
            .http
            .post(format!(
                "{}/internal/sandbox-audit/batch",
                self.config.toolserver_url
            ))
            .bearer_auth(&self.config.toolserver_token)
            .json(&batch)
            .send()
            .await
            .map_err(|e| format!("the tool server didn't answer: {}", e.without_url()))?;
        let status = res.status();
        let inner = self.inner.lock().expect("collector lock");
        if status.is_success() {
            inner.spool.remove(&path).map_err(|e| e.to_string())?;
            drop(inner);
            let mut inner = self.inner.lock().expect("collector lock");
            if batch.collector_run == inner.run {
                let last = batch.events.last().map_or(0, |e| e.seq);
                inner.acked_seq = inner.acked_seq.max(last);
            }
            Ok(true)
        } else if status == reqwest::StatusCode::BAD_REQUEST {
            tracing::error!(
                batch = %path.display(),
                "the tool server refused a batch; it is kept in rejected/ and its hole is recorded as a gap"
            );
            inner.spool.reject(&path).map_err(|e| e.to_string())?;
            Ok(true)
        } else {
            Err(format!("the tool server answered {status}"))
        }
    }

    /// One heartbeat: the run and the last `seq` the tool server took.
    pub async fn heartbeat(&self) -> Result<(), String> {
        let hb = {
            let inner = self.inner.lock().expect("collector lock");
            Heartbeat {
                collector_run: inner.run.clone(),
                acked_seq: inner.acked_seq,
            }
        };
        let res = self
            .http
            .post(format!(
                "{}/internal/sandbox-audit/heartbeat",
                self.config.toolserver_url
            ))
            .bearer_auth(&self.config.toolserver_token)
            .json(&hb)
            .send()
            .await
            .map_err(|e| format!("the tool server didn't answer: {}", e.without_url()))?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("the tool server answered {}", res.status()))
        }
    }

    /// The sender, heartbeat and flusher, until `cancel`; at the end every open minute and
    /// window is flushed and spooled.
    pub fn spawn_tasks(&self, cancel: CancellationToken) -> Vec<tokio::task::JoinHandle<()>> {
        let sender = {
            let c = self.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut backoff = Duration::from_millis(500);
                loop {
                    match c.send_one().await {
                        Ok(true) => {
                            backoff = Duration::from_millis(500);
                            continue;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "batch not delivered; retrying from the spool");
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                        }
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }
            })
        };
        let beat = {
            let c = self.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(HEARTBEAT_EVERY);
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tick.tick() => {}
                    }
                    if let Err(e) = c.heartbeat().await {
                        tracing::warn!(error = %e, "heartbeat not delivered");
                    }
                }
            })
        };
        let flusher = {
            let c = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(FLUSH_EVERY);
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            if let Err(e) = c.flush_at(Utc::now(), true) {
                                tracing::error!(error = ?e, "the last summaries couldn't be spooled at shutdown");
                            }
                            return;
                        }
                        _ = tick.tick() => {}
                    }
                    if let Err(e) = c.flush_at(Utc::now(), false) {
                        tracing::warn!(error = ?e, "summaries not spooled yet (kept counted)");
                    }
                }
            })
        };
        vec![sender, beat, flusher]
    }
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return false;
    };
    use sha2::{Digest, Sha256};
    let p = Sha256::digest(presented.as_bytes());
    let e = Sha256::digest(token.as_bytes());
    bool::from(p.as_slice().ct_eq(e.as_slice()))
}

/// `POST /v1/logs`.
async fn receive(State(c): State<Collector>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &c.config.otlp_token) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !ct.starts_with("application/x-protobuf") {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "OTLP/HTTP protobuf only (application/x-protobuf)",
        )
            .into_response();
    }
    // Verify on a real host: OpenShell's exporter sends uncompressed (no gzip support here).
    if let Some(enc) = headers.get(header::CONTENT_ENCODING) {
        if enc.as_bytes() != b"identity" {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "uncompressed only: set the exporter's compression to none",
            )
                .into_response();
        }
    }
    let records = match otlp::decode(&body, MAX_RECORDS) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "an OTLP request that isn't a logs export");
            return (StatusCode::BAD_REQUEST, "not an OTLP logs export").into_response();
        }
    };
    match c.take(&records, Utc::now()) {
        // An empty ExportLogsServiceResponse.
        Ok(()) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-protobuf")],
            Vec::<u8>::new(),
        )
            .into_response(),
        Err(SpoolError::Full) => {
            tracing::error!("the spool is full: OpenShell is told 503 and reports its own gap");
            (StatusCode::SERVICE_UNAVAILABLE, "spool full").into_response()
        }
        Err(SpoolError::Io(e)) => {
            tracing::error!(error = %e, "the spool can't be written");
            (StatusCode::SERVICE_UNAVAILABLE, "spool unavailable").into_response()
        }
    }
}
