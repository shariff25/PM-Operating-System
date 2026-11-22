//! The gateway's HTTP API: the OpenAI shapes the harness already speaks, in front of the
//! local model servers.
//!
//! - `POST /v1/chat/completions` (streaming or not) and `POST /v1/embeddings`: `model` must be
//!   a role alias, `pmos/<role>`; the gateway swaps in the role's model and forwards to its
//!   local server. A concrete model name is refused, so callers never depend on one.
//! - `GET /v1/models`: the role aliases.
//! - `GET /v1/pmos/workarounds`: the workaround registry and which roles use what (D11).
//! - `GET /healthz` (process up), `GET /readyz` (every model server answers; `cloud_test` says
//!   whether a testing-only upstream, whose prompts leave this machine, is on).
//!
//! - `GET /v1/pmos/roles`: each role's model id and upstream kind (never an address), and
//!   whether the stub or a testing-only upstream is on, for the benchmark runner's report.
//!
//! Every `/v1` call needs a client token. `X-Pmos-User` (a handle), `X-Pmos-Priority`
//! (`interactive`, `scheduled` or `personal`) and `X-Pmos-Run` place it in the queue and the
//! usage log. The first two are believed only from a client `clients:` marks `trusted`; any
//! other client's calls are keyed by its name and get its configured priority. Every client
//! has its own cap on calls in flight (`config::ClientPolicy`).

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
use crate::scheduler::{Owner, Permit, Priority, Scheduler};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub stream_silence: Duration,
    pub whole_answer: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            stream_silence: Duration::from_secs(120),
            whole_answer: Duration::from_secs(600),
        }
    }
}

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

pub fn router(cfg: GatewayConfig) -> Router {
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
    let roles: Vec<Value> = state
        .0
        .models
        .roles
        .values()
        .map(|r| {
            let upstream = match r.kind {
                UpstreamKind::OpenAi => "local",
                UpstreamKind::ClaudeTest => "claude-test",
                UpstreamKind::RemoteOpenAi => "remote-test",
            };
            json!({"role": r.name, "alias": r.alias(), "model": r.model, "upstream": upstream})
        })
        .collect();
    let cloud_test = state
        .0
        .models
        .roles
        .values()
        .any(|r| r.kind.is_cloud_test());
    Json(json!({
        "stub": state.0.stub,
        "cloud_test": cloud_test,
        "workarounds": state.0.workarounds_on,
        "roles": roles,
    }))
    .into_response()
}

async fn readyz(State(state): State<AppState>) -> Response {
    let (running, waiting) = state.0.sched.snapshot();
    if state.0.stub {
        return Json(json!({"ready": true, "stub": true, "running": running, "waiting": waiting}))
            .into_response();
    }
    let mut upstreams: Vec<String> = state
        .0
        .models
        .roles
        .values()
        .map(|r| r.upstream.clone())
        .collect();
    upstreams.sort();
    upstreams.dedup();
    let cloud_test = state
        .0
        .models
        .roles
        .values()
        .any(|r| r.kind.is_cloud_test());
    let mut checks = serde_json::Map::new();
    let mut ok = true;
    for base in upstreams {
        let remote = state
            .0
            .models
            .roles
            .values()
            .any(|r| r.upstream == base && r.kind == UpstreamKind::RemoteOpenAi);
        let probe = if remote {
            state
                .0
                .http
                .get(format!("{base}/models"))
                .bearer_auth(&state.0.remote_key)
        } else {
            state.0.http.get(format!("{base}/models"))
        };
        let up = tokio::time::timeout(
            Duration::from_secs(if remote { 5 } else { 2 }),
            probe.send(),
        )
        .await
        .is_ok_and(|r| r.is_ok_and(|r| r.status().is_success()));
        ok &= up;
        checks.insert(base, json!(if up { "ok" } else { "down" }));
    }
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(
            json!({"ready": ok, "upstreams": checks, "cloud_test": cloud_test,
                    "running": running, "waiting": waiting}),
        ),
    )
        .into_response()
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

/// The caller's headers, checked, and then believed only as far as its client is trusted:
/// a client `clients:` doesn't mark `trusted` is keyed by its own name and gets its
/// configured priority, whatever `X-Pmos-User` and `X-Pmos-Priority` say (review finding
/// N3). Malformed headers are refused from every client.
fn caller(headers: &HeaderMap, policy: &ClientPolicy) -> Result<Caller, String> {
    let declared = declared(headers)?;
    Ok(if policy.trusted {
        Caller {
            user: declared.user,
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

/// Waits for a slot for at most `limits.queue-wait-secs`. `None`: the queue was full, or the
/// wait ran out; either way the place in the queue has been given back.
async fn slot(inner: &Inner, owner: &Owner, priority: Priority) -> Result<Permit, &'static str> {
    let wait = Duration::from_secs(inner.models.limits.queue_wait_secs);
    match tokio::time::timeout(wait, inner.sched.acquire(owner, priority)).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(_)) => Err("the model queue is full; try again shortly"),
        Err(_) => Err("no model slot came free in time; try again shortly"),
    }
}

fn busy(message: &str) -> Response {
    let mut res = error(StatusCode::SERVICE_UNAVAILABLE, "server_busy", message);
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
    let who = match caller(&headers, &policy) {
        Ok(c) => c,
        Err(m) => return error(StatusCode::BAD_REQUEST, "invalid_request_error", m),
    };
    req["model"] = json!(role.model);
    let stream = endpoint == Endpoint::Chat && req["stream"] == json!(true);

    // The person's cap applies when the gateway believes the person (a trusted client's
    // `X-Pmos-User`); the client's own cap always applies. Without a believed person, the
    // client's cap alone bounds its calls.
    let owner = Owner::new(who.user.as_deref(), &client, policy.max_in_flight);
    let permit = match slot(inner, &owner, who.priority).await {
        Ok(p) => p,
        Err(m) => return busy(m),
    };
    let mut event = UsageEvent {
        at: Utc::now(),
        client,
        user: who.user,
        run: who.run,
        endpoint: endpoint.path(),
        role: role.name.clone(),
        model: role.model.clone(),
        upstream: role.upstream_host(),
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
    };
    let model_header = HeaderValue::from_str(&role.model).ok();
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
                stub::chat_stream(&req, &role.model),
            )
                .into_response(),
            (Endpoint::Chat, false) => Json(stub::chat(&req, &role.model)).into_response(),
            (Endpoint::Embeddings, _) => Json(stub::embeddings(&req, &role.model)).into_response(),
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
            upstream = %role.upstream, status, attempt, wait_ms = wait.as_millis() as u64,
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
                upstream = %role.upstream,
                error = %e,
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
        return finish(
            (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                Body::from_stream(body),
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
            tracing::warn!(upstream = %role.upstream, error = %e, "Claude test upstream unreachable");
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
    let answer = anthropic::from_messages(&answer, &role.model);
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
