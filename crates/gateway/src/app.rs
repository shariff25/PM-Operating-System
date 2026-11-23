//! The gateway's HTTP API: the OpenAI shapes the harness already speaks, in front of the
//! local model servers.
//!
//! - `POST /v1/chat/completions` (streaming or not) and `POST /v1/embeddings`: `model` must be
//!   a role alias, `pmos/<role>`; the gateway swaps in the role's model and forwards to its
//!   local server. A concrete model name is refused, so callers never depend on one.
//! - `GET /v1/models`: the role aliases.
//! - `GET /v1/pmos/workarounds`: the workaround registry and which roles use what (D11).
//! - `GET /healthz` (process up), `GET /readyz` (every model server answers; `class` and `stub`
//!   say what kind of gateway is serving, `cloud_test` whether a testing-only upstream, whose
//!   prompts leave this machine, is on).
//!
//! - `GET /v1/pmos/roles`: each role's model id (or its `label:`, when the config gives one)
//!   and upstream kind (never an address), and whether the stub or a testing-only upstream is
//!   on, for the benchmark runner's report.
//!
//! A role with a `label:` is reported by that label everywhere a client or a log sees it (the
//! roles answer, `x-pmos-model`, an answer's `model`, the usage log); its real model id is only
//! sent upstream.
//!
//! Every `/v1` call needs a client token. `X-Pmos-User` (a handle), `X-Pmos-Priority`
//! (`interactive`, `scheduled` or `personal`) and `X-Pmos-Run` place it in the queue and the
//! usage log. The first two are believed only from a client `clients:` marks `trusted`; any
//! other client's calls are keyed by its name and get its configured priority. A trusted
//! client with `user: from-name` (each PM's own bot, `bot-pm-<handle>`) may name only the PM
//! its name spells; naming anyone else is `403`, recorded in the usage log (review finding
//! R1). Every client has its own cap on calls in flight and on calls waiting, and so has each
//! believed person (`config::ClientPolicy`, review finding R2).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::anthropic;
use crate::config::{Client, ClientPolicy, Models, Role, UpstreamKind};
use crate::remote::{StreamScanner, ToolCallExtras};
use crate::retry::RetryPolicy;
use crate::scheduler::{Busy, Owner, Permit, Priority, Scheduler};
use crate::usage::{UsageEvent, UsageLog};
use crate::{stub, workarounds};

/// Largest request accepted: a long conversation with tool results, not a file upload.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct GatewayConfig {
    pub models: Models,
    pub clients: Vec<Client>,
    pub usage: Arc<UsageLog>,
    pub stub: bool,
    pub workarounds_on: bool,
    /// The Claude API key for testing-only Claude roles (`anthropic.rs`); unused otherwise.
    pub anthropic_key: String,
    /// The bearer key for testing-only remote OpenAI-compatible roles; unused otherwise.
    pub remote_key: String,
    /// How a `429`/`503` from an upstream is retried (`retry.rs`).
    pub retry: RetryPolicy,
    /// How long an upstream may take ([`Timeouts`]).
    pub timeouts: Timeouts,
}

/// How long an upstream may take. A streamed answer sends bytes as it goes, so a stream that
/// goes silent for `stream_silence` is stuck. An answer that isn't streamed (most calls the
/// harness makes, and every call to the Claude test upstream) sends nothing until it's
/// whole, so it gets `whole_answer` from the request to its last byte: with only the silence
/// limit, every generation longer than it was cut off as a `502`.
///
/// `ready_cache`: how long `GET /readyz` reuses its last probe of the model servers (the Low
/// on unauthenticated probes, review at c78b5fb). The route needs no token, so before, every
/// request to it sent a request to each upstream; now a flood of them sends at most one
/// round of probes per `ready_cache`. [`READY_CACHE`] by default; zero probes every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub stream_silence: Duration,
    pub whole_answer: Duration,
    pub ready_cache: Duration,
}

/// How long `GET /readyz` reuses a probe of the model servers, by default.
pub const READY_CACHE: Duration = Duration::from_secs(5);

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            stream_silence: Duration::from_secs(120),
            whole_answer: Duration::from_secs(600),
            ready_cache: READY_CACHE,
        }
    }
}

/// The last probe of the model servers for `GET /readyz`: when it was made, and whether each
/// upstream base answered.
type Probe = (Instant, std::collections::HashMap<String, bool>);

struct Inner {
    retry: RetryPolicy,
    models: Models,
    clients: Vec<Client>,
    usage: Arc<UsageLog>,
    sched: Arc<Scheduler>,
    /// For streamed answers: [`Timeouts::stream_silence`] between bytes.
    http: reqwest::Client,
    /// For answers that arrive whole (not streamed, and the testing-only Claude upstream):
    /// [`Timeouts::whole_answer`] in all.
    whole_http: reqwest::Client,
    stub: bool,
    workarounds_on: bool,
    anthropic_key: String,
    remote_key: String,
    /// Remote test upstreams only: tool-call `extra_content` to give back (`remote.rs`).
    extras: Arc<ToolCallExtras>,
    /// [`Timeouts::ready_cache`].
    ready_cache: Duration,
    /// `GET /readyz`'s last probe. Held across a probe, so callers that arrive while one
    /// runs wait for it and use its result rather than probing again.
    probe: tokio::sync::Mutex<Option<Probe>>,
}

