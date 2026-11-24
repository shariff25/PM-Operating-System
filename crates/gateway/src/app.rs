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
//! sent upstream. For such a role the gateway also keeps the upstream's own words from the
//! caller, which may name the model or the provider: only the fields a chat-completions client
//! reads are passed on from an answer ([`LABELLED_FIELDS`]), a streamed answer's comments and lines
//! that don't parse are dropped, and an error answer is replaced by a neutral one that keeps
//! its status, a coarse type and `Retry-After` ([`neutral_error`]). The roles answer and the
//! usage log carry the SHA-256 of its model id beside the label ([`Role::model_sha256`]).
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

use pmos_contract::model_source::{self as source, ModelSource};

use crate::anthropic;
use crate::config::{Client, ClientPolicy, GatewayClass, Hop, Models, Role, UpstreamKind};
use crate::keys::{KeyStore, Unusable};
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
    /// `PMOS_MODEL_SOURCE` (slice H4). Every answer reports it (`stub` with the stub on) in
    /// `x-pmos-model-source`, and every model call must name it in `X-Pmos-Model-Source`
    /// ([`check_expected_source`]). Under `hosted-test` every upstream error body is replaced
    /// ([`neutral_error`]). The start-up rules are `Config::check_source`'s.
    pub model_source: ModelSource,
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

/// The least time `GET /readyz` reuses a probe when a role's upstream is a testing-only remote
/// one (spec §3.6, "Timeouts and retries", r6): that probe carries the provider's key, and the
/// route needs no token, so no caller can make the gateway send keyed requests to the provider
/// more often than this.
pub const REMOTE_READY_CACHE: Duration = Duration::from_secs(30);

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
    /// The upstreams' `key-file:` keys (slice MP2).
    keys: Arc<KeyStore>,
    /// [`Timeouts::ready_cache`].
    ready_cache: Duration,
    /// `GET /readyz`'s last probe. Held across a probe, so callers that arrive while one
    /// runs wait for it and use its result rather than probing again.
    probe: tokio::sync::Mutex<Option<Probe>>,
    /// What every answer reports in `x-pmos-model-source`, and what every model call must
    /// name: `PMOS_MODEL_SOURCE`, or `stub` with the stub on.
    source: &'static str,
    /// `PMOS_MODEL_SOURCE=hosted-test`: every upstream error body is replaced (V3).
    hosted_test: bool,
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

/// What the router takes beside [`GatewayConfig`], each with a default that changes nothing.
#[derive(Debug, Default, Clone)]
pub struct RouterOptions {
    /// The upstreams' keys (`keys.rs`); `main` keeps a handle to reload them on `SIGHUP`.
    pub keys: Arc<KeyStore>,
    /// `$PMOS_STATE_DIR/route-control` (§4.11, slice MP8): `disabled-<label>` there takes that
    /// model out of every route, looked at before every call. `None`: no model is disabled.
    pub route_control: Option<std::path::PathBuf>,
}

pub fn router(cfg: GatewayConfig) -> Router {
    router_with(cfg, RouterOptions::default())
}

/// [`router`], with the options of [`RouterOptions`].
pub fn router_with(mut cfg: GatewayConfig, opts: RouterOptions) -> Router {
    // With the stub model on, the gateway is a `test` one whatever its config says, so a stub
    // run never reports `production` (`Config::served_class`).
    if cfg.stub {
        cfg.models.class = crate::config::GatewayClass::Test;
    }
    let sched = Scheduler::new(cfg.models.limits);
    // Both clients use the system proxy (`HTTPS_PROXY`, `NO_PROXY`) and trust store, so a
    // remote test upstream works from behind a proxy. Neither follows a redirect (slice H4,
    // spec §3.6, "Egress additions"): a `307` or `308` would resend the prompt to wherever it
    // points, and a same-host one would keep the key; a `3xx` is a `502` for that call
    // ([`redirect_refused`]). A `class: production` gateway resolves its two model server
    // names only to their pinned addresses, at every connection (`resolve::PinnedResolver`).
    let production = cfg.models.class == GatewayClass::Production;
    let builder = |b: reqwest::ClientBuilder| {
        let b = b.redirect(reqwest::redirect::Policy::none());
        if production {
            b.dns_resolver(Arc::new(crate::resolve::PinnedResolver::default()))
        } else {
            b
        }
    };
    let http = builder(reqwest::Client::builder())
        .connect_timeout(Duration::from_secs(5))
        // A long generation streams for minutes; a server that goes silent this long is stuck.
        .read_timeout(cfg.timeouts.stream_silence)
        .build()
        .expect("http client builds");
    let whole_http = builder(reqwest::Client::builder())
        .connect_timeout(Duration::from_secs(10))
        .timeout(cfg.timeouts.whole_answer)
        .build()
        .expect("http client builds");
    let served = crate::config::served_source(cfg.model_source, cfg.stub);
    // A remote test upstream's probe carries its key: at most one round per 30 seconds.
    let ready_cache = if cfg.models.roles.values().any(|r| r.kind.is_cloud_test()) {
        cfg.timeouts.ready_cache.max(REMOTE_READY_CACHE)
    } else {
        cfg.timeouts.ready_cache
    };
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
        keys: opts.keys,
        ready_cache,
        probe: tokio::sync::Mutex::new(None),
        source: served,
        hosted_test: cfg.model_source == ModelSource::HostedTest,
    }));
    let v1 = Router::new()
        .route("/models", get(models))
        .route("/chat/completions", post(chat))
        .route("/embeddings", post(embeddings))
        .route("/pmos/workarounds", get(list_workarounds))
        .route("/pmos/roles", get(list_roles))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));
    // Every answer, errors, refusals and streams included, says the source it was served from
    // (spec §3.6, "Keeping every caller on one source", r6, V4).
    let label = HeaderValue::from_static(served);
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .nest("/v1", v1)
        .with_state(state)
        .layer(middleware::map_response(move |mut res: Response| {
            let label = label.clone();
            async move {
                res.headers_mut().insert(source::HEADER, label);
                res
            }
        }))
}