#[derive(Clone)]
struct AppState(Arc<Inner>);

#[derive(Clone)]
struct ClientName(String);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Chat,
    Embeddings,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Chat => "chat/completions",
            Self::Embeddings => "embeddings",
        }
    }
}

pub fn router(mut cfg: GatewayConfig) -> Router {
    // With the stub model on, the gateway is a `test` one whatever its config says, so a stub
    // run never reports `production` (`Config::served_class`).
    if cfg.stub {
        cfg.models.class = crate::config::GatewayClass::Test;
    }
    let sched = Scheduler::new(cfg.models.limits);
    // Both clients use the system proxy (`HTTPS_PROXY`, `NO_PROXY`) and trust store, so a
    // remote test upstream works from behind a proxy.
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        // A long generation streams for minutes; a server that goes silent this long is stuck.
        .read_timeout(cfg.timeouts.stream_silence)
        .build()
        .expect("http client builds");
    let whole_http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(cfg.timeouts.whole_answer)
        .build()
        .expect("http client builds");
    // Summary lines for held refusals (`UsageLog::record_refusal`), a few seconds after
    // their minute ends. The task stops when the log is gone.
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        let usage = Arc::downgrade(&cfg.usage);
        rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                match usage.upgrade() {
                    Some(u) => u.flush_refusals(),
                    None => break,
                }
            }
        });
    }
    let state = AppState(Arc::new(Inner {
        models: cfg.models,
        clients: cfg.clients,
        usage: cfg.usage,
        sched,
        http,
        whole_http,
        stub: cfg.stub,
        workarounds_on: cfg.workarounds_on,
        anthropic_key: cfg.anthropic_key,
        remote_key: cfg.remote_key,
        retry: cfg.retry,
        extras: Arc::default(),
        ready_cache: cfg.timeouts.ready_cache,
        probe: tokio::sync::Mutex::new(None),
    }));
    let v1 = Router::new()
        .route("/models", get(models))
        .route("/chat/completions", post(chat))
        .route("/embeddings", post(embeddings))
        .route("/pmos/workarounds", get(list_workarounds))
        .route("/pmos/roles", get(list_roles))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .nest("/v1", v1)
        .with_state(state)
}

/// An error in the shape OpenAI clients expect.
fn error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error": {"message": message.into(), "type": kind}})),
    )
        .into_response()
}

async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(pmos_runtime::bearer_value);
    let client = presented.and_then(|t| {
        state
            .0
            .clients
            .iter()
            .find(|c| pmos_runtime::token_eq(t, &c.token))
    });
    match client {
        Some(c) => {
            req.extensions_mut().insert(ClientName(c.name.clone()));
            next.run(req).await
        }
        None => error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "a PM OS gateway token is required",
        ),
    }
}

async fn models(State(state): State<AppState>) -> Response {
    let data: Vec<Value> = state
        .0
        .models
        .aliases()
        .into_iter()
        .map(|id| json!({"id": id, "object": "model", "owned_by": "pmos"}))
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn list_workarounds(State(state): State<AppState>) -> Response {
    let entries: Vec<Value> = workarounds::REGISTRY
        .iter()
        .map(|w| {
            let roles: Vec<String> = state
                .0
                .models
                .roles
                .values()
                .filter(|r| r.workarounds.iter().any(|id| id == w.id))
                .map(Role::alias)
                .collect();
            json!({"id": w.id, "what": w.what, "delete_when": w.delete_when, "roles": roles})
        })
        .collect();
    Json(json!({"enabled": state.0.workarounds_on, "workarounds": entries})).into_response()
}

/// Which model serves each role, as this gateway runs now: the model id and the kind of
/// upstream (`local`, `claude-test` or `remote-test`), never its address or host, and whether
/// the stub model, a testing-only upstream or the workarounds are on. The benchmark runner
/// records this in its report (review finding N5), so a report says which models it measured
/// without the operator typing them.
async fn list_roles(State(state): State<AppState>) -> Response {
    // `class`: `production` only for a config that says so (`deploy/gateway.yaml`), which
    // never has a testing-only upstream (review finding R3), and only with the stub off
    // (`router` serves a stub gateway as `test`).
    let roles: Vec<Value> = state
        .0
        .models
        .roles
        .values()
        .map(|r| {
            json!({"role": r.name, "alias": r.alias(), "model": r.reported_model(),
                   "upstream": r.upstream_kind_label()})
        })
        .collect();
    let cloud_test = state
        .0
        .models
        .roles
        .values()
        .any(|r| r.kind.is_cloud_test());
    Json(json!({
        "class": state.0.models.class.as_str(),
        "stub": state.0.stub,
        "cloud_test": cloud_test,
        "workarounds": state.0.workarounds_on,
        "roles": roles,
    }))
    .into_response()
}

/// `GET /readyz`. Every answer says the gateway's `class` as served (`production` only for a
/// `class: production` config with the stub off) and whether the stub is on, never an address
/// or a key, so `deploy/pmos-admin status` can say plainly when a host serves stub answers or
/// isn't production-class (finding G2 of the review at 6bb806f: a host left on the first
/// install's `PMOS_GATEWAY_STUB=1` answered ready with nothing else to show it).
///
/// `upstreams` is keyed by role (`default`, `extraction`, ...), each with its upstream's kind
/// label and `ok` or `down`: labels only, never an address, host name, URL or key. The route
/// needs no token, and before the review at 6bb806f its keys were the model servers' base
/// URLs, so it named their hosts (and a remote test upstream's provider).
///
/// The probe of the model servers is reused for [`Timeouts::ready_cache`] (5 s by default),
/// so unauthenticated callers can't make the gateway probe every upstream on each request;
/// `running` and `waiting` are always read afresh.
async fn readyz(State(state): State<AppState>) -> Response {
    let (running, waiting) = state.0.sched.snapshot();
    let class = state.0.models.class.as_str();
    if state.0.stub {
        return Json(json!({"ready": true, "class": class, "stub": true,
                           "running": running, "waiting": waiting}))
        .into_response();
    }
    let cloud_test = state
        .0
        .models
        .roles
        .values()
        .any(|r| r.kind.is_cloud_test());
    let up_by_base = probe_upstreams(&state.0).await;
    let ok = up_by_base.values().all(|up| *up);
    // By role, with labels only (no address, host, URL or key).
    let checks: serde_json::Map<String, Value> = state
        .0
        .models
        .roles
        .values()
        .map(|r| {
            let up = up_by_base.get(&r.upstream).copied().unwrap_or(false);
            (
                r.name.clone(),
                json!({"upstream": r.upstream_kind_label(),
                       "status": if up { "ok" } else { "down" }}),
            )
        })
        .collect();
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(
            json!({"ready": ok, "class": class, "stub": false, "upstreams": checks,
                    "cloud_test": cloud_test, "running": running, "waiting": waiting}),
        ),
    )
        .into_response()
}