/// X4 (r7, spec §3.6, "Callers state the source they expect"): a model call must say, in
/// `X-Pmos-Model-Source`, the source it expects; the gateway compares before it queues,
/// counts or forwards anything. Missing: `400 model_source_required`, under every source, the
/// stub included, so no caller can skip the check by leaving the header out. Another source:
/// `409 model_source_mismatch` (the answer's `x-pmos-model-source` says this gateway's).
/// `Some`: the refusal to answer with.
fn check_expected_source(headers: &HeaderMap, served: &str) -> Option<Response> {
    match headers.get(source::HEADER).map(|v| v.to_str()) {
        None => Some(error(
            StatusCode::BAD_REQUEST,
            "model_source_required",
            "say which model source this call expects in X-Pmos-Model-Source",
        )),
        Some(Ok(v)) if v == served => None,
        Some(_) => Some(error(
            StatusCode::CONFLICT,
            "model_source_mismatch",
            "this gateway serves another model source than the call expects \
             (x-pmos-model-source); nothing was sent to a model",
        )),
    }
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
                .filter(|r| r.uses_workaround(w.id))
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
            let mut v = json!({"role": r.name, "alias": r.alias(), "model": r.reported_model(),
                               "upstream": r.upstream_kind_label()});
            // A labelled role: the hash of its real model id, so a benchmark report can tell a
            // model swapped behind an unchanged label (`crates/evals`, "Which runs count").
            if let Some(h) = r.model_sha256() {
                v["model_sha256"] = json!(h);
            }
            v
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
        "model_source": state.0.source,
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
                           "model_source": state.0.source,
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
                    "model_source": state.0.source,
                    "cloud_test": cloud_test, "running": running, "waiting": waiting}),
        ),
    )
        .into_response()
}

/// Whether each upstream base answers `GET <base>/models`: the cached probe while it is
/// younger than [`Timeouts::ready_cache`], else a new one.
///
/// The probe runs in a task of its own, so a caller that hangs up mid-probe doesn't cancel
/// it: it still completes and fills the cache (the Low on `/readyz`, review at 52dbae2).
/// Before, the probe ran in the caller's request, and a client that connected and hung up
/// over and over left the cache empty and kept a round of probes in flight.
async fn probe_upstreams(inner: &Arc<Inner>) -> std::collections::HashMap<String, bool> {
    let task = tokio::spawn({
        let inner = inner.clone();
        async move { probe_now(&inner).await }
    });
    match task.await {
        Ok(up) => up,
        // The probe task failed: every upstream reads as down.
        Err(_) => upstream_bases(inner)
            .into_iter()
            .map(|b| (b, false))
            .collect(),
    }
}

/// Every upstream base the roles use, once each.
fn upstream_bases(inner: &Inner) -> Vec<String> {
    let mut upstreams: Vec<String> = inner
        .models
        .roles
        .values()
        .flat_map(Role::hops)
        .map(|h| h.upstream)
        .collect();
    upstreams.sort();
    upstreams.dedup();
    upstreams
}

/// [`probe_upstreams`]' work, in its own task.
async fn probe_now(inner: &Inner) -> std::collections::HashMap<String, bool> {
    let mut cached = inner.probe.lock().await;
    if let Some((at, result)) = cached.as_ref() {
        if at.elapsed() < inner.ready_cache {
            return result.clone();
        }
    }
    let mut up_by_base = std::collections::HashMap::new();
    for base in upstream_bases(inner) {
        let hop = inner
            .models
            .roles
            .values()
            .flat_map(Role::hops)
            .find(|h| h.upstream == base && h.kind == UpstreamKind::RemoteOpenAi);
        let remote = hop.is_some();
        let probe = match hop.map(|h| upstream_key(inner, &h)) {
            // Its key file can't be used: nothing is sent, and it reads as down.
            Some(Err(_)) => {
                up_by_base.insert(base, false);
                continue;
            }
            Some(Ok(key)) => inner
                .http
                .get(format!("{base}/models"))
                .bearer_auth(key.unwrap_or_default()),
            None => inner.http.get(format!("{base}/models")),
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

/// Reads the token counts of a streamed answer as it passes (the `usage` an event carries when
/// the caller asked for `stream_options.include_usage`), so a streamed call's usage line has
/// them as a whole answer's does. The bytes themselves are passed on untouched.
#[derive(Default)]
struct StreamUsage {
    pending: Vec<u8>,
}

impl StreamUsage {
    /// The latest `(prompt_tokens, completion_tokens)` among the whole lines in `bytes` (with
    /// what came before), when one of them carried usage.
    fn feed(&mut self, bytes: &[u8]) -> Option<(Option<u64>, Option<u64>)> {
        self.pending.extend_from_slice(bytes);
        let mut found = None;
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            if !data.windows(7).any(|w| w == b"\"usage\"") {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<Value>(data.trim_ascii()) else {
                continue;
            };
            let usage = &event["usage"];
            if usage.is_object() {
                found = Some((
                    usage["prompt_tokens"].as_u64(),
                    usage["completion_tokens"].as_u64(),
                ));
            }
        }
        found
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
        usage: StreamUsage,
        seen: usize,
        done: bool,
    }
    let flow = Flow {
        upstream,
        end,
        scanner,
        usage: StreamUsage::default(),
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
                let e = cut(
                    &mut f,
                    502,
                    format!("the streamed answer broke off ({})", error_kind(&e)),
                );
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
                if let Some((prompt, completion)) = f.usage.feed(&bytes) {
                    f.end.event.prompt_tokens = prompt;
                    f.end.event.completion_tokens = completion;
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
    // X4: before the call is queued, counted or forwarded.
    if let Some(refused) = check_expected_source(&headers, inner.source) {
        tracing::warn!(
            client = %client, role = %role.alias(), status = refused.status().as_u16(),
            "a model call didn't name this gateway's model source; refused before forwarding"
        );
        return refused;
    }
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
        model_sha256: role.model_sha256(),
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
        hop: None,
        fallback: false,
        reason: None,
        skipped: Vec::new(),
        upstream_sha256: None,
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
    // Clients that may name anyone hold at most their person pool's slots of the person's
    // together (U1): the runtime in a pool of its own, the team bots in another, so a team
    // bot's calls never hold up the runtime's, and neither holds up a PM's own bot (W1).
    let owner = Owner::new(who.user.as_deref(), &client, policy.max_in_flight)
        .with_queue(policy.max_queue)
        .with_person_share(policy.per_user_share)
        .with_pool(policy.pool);
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
        model_sha256: role.model_sha256(),
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
        hop: Some(1),
        fallback: false,
        reason: None,
        skipped: Vec::new(),
        upstream_sha256: None,
    };
    let model_header = HeaderValue::from_str(role.reported_model()).ok();
    let primary_sha = role.model_sha256();
    let finish = |mut res: Response| {
        if let Some(v) = model_header.clone() {
            res.headers_mut().insert("x-pmos-model", v);
        }
        // §4.9: every answer says which model of the route answered. A one-hop role's (and a
        // stub's) is always its primary.
        route_headers(&mut res, primary_sha.as_deref(), 1, false);
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

    if role.routed() {
        return forward_routed(
            inner,
            Routed {
                role,
                endpoint,
                req,
                stream,
                event,
                permit,
                owner,
                priority: who.priority,
                started,
                only: None,
            },
        )
        .await;
    }

    if role.kind == UpstreamKind::ClaudeTest {
        let res = claude(inner, &role, endpoint, &req, stream, &mut event).await;
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        drop(permit);
        return finish(res);
    }

    // A role whose workarounds need the whole answer (`strip-think`): a streamed call is asked
    // of the model server whole, the workarounds apply, and the caller gets it as an event
    // stream ([`anthropic::as_stream`]), so it behaves the same streamed or not. The model
    // server's hop is then as silent as a call that isn't streamed, under the same limits.
    let whole_for_stream =
        stream && workarounds::needs_whole_answer(&role.workarounds, inner.workarounds_on);
    if whole_for_stream {
        req["stream"] = json!(false);
        if let Some(o) = req.as_object_mut() {
            o.remove("stream_options");
        }
    }
    let upstream_stream = stream && !whole_for_stream;
    let url = format!("{}/{}", role.upstream, endpoint.path());
    let remote = role.kind == UpstreamKind::RemoteOpenAi;
    // The upstream's own key (slice MP2): its `key-file:`, else `PMOS_GATEWAY_REMOTE_KEY`. A
    // key file that can't be used sends nothing.
    let key = match upstream_key(inner, &role.hops()[0]) {
        Ok(k) => k.unwrap_or_default(),
        Err(why) => {
            tracing::warn!(
                role = %role.alias(), upstream = role.upstream_kind_label(), reason = why.as_str(),
                "the upstream's key file can't be used; nothing was sent"
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
        let client = if upstream_stream {
            &inner.http
        } else {
            &inner.whole_http
        };
        let post = client.post(&url);
        // The client's own gateway token never goes upstream; the remote key does.
        let post = if remote { post.bearer_auth(&key) } else { post };
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
            role = %role.alias(), upstream = role.upstream_kind_label(), status, attempt,
            wait_ms = wait.as_millis() as u64,
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
            // Out of retries: the upstream's own answer, as it would have been without them;
            // for a labelled role, a neutral one in its place ([`neutral_error`]).
            event.status = status;
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            let mut res = upstream_error_answer(&role, inner.hosted_test, status, body);
            if let Some(v) = headers.get(header::RETRY_AFTER) {
                res.headers_mut().insert(header::RETRY_AFTER, v.clone());
            }
            return finish(res);
        }
        Attempt::Down(e) => {
            // V3: the role and its kind label, and the error's kind, never its text (which
            // holds the URL, or the address a resolver answered).
            tracing::warn!(
                role = %role.alias(), upstream = role.upstream_kind_label(),
                error = error_kind(&e), timeout = e.is_timeout(),
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
    if status.is_redirection() {
        // No redirect is followed, and none is passed on (its `Location` names somewhere else).
        event.status = 502;
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        drop(permit);
        tracing::warn!(
            role = %role.alias(), upstream = role.upstream_kind_label(), status = status.as_u16(),
            "model server answered a redirect, which this gateway never follows"
        );
        return finish(redirect_refused(&role));
    }

    if upstream_stream && status.is_success() {
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
                    format!(
                        "the `{}` model server stopped mid-answer ({})",
                        role.name,
                        error_kind(&e)
                    )
                }
            };
            return finish(error(StatusCode::BAD_GATEWAY, "upstream_error", message));
        }
    };
    if !status.is_success() {
        event.latency_ms = started.elapsed().as_millis() as u64;
        inner.usage.record(event);
        // The model server's own error (bad parameters, context too long) helps the caller;
        // for a labelled role it may name the model, so a neutral one takes its place.
        return finish(upstream_error_answer(
            &role,
            inner.hosted_test,
            status.as_u16(),
            bytes,
        ));
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
    if whole_for_stream {
        return finish(
            (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                anthropic::as_stream(&answer),
            )
                .into_response(),
        );
    }
    finish(Json(answer).into_response())
}

/// The headers every answer carries about the model that answered (§4.9): its
/// `model_sha256` (labelled models only), its 1-based place in the role's route, and whether
/// it is a fallback. `x-pmos-model` is set beside them.
fn route_headers(res: &mut Response, sha: Option<&str>, hop: u32, fallback: bool) {
    let h = res.headers_mut();
    if let Some(v) = sha.and_then(|s| HeaderValue::from_str(s).ok()) {
        h.insert("x-pmos-model-sha256", v);
    }
    h.insert("x-pmos-route-hop", HeaderValue::from(hop));
    h.insert(
        "x-pmos-fallback",
        HeaderValue::from_static(if fallback { "1" } else { "0" }),
    );
}

/// A routed role's call, admitted: its slot is held (one slot covers every hop, §4.3).
struct Routed {
    role: Role,
    endpoint: Endpoint,
    req: Value,
    stream: bool,
    event: UsageEvent,
    permit: Permit,
    owner: Owner,
    priority: Priority,
    started: Instant,
    /// The one hop (index into the route) this call may use, when the client may not fall
    /// back (§4.10): `Some(0)` for primary-only, the pinned one for `pin`.
    only: Option<usize>,
}

/// What one attempt at one hop came to.
enum HopOutcome {
    /// The commit point was reached (§4.5): this hop answers, for good.
    Committed(Committed),
    /// An answer that is the caller's own (a client `4xx`), never routed around (§4.4).
    Final {
        status: u16,
        headers: HeaderMap,
        body: Bytes,
    },
    /// A failure on the upstream's side: the same hop again, or the next.
    Failed(crate::fallback::Failure),
}

enum Committed {
    /// A whole, valid answer (not streamed, or asked whole for a hop's workaround, or the
    /// Claude test upstream's translated answer).
    Whole(Value),
    /// A stream past its first content: the events held until then, and the rest.
    Stream { held: Bytes, rest: Upstream },
}

/// §4.3 and §4.4: the hops of a routed role, in order, until one commits, the list ends, or
/// `routing.budget-secs` would be passed. Each hop gets up to `routing.same-hop-attempts`; a
/// failure on the upstream's side moves to the next hop, a client's own error and a refusal are
/// answers. Nothing left: `503 no_model_available`, naming no hop and no upstream's error. The
/// caller never sees an upstream's own error text: every hop of a route is labelled, so every
/// error body is the neutral one (V3). Each attempt gets its usage line (§4.9).
async fn forward_routed(inner: &Inner, call: Routed) -> Response {
    let Routed {
        role,
        endpoint,
        req,
        stream,
        mut event,
        permit,
        owner,
        priority,
        started,
        only,
    } = call;
    let routing = inner.models.routing;
    let hops = role.hops();
    let mut skipped: Vec<crate::usage::Skipped> = Vec::new();
    let candidates: Vec<usize> = match only {
        Some(i) => vec![i],
        None => (0..hops.len()).collect(),
    };
    let mut plan = Vec::new();
    for i in candidates {
        let hop = &hops[i];
        let label = hop.reported_model().to_string();
        if !crate::fallback::serves(
            hop,
            endpoint == Endpoint::Embeddings,
            &req,
            stream,
            inner.workarounds_on,
        ) {
            skipped.push(crate::usage::Skipped {
                label,
                why: "feature",
            });
            continue;
        }
        plan.push(i);
    }
    let deadline = Instant::now() + routing.budget();
    let mut permit = Some(permit);
    let mut attempts = 0u32;
    let mut first = true;
    'hops: for &i in &plan {
        let hop = &hops[i];
        let key = match upstream_key(inner, hop) {
            Ok(k) => k,
            Err(why) => {
                tracing::warn!(
                    role = %role.alias(), model = %hop.reported_model(), reason = why.as_str(),
                    "the upstream's key file can't be used; this model is skipped"
                );
                skipped.push(crate::usage::Skipped {
                    label: hop.reported_model().to_string(),
                    why: "key",
                });
                continue;
            }
        };
        let mut same = 0u32;
        loop {
            if !first && Instant::now() >= deadline {
                break 'hops;
            }
            first = false;
            same += 1;
            attempts += 1;
            let outcome =
                attempt_hop(inner, &role, hop, endpoint, &req, stream, key.as_deref()).await;
            let mut line = hop_line(inner, &event, hop, i);
            line.attempt = attempts;
            line.attempts = attempts;
            match outcome {
                HopOutcome::Committed(c) => {
                    line.skipped = skipped;
                    let permit = permit.take().expect("a committed hop holds its slot");
                    return committed(
                        inner, hop, i, endpoint, stream, c, line, permit, started, attempts,
                    );
                }
                HopOutcome::Final {
                    status,
                    headers,
                    body,
                } => {
                    line.status = status;
                    line.skipped = skipped;
                    line.latency_ms = started.elapsed().as_millis() as u64;
                    inner.usage.record(line);
                    drop(permit.take());
                    let mut res = (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                        Json(neutral_error(status, &body)),
                    )
                        .into_response();
                    if let Some(v) = headers.get(header::RETRY_AFTER) {
                        res.headers_mut().insert(header::RETRY_AFTER, v.clone());
                    }
                    return hop_response(res, hop, i, attempts);
                }
                HopOutcome::Failed(failure) => {
                    tracing::warn!(
                        role = %role.alias(), model = %hop.reported_model(),
                        upstream = hop.upstream_kind_label(), reason = failure.reason, attempt = attempts,
                        "a model of the route failed"
                    );
                    line.status = 502;
                    line.retrying = true;
                    line.reason = Some(failure.reason);
                    line.latency_ms = started.elapsed().as_millis() as u64;
                    inner.usage.record(line);
                    let wait = match failure.same {
                        crate::fallback::Same::No => None,
                        crate::fallback::Same::After(d) => Some(d),
                        crate::fallback::Same::Backoff => Some(inner.retry.backoff(same)),
                    };
                    let Some(wait) = wait.filter(|w| {
                        same < routing.same_hop_attempts && Instant::now() + *w < deadline
                    }) else {
                        continue 'hops;
                    };
                    // The slot is given back while waiting, as a one-hop role's retry does.
                    drop(permit.take());
                    tokio::time::sleep(wait).await;
                    let waited = Instant::now();
                    match slot(inner, &owner, priority).await {
                        Ok(p) => permit = Some(p),
                        Err(why) => {
                            event.status = why.status().as_u16();
                            event.attempts = attempts;
                            event.latency_ms = started.elapsed().as_millis() as u64;
                            inner.usage.record(event);
                            return busy(why);
                        }
                    }
                    event.queue_ms += waited.elapsed().as_millis() as u64;
                }
            }
        }
    }
    drop(permit);
    // Nothing left (§4.3, step 5): no hop, no upstream's error, named.
    event.status = 503;
    event.attempt = attempts;
    event.attempts = attempts;
    event.skipped = skipped;
    event.latency_ms = started.elapsed().as_millis() as u64;
    inner.usage.record(event);
    no_model_available(1)
}

/// `503 no_model_available` (§4.3, step 5), with `Retry-After` (at least a second).
fn no_model_available(retry_after_secs: u64) -> Response {
    let mut res = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": {
            "type": "no_model_available",
            "status": 503,
            "message": "no model of this role is available now; try again shortly",
        }})),
    )
        .into_response();
    res.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(retry_after_secs.max(1)),
    );
    res
}