/// Whether each upstream base answers `GET <base>/models`: the cached probe while it is
/// younger than [`Timeouts::ready_cache`], else a new one.
async fn probe_upstreams(inner: &Inner) -> std::collections::HashMap<String, bool> {
    let mut cached = inner.probe.lock().await;
    if let Some((at, result)) = cached.as_ref() {
        if at.elapsed() < inner.ready_cache {
            return result.clone();
        }
    }
    let mut upstreams: Vec<String> = inner
        .models
        .roles
        .values()
        .map(|r| r.upstream.clone())
        .collect();
    upstreams.sort();
    upstreams.dedup();
    let mut up_by_base = std::collections::HashMap::new();
    for base in upstreams {
        let remote = inner
            .models
            .roles
            .values()
            .any(|r| r.upstream == base && r.kind == UpstreamKind::RemoteOpenAi);
        let probe = if remote {
            inner
                .http
                .get(format!("{base}/models"))
                .bearer_auth(&inner.remote_key)
        } else {
            inner.http.get(format!("{base}/models"))
        };
        let up = tokio::time::timeout(
            Duration::from_secs(if remote { 5 } else { 2 }),
            probe.send(),
        )
        .await
        .is_ok_and(|r| r.is_ok_and(|r| r.status().is_success()));
        up_by_base.insert(base, up);
    }
    // Timed from the end of the probe, so a slow one is still reused for the full period.
    *cached = Some((Instant::now(), up_by_base.clone()));
    up_by_base
}

async fn chat(
    State(state): State<AppState>,
    Extension(client): Extension<ClientName>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, client.0, headers, body, Endpoint::Chat).await
}

async fn embeddings(
    State(state): State<AppState>,
    Extension(client): Extension<ClientName>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, client.0, headers, body, Endpoint::Embeddings).await
}

/// Who is asking, at what priority, for which run.
struct Caller {
    user: Option<String>,
    run: Option<String>,
    priority: Priority,
}

/// Why a call's headers were refused.
enum Refused {
    /// Malformed (`400`), from any client.
    Malformed(String),
    /// A trusted client named a person it may not name (`403`, review finding R1): the
    /// handle it named.
    NotItsPerson(String),
}

/// The caller's headers, checked, and then believed only as far as its client is trusted:
/// a client `clients:` doesn't mark `trusted` is keyed by its own name and gets its
/// configured priority, whatever `X-Pmos-User` and `X-Pmos-Priority` say (review finding
/// N3). A trusted client may name only the people its `user` rule allows (R1). Malformed
/// headers are refused from every client.
fn caller(headers: &HeaderMap, client: &str, policy: &ClientPolicy) -> Result<Caller, Refused> {
    let declared = declared(headers).map_err(Refused::Malformed)?;
    if policy.trusted {
        if let Some(u) = &declared.user {
            if !policy.may_name(client, u) {
                return Err(Refused::NotItsPerson(u.clone()));
            }
        }
    }
    Ok(if policy.trusted {
        Caller {
            // One key per person, whichever client names them (`config::person_key`, G1).
            user: declared.user.map(|u| crate::config::person_key(&u)),
            run: declared.run,
            priority: declared.priority.unwrap_or(policy.priority),
        }
    } else {
        Caller {
            user: None,
            run: declared.run,
            priority: policy.priority,
        }
    })
}

/// What the headers declare, before trust is applied.
struct Declared {
    user: Option<String>,
    run: Option<String>,
    priority: Option<Priority>,
}