/// The usage line for an attempt at `hop` (index `i` of the route): the call's line with the
/// hop's label, hash, kind, place and fallback flag (§4.9).
fn hop_line(inner: &Inner, event: &UsageEvent, hop: &Hop, i: usize) -> UsageEvent {
    let mut line = event.clone();
    line.model = hop.reported_model().to_string();
    line.model_sha256 = hop.model_sha256();
    line.upstream = hop.logged_upstream_host();
    line.hop = Some(i as u32 + 1);
    line.fallback = i > 0;
    line.upstream_sha256 = inner.hosted_test.then(|| hop.upstream_sha256());
    line.status = 200;
    line.retrying = false;
    line.reason = None;
    line
}

/// An answer from `hop` (index `i`), with the headers that say which model answered (§4.9).
fn hop_response(mut res: Response, hop: &Hop, i: usize, attempts: u32) -> Response {
    if let Ok(v) = HeaderValue::from_str(hop.reported_model()) {
        res.headers_mut().insert("x-pmos-model", v);
    }
    route_headers(&mut res, hop.model_sha256().as_deref(), i as u32 + 1, i > 0);
    res.headers_mut()
        .insert("x-pmos-upstream-attempts", HeaderValue::from(attempts));
    res
}

/// The answer once `hop` has committed: a whole answer with the hop's workarounds and label
/// applied, or its stream (held events first) under the gateway's usual limits.
#[allow(clippy::too_many_arguments)]
fn committed(
    inner: &Inner,
    hop: &Hop,
    i: usize,
    endpoint: Endpoint,
    stream: bool,
    c: Committed,
    mut event: UsageEvent,
    permit: Permit,
    started: Instant,
    attempts: u32,
) -> Response {
    let label = hop.reported_model().to_string();
    let remote = hop.kind == UpstreamKind::RemoteOpenAi;
    match c {
        Committed::Stream { held, rest } => {
            let end = StreamEnd {
                usage: inner.usage.clone(),
                event,
                started,
                _permit: permit,
            };
            let scanner = remote.then(|| StreamScanner::new(inner.extras.clone(), &hop.upstream));
            let limits = inner.models.limits;
            let upstream: Upstream =
                Box::pin(futures_util::stream::once(async move { Ok(held) }).chain(rest));
            let body = bounded_stream(
                upstream,
                end,
                scanner,
                tokio::time::Instant::now() + Duration::from_secs(limits.stream_max_secs),
                limits.max_answer_bytes,
            );
            let res = (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                Body::from_stream(relabel_stream(body, label)),
            )
                .into_response();
            hop_response(res, hop, i, attempts)
        }
        Committed::Whole(mut answer) => {
            if remote && endpoint == Endpoint::Chat {
                inner.extras.remember_answer(&hop.upstream, &answer);
            }
            if endpoint == Endpoint::Chat && hop.kind != UpstreamKind::ClaudeTest {
                event.workarounds =
                    workarounds::apply(&hop.workarounds, inner.workarounds_on, &mut answer);
            }
            relabel(&mut answer, &label);
            event.prompt_tokens = answer["usage"]["prompt_tokens"].as_u64();
            event.completion_tokens = answer["usage"]["completion_tokens"].as_u64();
            event.latency_ms = started.elapsed().as_millis() as u64;
            inner.usage.record(event);
            drop(permit);
            let res = if stream {
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
            };
            hop_response(res, hop, i, attempts)
        }
    }
}