fn declared(headers: &HeaderMap) -> Result<Declared, String> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let user = match text("x-pmos-user") {
        None => None,
        Some(u) if pmos_contract::model::valid_handle(u) => Some(u.to_string()),
        Some(u) => return Err(format!("X-Pmos-User `{u}` is not a PM OS handle")),
    };
    let priority = match text("x-pmos-priority") {
        None => None,
        Some(p) => Some(Priority::parse(p).ok_or_else(|| {
            format!("X-Pmos-Priority must be interactive, scheduled or personal, not `{p}`")
        })?),
    };
    let run = match text("x-pmos-run") {
        None => None,
        Some(r) if !r.is_empty() && r.len() <= 128 => Some(r.to_string()),
        Some(_) => return Err("X-Pmos-Run must be 1–128 characters".into()),
    };
    Ok(Declared {
        user,
        run,
        priority,
    })
}

/// Why no slot was had.
#[derive(Clone, Copy)]
enum NoSlot {
    /// The queue had no room for it ([`Busy`]).
    Refused(Busy),
    /// It waited `limits.queue-wait-secs` and none came free.
    TimedOut,
}

impl NoSlot {
    /// `429` when the caller's own client or person already had its share waiting (R2), else
    /// `503`; with `Retry-After` either way.
    fn status(self) -> StatusCode {
        match self {
            Self::Refused(Busy::Client | Busy::Person) => StatusCode::TOO_MANY_REQUESTS,
            Self::Refused(Busy::Full) | Self::TimedOut => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Refused(Busy::Full) => "the model queue is full; try again shortly",
            Self::Refused(Busy::Client) => {
                "this client already has as many model calls waiting as it may; try again shortly"
            }
            Self::Refused(Busy::Person) => {
                "this person already has as many model calls waiting as they may; try again shortly"
            }
            Self::TimedOut => "no model slot came free in time; try again shortly",
        }
    }
}

/// Waits for a slot for at most `limits.queue-wait-secs`. `Err`: no room to wait, or the wait
/// ran out; either way the place in the queue has been given back.
async fn slot(inner: &Inner, owner: &Owner, priority: Priority) -> Result<Permit, NoSlot> {
    let wait = Duration::from_secs(inner.models.limits.queue_wait_secs);
    match tokio::time::timeout(wait, inner.sched.acquire(owner, priority)).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(why)) => Err(NoSlot::Refused(why)),
        Err(_) => Err(NoSlot::TimedOut),
    }
}

fn busy(why: NoSlot) -> Response {
    let kind = if why.status() == StatusCode::TOO_MANY_REQUESTS {
        "rate_limit_error"
    } else {
        "server_busy"
    };
    let mut res = error(why.status(), kind, why.message());
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    res
}

/// Why an upstream's whole answer couldn't be read.
enum ReadError {
    /// More than `limits.max-answer-bytes`.
    TooLarge,
    /// The connection broke, or the answer timed out (the error, without its URL).
    Broken(reqwest::Error),
}