/// One attempt at one hop (§4.4, §4.5): the request with this hop's model id (and, for a
/// remote one, the tool-call `extra_content` it gave, never another upstream's), sent with
/// this hop's key only, and what came back, classified.
async fn attempt_hop(
    inner: &Inner,
    role: &Role,
    hop: &Hop,
    endpoint: Endpoint,
    req: &Value,
    stream: bool,
    key: Option<&str>,
) -> HopOutcome {
    use crate::fallback::{Failure, Hit, Same, Scope};
    let fail =
        |reason, same, hit, scope| HopOutcome::Failed(Failure::new(reason, same, hit, scope));
    let max = inner.models.limits.max_answer_bytes;
    let mut req = req.clone();
    req["model"] = json!(hop.model);
    if hop.kind == UpstreamKind::ClaudeTest {
        return attempt_claude(inner, hop, &req, key.unwrap_or_default()).await;
    }
    let whole_for_stream =
        stream && workarounds::needs_whole_answer(&hop.workarounds, inner.workarounds_on);
    if whole_for_stream {
        req["stream"] = json!(false);
        if let Some(o) = req.as_object_mut() {
            o.remove("stream_options");
        }
    }
    let upstream_stream = stream && !whole_for_stream;
    if hop.kind == UpstreamKind::RemoteOpenAi {
        inner.extras.restore(&hop.upstream, &mut req);
    }
    let client = if upstream_stream {
        &inner.http
    } else {
        &inner.whole_http
    };
    let post = client.post(format!("{}/{}", hop.upstream, endpoint.path()));
    let post = match key {
        Some(k) => post.bearer_auth(k),
        None => post,
    };
    let res = match post.json(&req).send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return fail("timeout", Same::No, Hit::Count, Scope::Upstream),
        Err(e) if e.is_connect() => {
            return fail("connect", Same::Backoff, Hit::Count, Scope::Upstream)
        }
        Err(_) => return fail("connect", Same::No, Hit::Count, Scope::Upstream),
    };
    let status = res.status().as_u16();
    if !res.status().is_success() {
        let headers = res.headers().clone();
        let body = read_capped(res, max.min(ERROR_BODY_MAX))
            .await
            .unwrap_or_default();
        return match crate::fallback::classify_status(
            status,
            &headers,
            &body,
            inner.models.routing.short_retry_after(),
        ) {
            crate::fallback::Verdict::Answer => HopOutcome::Final {
                status,
                headers,
                body,
            },
            crate::fallback::Verdict::Fail(failure) => {
                if failure.reason == "auth" {
                    tracing::warn!(
                        role = %role.alias(), model = %hop.reported_model(),
                        "upstream refused the key"
                    );
                }
                HopOutcome::Failed(failure)
            }
        };
    }
    if upstream_stream {
        return match crate::fallback::first_content(
            Box::pin(res.bytes_stream()),
            inner.models.routing.first_token(),
        )
        .await
        {
            Ok((held, rest)) => HopOutcome::Committed(Committed::Stream { held, rest }),
            Err(_) => fail("first_token", Same::No, Hit::Count, Scope::Model),
        };
    }
    let Ok(bytes) = read_capped(res, max).await else {
        return fail("bad_answer", Same::No, Hit::Count, Scope::Model);
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v @ Value::Object(_)) => HopOutcome::Committed(Committed::Whole(v)),
        _ => fail("bad_answer", Same::No, Hit::Count, Scope::Model),
    }
}