/// Reads a whole answer, refusing one larger than `max` bytes.
async fn read_capped(mut res: reqwest::Response, max: usize) -> Result<Bytes, ReadError> {
    if res.content_length().is_some_and(|n| n > max as u64) {
        return Err(ReadError::TooLarge);
    }
    let mut buf = bytes::BytesMut::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| ReadError::Broken(e.without_url()))?
    {
        if buf.len() + chunk.len() > max {
            return Err(ReadError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

/// What one try at the upstream came to.
enum Attempt {
    /// An answer to use (success, or an error that retrying wouldn't change).
    Live(reqwest::Response),
    /// A `429`/`503`, read in full so its `Retry-After` and error body can be judged.
    Failed {
        status: u16,
        headers: HeaderMap,
        body: Bytes,
    },
    /// No answer at all.
    Down(reqwest::Error),
}

/// Logs the event when a streamed answer ends (finished or the caller hung up), and holds the
/// slot until then.
struct StreamEnd {
    usage: Arc<UsageLog>,
    event: UsageEvent,
    started: Instant,
    _permit: Permit,
}

impl Drop for StreamEnd {
    fn drop(&mut self) {
        self.event.latency_ms = self.started.elapsed().as_millis() as u64;
        self.usage.record(self.event.clone());
    }
}

type Upstream = std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// What a stream that runs past its limits ends with, as the caller sees it.
const STREAM_TOO_LONG: &str = "the streamed answer took longer than this gateway allows";
const STREAM_TOO_LARGE: &str = "the streamed answer is larger than this gateway accepts";

/// The caller's side of a streamed answer: the upstream's bytes as they come, until the
/// upstream ends, breaks, passes `max_bytes` in all, or reaches `deadline`. A stream cut off
/// that way ends with an error, so the caller sees a broken answer rather than a short one
/// that looks whole, and the usage line says `504` (too long) or `502` (broken or too large).
///
/// `end` lives as long as the stream: when the answer ends, or the caller hangs up and the
/// body is dropped, it logs the call and frees the slot, and dropping the upstream stream with
/// it closes the connection to the model server, so generation stops there too.
fn bounded_stream(
    upstream: Upstream,
    end: StreamEnd,
    scanner: Option<StreamScanner>,
    deadline: tokio::time::Instant,
    max_bytes: usize,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    struct Flow {
        upstream: Upstream,
        end: StreamEnd,
        scanner: Option<StreamScanner>,
        seen: usize,
        done: bool,
    }
    let flow = Flow {
        upstream,
        end,
        scanner,
        seen: 0,
        done: false,
    };
    futures_util::stream::unfold(flow, move |mut f| async move {
        if f.done {
            return None;
        }
        let cut = |f: &mut Flow, status: u16, why: String| {
            f.done = true;
            f.end.event.status = status;
            tracing::warn!(role = %f.end.event.role, status, reason = %why, "streamed answer cut off");
            Err(std::io::Error::other(why))
        };
        match tokio::time::timeout_at(deadline, f.upstream.next()).await {
            Err(_) => {
                let e = cut(&mut f, 504, STREAM_TOO_LONG.into());
                Some((e, f))
            }
            Ok(None) => None,
            // Never the upstream's URL.
            Ok(Some(Err(e))) => {
                let e = cut(&mut f, 502, e.without_url().to_string());
                Some((e, f))
            }
            Ok(Some(Ok(bytes))) => {
                f.seen += bytes.len();
                if f.seen > max_bytes {
                    let e = cut(&mut f, 502, STREAM_TOO_LARGE.into());
                    return Some((e, f));
                }
                if let Some(s) = &mut f.scanner {
                    s.feed(&bytes);
                }
                Some((Ok(bytes), f))
            }
        }
    })
}

async fn forward(
    state: AppState,
    client: String,
    headers: HeaderMap,
    body: Bytes,
    endpoint: Endpoint,
) -> Response {
    let inner = &state.0;
    let started = Instant::now();
    let mut req: Value = match serde_json::from_slice(&body) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "the body must be a JSON object",
            )
        }
    };
    let alias = req["model"].as_str().unwrap_or_default().to_string();
    let Some(role) = inner.models.role(&alias).cloned() else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "unknown model `{alias}`; use one of {}",
                inner.models.aliases().join(", ")
            ),
        );
    };
    let policy = inner.models.client_policy(&client);
    let stream = endpoint == Endpoint::Chat && req["stream"] == json!(true);
    // A usage line for a call refused before it reached the queue or a slot.
    let refused_line = |status: u16,
                        user: Option<String>,
                        run: Option<String>,
                        priority: Priority,
                        refused: &'static str| UsageEvent {
        at: Utc::now(),
        client: client.clone(),
        user,
        run,
        endpoint: endpoint.path(),
        role: role.name.clone(),
        model: role.reported_model().to_string(),
        upstream: role.logged_upstream_host(),
        priority,
        stream,
        stub: inner.stub,
        status,
        attempt: 0,
        attempts: 0,
        retrying: false,
        queue_ms: started.elapsed().as_millis() as u64,
        latency_ms: started.elapsed().as_millis() as u64,
        prompt_tokens: None,
        completion_tokens: None,
        workarounds: Vec::new(),
        refused: Some(refused),
        refused_count: None,
        refused_users: Vec::new(),
        refused_users_more: None,
        user_key: None,
    };
    let who = match caller(&headers, &client, &policy) {
        Ok(c) => c,
        Err(Refused::Malformed(m)) => {
            return error(StatusCode::BAD_REQUEST, "invalid_request_error", m)
        }
        Err(Refused::NotItsPerson(named)) => {
            // R1: the client's token is believed only for its own PM. The line names whom it
            // tried to name, so an operator sees a worker token used for someone else. A
            // looping token gets its first refusal recorded and then a count a minute.
            // `user_key` is the person key every other line uses for that handle (its
            // spelled form, `config::person_key`), so the refusal can be matched to them.
            let user_key = crate::config::person_key(&named);
            let mut line = refused_line(
                403,
                Some(named.clone()),
                None,
                policy.priority,
                "names a person this client may not name",
            );
            line.user_key = Some(user_key.clone());
            if inner.usage.record_refusal(line) {
                tracing::warn!(
                    client = %client, named = %named, user_key = %user_key,
                    "a client named a person it may not name"
                );
            }
            return error(
                StatusCode::FORBIDDEN,
                "permission_error",
                "this client may not make calls for that person (X-Pmos-User)",
            );
        }
    };
    req["model"] = json!(role.model);

    // The person's cap applies when the gateway believes the person (a trusted client's
    // `X-Pmos-User`); the client's own cap always applies. Without a believed person, the
    // client's cap alone bounds its calls. The same holds for calls waiting (R2).
    // A client may hold only its share of the person's running slots (G3).
    // Clients that may name anyone hold at most their shared cap of the person's running
    // slots together, so with a PM's own bot configured, its calls never wait on theirs (U1).
    let owner = Owner::new(who.user.as_deref(), &client, policy.max_in_flight)
        .with_queue(policy.max_queue)
        .with_person_share(policy.per_user_share)
        .with_shared_cap(policy.shared_person_cap);
    let permit = match slot(inner, &owner, who.priority).await {
        Ok(p) => p,
        Err(why) => {
            if let NoSlot::Refused(Busy::Client | Busy::Person) = why {
                inner.usage.record_refusal(refused_line(
                    429,
                    who.user.clone(),
                    who.run.clone(),
                    who.priority,
                    why.message(),
                ));
            }
            return busy(why);
        }
    };
    let mut event = UsageEvent {
        at: Utc::now(),
        client,
        user: who.user,
        run: who.run,
        endpoint: endpoint.path(),
        role: role.name.clone(),
        model: role.reported_model().to_string(),
        upstream: role.logged_upstream_host(),
        priority: who.priority,
        stream,
        stub: inner.stub,
        status: 200,
        attempt: 1,
        attempts: 1,
        retrying: false,
        queue_ms: started.elapsed().as_millis() as u64,
        latency_ms: 0,
        prompt_tokens: None,
        completion_tokens: None,
        workarounds: Vec::new(),
        refused: None,
        refused_count: None,
        refused_users: Vec::new(),
        refused_users_more: None,
        user_key: None,
    };
    let model_header = HeaderValue::from_str(role.reported_model()).ok();
    let finish = |mut res: Response| {
        if let Some(v) = model_header.clone() {
            res.headers_mut().insert("x-pmos-model", v);
        }
        res
    };

    if inner.stub {
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        drop(permit);
        return finish(match (endpoint, stream) {
            (Endpoint::Chat, true) => (
                [(header::CONTENT_TYPE, "text/event-stream")],
                stub::chat_stream(&req, role.reported_model()),
            )
                .into_response(),
            (Endpoint::Chat, false) => {
                Json(stub::chat(&req, role.reported_model())).into_response()
            }
            (Endpoint::Embeddings, _) => {
                Json(stub::embeddings(&req, role.reported_model())).into_response()
            }
        });
    }

    if role.kind == UpstreamKind::ClaudeTest {
        let res = claude(inner, &role, endpoint, &req, stream, &mut event).await;
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        drop(permit);
        return finish(res);
    }

    let url = format!("{}/{}", role.upstream, endpoint.path());
    let remote = role.kind == UpstreamKind::RemoteOpenAi;
    if remote {
        let restored = inner.extras.restore(&role.upstream, &mut req);
        if restored > 0 {
            tracing::debug!(role = %role.alias(), restored, "restored tool-call extra_content");
        }
    }

    // Try the upstream, retrying a 429/503 or a failed connection within the policy's limits
    // (`retry.rs`). Each failed attempt that will be retried gets its own usage line, and the
    // slot is given back while waiting so retries don't hold up the queue; the same priority
    // takes it again afterwards. A streamed answer is only ever retried here, before any byte
    // of it has gone to the caller.
    let mut permit = Some(permit);
    let first_attempt = Instant::now();
    let mut attempt = 0u32;
    let outcome = loop {
        attempt += 1;
        // Not streamed: nothing arrives until the answer is whole, so the silence limit
        // would cut off every long generation.
        let client = if stream {
            &inner.http
        } else {
            &inner.whole_http
        };
        let post = client.post(&url);
        // The client's own gateway token never goes upstream; the remote key does.
        let post = if remote {
            post.bearer_auth(&inner.remote_key)
        } else {
            post
        };
        let this = match post.json(&req).send().await {
            Ok(r) if !crate::retry::is_retryable_status(r.status().as_u16()) => Attempt::Live(r),
            Ok(r) => {
                let status = r.status().as_u16();
                let headers = r.headers().clone();
                let body = read_capped(r, inner.models.limits.max_answer_bytes)
                    .await
                    .unwrap_or_default();
                Attempt::Failed {
                    status,
                    headers,
                    body,
                }
            }
            Err(e) => Attempt::Down(e),
        };
        let (status, headers, body) = match &this {
            Attempt::Live(_) => break this,
            Attempt::Failed {
                status,
                headers,
                body,
            } => (*status, headers.clone(), body.clone()),
            Attempt::Down(e) if e.is_connect() => (0, HeaderMap::new(), Bytes::new()),
            Attempt::Down(_) => break this,
        };
        let Some(wait) =
            inner
                .retry
                .next_delay(attempt, status, &headers, &body, first_attempt.elapsed())
        else {
            break this;
        };
        tracing::warn!(
            upstream = %role.logged_upstream(), status, attempt, wait_ms = wait.as_millis() as u64,
            "model server refused or was unreachable; retrying"
        );
        let mut line = event.clone();
        line.status = if status == 0 { 502 } else { status };
        line.attempt = attempt;
        line.attempts = attempt;
        line.retrying = true;
        line.latency_ms = first_attempt.elapsed().as_millis() as u64;
        inner.usage.record(line);
        drop(permit.take());
        tokio::time::sleep(wait).await;
        let waited = Instant::now();
        match slot(inner, &owner, who.priority).await {
            Ok(p) => permit = Some(p),
            // No room to wait again, or no slot in time: the caller gets what the upstream
            // last said.
            Err(_) => break this,
        }
        event.queue_ms += waited.elapsed().as_millis() as u64;
    };
    event.attempt = attempt;
    event.attempts = attempt;
    let finish = |res: Response| {
        let mut res = finish(res);
        res.headers_mut()
            .insert("x-pmos-upstream-attempts", HeaderValue::from(attempt));
        res
    };
    let res = match outcome {
        Attempt::Live(r) => r,
        Attempt::Failed {
            status,
            headers,
            body,
        } => {
            // Out of retries: the upstream's own answer, as it would have been without them.
            event.status = status;
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            let mut res = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response();
            if let Some(v) = headers.get(header::RETRY_AFTER) {
                res.headers_mut().insert(header::RETRY_AFTER, v.clone());
            }
            return finish(res);
        }
        Attempt::Down(e) => {
            tracing::warn!(
                upstream = %role.logged_upstream(),
                error = %logged_error(&role, &e),
                cause = %causes(&e),
                timeout = e.is_timeout(),
                "model server unreachable"
            );
            event.status = 502;
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            return finish(error(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!("the `{}` model server is unavailable", role.name),
            ));
        }
    };
    let permit = permit.expect("a live answer holds its slot");
    let status = res.status();
    event.status = status.as_u16();

    if stream && status.is_success() {
        let end = StreamEnd {
            usage: inner.usage.clone(),
            event,
            started,
            _permit: permit,
        };
        // Remote test upstreams: remember tool-call extra_content as it passes (`remote.rs`).
        let scanner = remote.then(|| StreamScanner::new(inner.extras.clone(), &role.upstream));
        let limits = inner.models.limits;
        let body = bounded_stream(
            Box::pin(res.bytes_stream()),
            end,
            scanner,
            tokio::time::Instant::now() + Duration::from_secs(limits.stream_max_secs),
            limits.max_answer_bytes,
        );
        let body = match &role.label {
            Some(label) => Body::from_stream(relabel_stream(body, label.clone())),
            None => Body::from_stream(body),
        };
        return finish(
            (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                body,
            )
                .into_response(),
        );
    }

    let bytes = match read_capped(res, inner.models.limits.max_answer_bytes).await {
        Ok(b) => b,
        Err(e) => {
            event.status = 502;
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            // Never the upstream's URL: `read_capped` strips it from the error.
            let message = match e {
                ReadError::TooLarge => format!(
                    "the `{}` model server's answer is larger than this gateway accepts",
                    role.name
                ),
                ReadError::Broken(e) => {
                    format!("the `{}` model server stopped mid-answer: {e}", role.name)
                }
            };
            return finish(error(StatusCode::BAD_GATEWAY, "upstream_error", message));
        }
    };
    if !status.is_success() {
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        // The model server's own error (bad parameters, context too long) helps the caller.
        return finish(
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response(),
        );
    }
    let mut answer: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            event.status = 502;
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "the model server returned something other than JSON",
            );
        }
    };
    if remote && endpoint == Endpoint::Chat {
        inner.extras.remember_answer(&role.upstream, &answer);
    }
    if endpoint == Endpoint::Chat {
        event.workarounds =
            workarounds::apply(&role.workarounds, inner.workarounds_on, &mut answer);
    }
    if role.label.is_some() {
        relabel(&mut answer, role.reported_model());
    }
    event.prompt_tokens = answer["usage"]["prompt_tokens"].as_u64();
    event.completion_tokens = answer["usage"]["completion_tokens"].as_u64();
    event.latency_ms = started.elapsed().as_millis() as u64;
    inner.usage.record(event);
    drop(permit);
    finish(Json(answer).into_response())
}

/// One call to the testing-only Claude upstream (`anthropic.rs`): the request translated to
/// the Messages API and the answer back. A streamed request gets the whole answer as one
/// event stream once it is ready.
async fn claude(
    inner: &Inner,
    role: &Role,
    endpoint: Endpoint,
    req: &Value,
    stream: bool,
    event: &mut UsageEvent,
) -> Response {
    if endpoint != Endpoint::Chat {
        event.status = 400;
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "the `{}` role is the Claude test upstream, which has no embeddings",
                role.name
            ),
        );
    }
    let body = match anthropic::to_messages(req, &role.model, role.effort.as_deref()) {
        Ok(b) => b,
        Err(m) => {
            event.status = 400;
            return error(StatusCode::BAD_REQUEST, "invalid_request_error", m);
        }
    };
    let res = inner
        .whole_http
        .post(format!("{}/v1/messages", role.upstream))
        .header("x-api-key", &inner.anthropic_key)
        .header("anthropic-version", anthropic::API_VERSION)
        .header("anthropic-beta", anthropic::FALLBACK_BETA)
        .json(&body)
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                upstream = %role.logged_upstream(),
                error = %logged_error(role, &e),
                "Claude test upstream unreachable"
            );
            event.status = 502;
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!(
                    "the `{}` model (Claude test upstream) is unavailable",
                    role.name
                ),
            );
        }
    };
    let status = res.status();
    event.status = status.as_u16();
    let answer: Value = match read_capped(res, inner.models.limits.max_answer_bytes).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or(Value::Null),
        Err(ReadError::TooLarge) => {
            event.status = 502;
            return error(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!(
                    "the `{}` model's answer (Claude test upstream) is larger than this gateway accepts",
                    role.name
                ),
            );
        }
        Err(ReadError::Broken(_)) => Value::Null,
    };
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Claude test upstream refused the request"
        );
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(anthropic::error_body(&answer, status.as_u16())),
        )
            .into_response();
    }
    let answer = anthropic::from_messages(&answer, role.reported_model());
    event.prompt_tokens = answer["usage"]["prompt_tokens"].as_u64();
    event.completion_tokens = answer["usage"]["completion_tokens"].as_u64();
    if stream {
        (
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            anthropic::as_stream(&answer),
        )
            .into_response()
    } else {
        Json(answer).into_response()
    }
}