/// The largest upstream error body read (to classify it; it never reaches a caller).
const ERROR_BODY_MAX: usize = 1024 * 1024;

/// One attempt at a Claude test upstream hop: the Messages API's whole answer, translated.
async fn attempt_claude(inner: &Inner, hop: &Hop, req: &Value, key: &str) -> HopOutcome {
    use crate::fallback::{Failure, Hit, Same, Scope};
    let fail =
        |reason, same, hit, scope| HopOutcome::Failed(Failure::new(reason, same, hit, scope));
    let body = match anthropic::to_messages(req, &hop.model, hop.effort.as_deref()) {
        Ok(b) => b,
        Err(m) => {
            return HopOutcome::Final {
                status: 400,
                headers: HeaderMap::new(),
                body: Bytes::from(m),
            }
        }
    };
    let res = match inner
        .whole_http
        .post(format!("{}/v1/messages", hop.upstream))
        .header("x-api-key", key)
        .header("anthropic-version", anthropic::API_VERSION)
        .header("anthropic-beta", anthropic::FALLBACK_BETA)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return fail("timeout", Same::No, Hit::Count, Scope::Upstream),
        Err(e) if e.is_connect() => {
            return fail("connect", Same::Backoff, Hit::Count, Scope::Upstream)
        }
        Err(_) => return fail("connect", Same::No, Hit::Count, Scope::Upstream),
    };
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let Ok(bytes) = read_capped(res, inner.models.limits.max_answer_bytes).await else {
        return fail("bad_answer", Same::No, Hit::Count, Scope::Model);
    };
    if !(200..300).contains(&status) {
        return match crate::fallback::classify_status(
            status,
            &headers,
            &bytes,
            inner.models.routing.short_retry_after(),
        ) {
            crate::fallback::Verdict::Answer => HopOutcome::Final {
                status,
                headers,
                body: bytes,
            },
            crate::fallback::Verdict::Fail(failure) => HopOutcome::Failed(failure),
        };
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v @ Value::Object(_)) => HopOutcome::Committed(Committed::Whole(
            anthropic::from_messages(&v, hop.reported_model()),
        )),
        _ => fail("bad_answer", Same::No, Hit::Count, Scope::Model),
    }
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
    let key = match upstream_key(inner, &role.hops()[0]) {
        Ok(k) => k.unwrap_or_default(),
        Err(why) => {
            tracing::warn!(
                role = %role.alias(), upstream = role.upstream_kind_label(), reason = why.as_str(),
                "the upstream's key file can't be used; nothing was sent"
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
    let res = inner
        .whole_http
        .post(format!("{}/v1/messages", role.upstream))
        .header("x-api-key", &key)
        .header("anthropic-version", anthropic::API_VERSION)
        .header("anthropic-beta", anthropic::FALLBACK_BETA)
        .json(&body)
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                role = %role.alias(), upstream = role.upstream_kind_label(),
                error = error_kind(&e), "Claude test upstream unreachable"
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
    if status.is_redirection() {
        event.status = 502;
        return redirect_refused(role);
    }
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
        if role.label.is_some() || inner.hosted_test {
            let raw = serde_json::to_vec(&answer).unwrap_or_default();
            return upstream_error_answer(role, inner.hosted_test, status.as_u16(), raw.into());
        }
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

/// The key a hop's upstream is sent (slice MP2, §4.11): the one in its `key-file:`, else the
/// environment's key for its kind (`PMOS_GATEWAY_REMOTE_KEY`, `PMOS_GATEWAY_ANTHROPIC_KEY`), or
/// none for a local model server. `Err`: its key file can't be used, so nothing may be sent.
fn upstream_key(inner: &Inner, hop: &Hop) -> Result<Option<String>, Unusable> {
    if let Some(path) = &hop.key_file {
        return inner.keys.get(path).map(Some);
    }
    Ok(match hop.kind {
        UpstreamKind::RemoteOpenAi => Some(inner.remote_key.clone()),
        UpstreamKind::ClaudeTest => Some(inner.anthropic_key.clone()),
        UpstreamKind::OpenAi => None,
    })
}

/// A transport error as the logs and callers see it (V3, slice H4): its kind only, never its
/// text, which holds the URL (and so the provider's host) or a resolver's answer.
pub fn error_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_redirect() {
        "redirect"
    } else if e.is_body() {
        "body"
    } else if e.is_decode() {
        "decode"
    } else if e.is_request() {
        "request"
    } else {
        "transport"
    }
}

/// The answer for an upstream's `3xx`: a `502`, with neither the upstream's body nor its
/// `Location` (spec §3.6, "Egress additions").
fn redirect_refused(role: &Role) -> Response {
    error(
        StatusCode::BAD_GATEWAY,
        "upstream_error",
        format!(
            "the `{}` model server answered a redirect, which this gateway never follows",
            role.name
        ),
    )
}

/// The top-level fields of an answer (whole, or one streamed event) that a labelled role
/// passes on: what a chat-completions client reads. Anything else the upstream adds, such as
/// `system_fingerprint` or `provider`, can name the model or the provider, and is dropped.
pub const LABELLED_FIELDS: &[&str] = &[
    "id", "object", "created", "model", "choices", "usage", "data",
];

/// For a labelled role: puts `label` in an answer's `model` field, where the upstream wrote
/// its own model id, keeps only [`LABELLED_FIELDS`], and replaces an `error` the upstream put
/// in a streamed event with a neutral one.
fn relabel(answer: &mut Value, label: &str) {
    let Some(obj) = answer.as_object_mut() else {
        return;
    };
    let had_error = obj.contains_key("error");
    obj.retain(|k, _| LABELLED_FIELDS.contains(&k.as_str()));
    if let Some(model) = obj.get_mut("model").filter(|m| m.is_string()) {
        *model = json!(label);
    }
    if had_error {
        obj.insert(
            "error".into(),
            json!({"type": "upstream_error",
                   "message": "the model server reported an error mid-answer"}),
        );
    }
}

/// The type a neutral error ([`neutral_error`]) gives a status: coarse enough to name
/// nothing, fine enough for a caller to act on (wait and retry, or not).
pub fn coarse_error_type(status: u16, body: &[u8]) -> &'static str {
    match status {
        // A quota that won't come back soon is worth telling apart: retrying won't help.
        429 if crate::retry::unrecoverable(body) => "quota_exhausted",
        429 => "rate_limit",
        503 | 529 => "overloaded",
        401 | 403 => "auth",
        400 | 404 | 405 | 409 | 413 | 415 | 422 => "bad_request",
        _ => "upstream_error",
    }
}

/// A neutral error body for a labelled role's non-2xx answer (the spec's point V3): its
/// status and a coarse type ([`coarse_error_type`]), and a fixed message, never the upstream's
/// own words, which may name the model or the provider.
pub fn neutral_error(status: u16, body: &[u8]) -> Value {
    let kind = coarse_error_type(status, body);
    json!({"error": {
        "type": kind,
        "status": status,
        "message": format!("the model server answered {status} ({kind})"),
    }})
}

/// The answer a caller gets for an upstream's non-2xx answer: as it came for a role without
/// a label under `local` (PM OS's own vLLM, whose messages, such as "context too long", help
/// the caller); for a labelled role, and for every role under `hosted-test` (V3, slice H4),
/// [`neutral_error`] with the same status, so a provider's `401` body (a masked key, an
/// account id, the vendor's name) never reaches a worker, a trace or a draft. The caller adds
/// any `Retry-After`.
fn upstream_error_answer(role: &Role, hosted_test: bool, status: u16, body: Bytes) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if role.label.is_some() || hosted_test {
        (code, Json(neutral_error(status, &body))).into_response()
    } else {
        (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
    }
}

/// Rewrites each line of a streamed answer for a labelled role. Bytes are passed on a whole
/// line at a time: a `data:` JSON object is [`relabel`]led, `data: [DONE]`, blank lines and
/// `event:`, `id:` and `retry:` lines go through as they came, a comment's text is dropped
/// (a bare `:` keeps it as a keep-alive), and anything else (a `data:` line that doesn't
/// parse, a line of no known kind) is dropped, since it may carry the upstream's own words.
/// So is an unterminated last line that doesn't parse ([`SseRelabel::finish`]).
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
            if line.is_empty()
                || [&b"event:"[..], b"id:", b"retry:"]
                    .iter()
                    .any(|p| line.starts_with(p))
            {
                out.extend_from_slice(piece);
                continue;
            }
            if line.starts_with(b":") {
                // A comment: kept as a keep-alive, without its text.
                out.extend_from_slice(b":");
                out.extend_from_slice(ending);
                continue;
            }
            let Some(payload) = line.strip_prefix(b"data:") else {
                continue;
            };
            let payload = payload.strip_prefix(b" ").unwrap_or(payload);
            if payload == b"[DONE]" {
                out.extend_from_slice(piece);
                continue;
            }
            let relabelled = serde_json::from_slice::<Value>(payload)
                .ok()
                .filter(Value::is_object)
                .and_then(|mut v| {
                    relabel(&mut v, &self.label);
                    serde_json::to_vec(&v).ok()
                });
            if let Some(json) = relabelled {
                out.extend_from_slice(b"data: ");
                out.extend_from_slice(&json);
                out.extend_from_slice(ending);
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
        // The comment's text is dropped (a bare `:` keeps it as a keep-alive): deliberately
        // changed from passing it as it came, since a provider can write its name there.
        assert_eq!(
            text,
            "data: {\"choices\":[],\"id\":\"1\",\"model\":\"hosted-model-a\"}\r\n\n:\n\
             data: {\"id\":\"2\",\"model\":\"hosted-model-a\"}\n\ndata: [DONE]"
        );
    }

    #[test]
    fn drops_pass_through_fields_unparsed_lines_and_a_broken_last_line() {
        let mut sse = SseRelabel::new("hosted-model-a".into());
        let mut out = Vec::new();
        for chunk in [
            &b"data: {\"id\":\"1\",\"model\":\"real-id\",\"system_fingerprint\":\"fp-real-id\",\
               \"provider\":\"ProviderCo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n"[..],
            b": served by ProviderCo real-id\nevent: message\nid: 7\nretry: 100\n",
            b"data: not json real-id\nsomething: ProviderCo\n",
            b"data: {\"error\":{\"message\":\"real-id at ProviderCo failed\"}}\n\n",
            b"data: {\"id\":\"2\",\"model\":\"real-id\",\"provider\":\"Prov",
        ] {
            out.extend_from_slice(&sse.feed(chunk));
        }
        out.extend_from_slice(&sse.finish());
        let text = String::from_utf8(out).unwrap();
        for f in [
            "real-id",
            "ProviderCo",
            "fp-",
            "system_fingerprint",
            "provider",
        ] {
            assert!(!text.contains(f), "`{f}` passed: {text}");
        }
        assert_eq!(
            text,
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"index\":0}],\"id\":\"1\",\
             \"model\":\"hosted-model-a\"}\n\n:\nevent: message\nid: 7\nretry: 100\n\
             data: {\"error\":{\"message\":\"the model server reported an error mid-answer\",\
             \"type\":\"upstream_error\"}}\n\n"
        );
    }

    #[test]
    fn a_whole_answer_keeps_only_what_a_client_reads() {
        let mut v = serde_json::json!({"id": "x", "object": "chat.completion", "created": 1,
            "model": "real-id", "system_fingerprint": "fp", "provider": "ProviderCo",
            "choices": [], "usage": {"prompt_tokens": 1}});
        super::relabel(&mut v, "hosted-model-a");
        assert_eq!(
            v,
            serde_json::json!({"id": "x", "object": "chat.completion", "created": 1,
                "model": "hosted-model-a", "choices": [], "usage": {"prompt_tokens": 1}})
        );
    }

    #[test]
    fn neutral_errors_keep_the_status_and_a_coarse_type() {
        use super::{coarse_error_type, neutral_error};
        for (status, body, want) in [
            (429, &b"{}"[..], "rate_limit"),
            (
                429,
                b"{\"error\":{\"code\":\"insufficient_quota\"}}",
                "quota_exhausted",
            ),
            (503, b"", "overloaded"),
            (529, b"", "overloaded"),
            (401, b"", "auth"),
            (403, b"", "auth"),
            (400, b"context too long for real-id", "bad_request"),
            (422, b"", "bad_request"),
            (500, b"", "upstream_error"),
            (502, b"", "upstream_error"),
        ] {
            assert_eq!(coarse_error_type(status, body), want, "{status}");
            let v = neutral_error(status, body);
            assert_eq!(v["error"]["type"], want);
            assert_eq!(v["error"]["status"], status);
            assert!(!v.to_string().contains("real-id"));
        }
    }
}