/// A transport error as the logs show it: for a labelled role ([`Role::label`]) without the
/// upstream's URL, which names the provider's host.
fn logged_error(role: &Role, e: &reqwest::Error) -> String {
    let text = e.to_string();
    match (&role.label, e.url()) {
        (Some(_), Some(url)) => text.replace(url.as_str(), role.upstream_kind_label()),
        _ => text,
    }
}

/// Puts `label` in an answer's `model` field, where the upstream wrote its own model id.
fn relabel(answer: &mut Value, label: &str) {
    if let Some(model) = answer.get_mut("model").filter(|m| m.is_string()) {
        *model = json!(label);
    }
}

/// Rewrites the `model` field of each `data:` line of a streamed answer ([`relabel`]), for a
/// labelled role. Bytes are passed on a whole line at a time; a line that isn't a JSON object
/// with a `model` (`data: [DONE]`, a comment, an `event:`) goes through as it came.
#[derive(Debug)]
struct SseRelabel {
    label: String,
    pending: Vec<u8>,
}

impl SseRelabel {
    fn new(label: String) -> Self {
        Self {
            label,
            pending: Vec::new(),
        }
    }

    /// The complete lines received so far, rewritten; a partial line waits for its end.
    fn feed(&mut self, bytes: &[u8]) -> Bytes {
        self.pending.extend_from_slice(bytes);
        match self.pending.iter().rposition(|b| *b == b'\n') {
            Some(i) => {
                let rest = self.pending.split_off(i + 1);
                let lines = std::mem::replace(&mut self.pending, rest);
                Bytes::from(self.rewrite(&lines))
            }
            None => Bytes::new(),
        }
    }

    /// Whatever is left when the upstream ends.
    fn finish(&mut self) -> Bytes {
        let lines = std::mem::take(&mut self.pending);
        Bytes::from(self.rewrite(&lines))
    }

    fn rewrite(&self, text: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        for piece in text.split_inclusive(|b| *b == b'\n') {
            let body_len = piece.len()
                - piece
                    .iter()
                    .rev()
                    .take_while(|b| matches!(b, b'\n' | b'\r'))
                    .count();
            let (line, ending) = piece.split_at(body_len);
            let relabelled = line.strip_prefix(b"data:").and_then(|payload| {
                let payload = payload.strip_prefix(b" ").unwrap_or(payload);
                let mut v: Value = serde_json::from_slice(payload).ok()?;
                v.get("model").filter(|m| m.is_string())?;
                relabel(&mut v, &self.label);
                serde_json::to_vec(&v).ok()
            });
            match relabelled {
                Some(json) => {
                    out.extend_from_slice(b"data: ");
                    out.extend_from_slice(&json);
                    out.extend_from_slice(ending);
                }
                None => out.extend_from_slice(piece),
            }
        }
        out
    }
}

/// A streamed answer with each event's `model` rewritten to `label` ([`SseRelabel`]).
fn relabel_stream(
    upstream: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    label: String,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    let state = (Box::pin(upstream), SseRelabel::new(label), false);
    futures_util::stream::unfold(state, |(mut up, mut sse, done)| async move {
        if done {
            return None;
        }
        loop {
            match up.next().await {
                Some(Ok(bytes)) => {
                    let out = sse.feed(&bytes);
                    if !out.is_empty() {
                        return Some((Ok(out), (up, sse, false)));
                    }
                }
                Some(Err(e)) => return Some((Err(e), (up, sse, true))),
                None => {
                    let out = sse.finish();
                    return (!out.is_empty()).then_some((Ok(out), (up, sse, true)));
                }
            }
        }
    })
}

#[cfg(test)]
mod relabel_tests {
    use super::SseRelabel;

    #[test]
    fn rewrites_the_model_of_each_event_across_chunk_boundaries() {
        let mut sse = SseRelabel::new("hosted-model-a".into());
        let mut out = Vec::new();
        for chunk in [
            &b"data: {\"id\":\"1\",\"model\":\"real"[..],
            b"-id\",\"choices\":[]}\r\n\n: keep-alive\n",
            b"data: {\"id\":\"2\",\"model\":\"real-id\"}\n\ndata: [DONE]",
        ] {
            out.extend_from_slice(&sse.feed(chunk));
        }
        out.extend_from_slice(&sse.finish());
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("real-id"), "{text}");
        assert_eq!(
            text,
            "data: {\"choices\":[],\"id\":\"1\",\"model\":\"hosted-model-a\"}\r\n\n: keep-alive\n\
             data: {\"id\":\"2\",\"model\":\"hosted-model-a\"}\n\ndata: [DONE]"
        );
    }
}

/// What lies under a transport error ("connection reset by peer", "operation timed out"),
/// which reqwest's own message ("error sending request for url ...") leaves out. Never holds
/// a header, so never the remote key.
fn causes(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = Vec::new();
    let mut next = e.source();
    while let Some(cause) = next {
        out.push(cause.to_string());
        next = cause.source();
    }
    out.join(": ")
}

#[cfg(test)]
mod causes_tests {
    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);
    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|l| l as _)
        }
    }

    #[test]
    fn lists_every_cause_under_the_error() {
        let e = Layer(
            "error sending request",
            Some(Box::new(Layer(
                "client error (SendRequest)",
                Some(Box::new(Layer("connection reset by peer", None))),
            ))),
        );
        assert_eq!(
            super::causes(&e),
            "client error (SendRequest): connection reset by peer"
        );
        assert_eq!(super::causes(&Layer("alone", None)), "");
    }
}
