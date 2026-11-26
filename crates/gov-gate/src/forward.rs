//! The MCP front door in observe mode (spec §5.2, §5.3): the gate stands between an agent's
//! MCP client and one upstream MCP server, forwards every message, and writes a receipt
//! before and after each tool call.
//!
//! In observe mode the agent keeps its own credential, so that credential passes through
//! this code in the request's headers. It is forwarded and nothing else: never logged, never
//! stored, never put in a receipt. Nothing is blocked. A stock client's identity is only what
//! it says in the `x-gov-agent` header, and the receipt records it as claimed.
//!
//! What the gate reads is what the server gets (review finding GR-62): a request body is
//! parsed strictly, and what is forwarded is that parse written out again, never the bytes
//! that arrived. Receipts are stamped from this machine's clock; that changes to the lease's
//! time when there is an Authority (G4).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use gov_core::action::Salt;
use gov_core::connector::{tool_digest, ServerMap};
use gov_core::receipt::{Coverage, OutcomeStatus, ProofFailure};
use gov_core::{Digest, Envelope, Id};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Semaphore;

use crate::sessions::{OpenRefused, Proved, Sessions, SessionsConfig};
use crate::{
    Gate, GateError, LedgerLink, Observed, Shipped, Spool, Trimmed, Who, WitnessRead, WitnessReader,
};

/// The largest request, or answer the gate must read, that it will hold.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The most tool calls recorded at once.
pub const MAX_IN_FLIGHT: usize = 256;
/// The most sessions whose tool listings are remembered. Past it the oldest is forgotten: a
/// forgotten listing only makes a tool `unknown` until it is listed again.
pub const MAX_SESSIONS: usize = 4096;

/// For each MCP session, the digest of each tool's definition as the server last listed it
/// there, and the order the sessions were first seen in.
#[derive(Default)]
pub struct Listings {
    sessions: BTreeMap<String, (u64, BTreeMap<String, Digest>)>,
    /// The session ids the server itself has issued, and when. A listing is remembered only
    /// for one of these, or for no session at all: a client cannot invent the id that pushes
    /// another session's listing out (review finding GR-76).
    issued: BTreeMap<String, u64>,
    next: u64,
}

impl Listings {
    /// The server named a session in an answer.
    pub fn issue(&mut self, session: &str) {
        if self.issued.contains_key(session) {
            return;
        }
        if self.issued.len() >= MAX_SESSIONS {
            let oldest = self
                .issued
                .iter()
                .min_by_key(|(_, seen)| **seen)
                .map(|(name, _)| name.clone());
            if let Some(oldest) = oldest {
                self.issued.remove(&oldest);
                self.sessions.remove(&oldest);
            }
        }
        self.next += 1;
        self.issued.insert(session.to_string(), self.next);
    }

    /// Whether a listing made under `session` is kept: the server issued that id, or the
    /// client named none.
    fn known(&self, session: &str) -> bool {
        session.is_empty() || self.issued.contains_key(session)
    }

    /// The listing for `session`. When the table is full the session seen longest ago is
    /// forgotten, and only that one: a client that makes up session ids cannot make every
    /// other session's tools `unknown` at a stroke (review finding GR-76).
    fn entry(&mut self, session: String) -> &mut BTreeMap<String, Digest> {
        if self.sessions.len() >= MAX_SESSIONS && !self.sessions.contains_key(&session) {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, (seen, _))| *seen)
                .map(|(name, _)| name.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }
        self.next += 1;
        let stamp = self.next;
        &mut self
            .sessions
            .entry(session)
            .or_insert((stamp, BTreeMap::new()))
            .1
    }

    /// The digest of `tool`'s definition as the server last listed it in `session`.
    pub fn digest(&self, session: &str, tool: &str) -> Option<Digest> {
        self.sessions
            .get(session)
            .and_then(|(_, tools)| tools.get(tool))
            .cloned()
    }

    /// Take in the server's answer to a `tools/list` request. The gate and the shim both
    /// remember listings through this one function, so that for the same messages they
    /// hold the same digests (spec §5.3.1).
    pub fn remember(
        &mut self,
        session: &str,
        request: &Value,
        succeeded: bool,
        event_stream: bool,
        body: &[u8],
    ) {
        // A session id the server never issued: nothing is remembered for it.
        if !self.known(session) {
            return;
        }
        let continued = request
            .get("params")
            .and_then(|p| p.get("cursor"))
            .is_some_and(|c| !c.is_null());
        let mut seen: BTreeMap<String, Option<Digest>> = BTreeMap::new();
        if succeeded {
            for tool in replies(event_stream, body)
                .iter()
                .filter_map(|r| r.get("result")?.get("tools")?.as_array())
                .flatten()
            {
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    // A definition with no digest is not a tool that was reviewed: it is
                    // forgotten, never left as it was (review finding GR-64).
                    seen.insert(name.to_string(), tool_digest(tool).ok());
                }
            }
        }
        let entry = self.entry(session.to_string());
        // A first page replaces what was remembered, so a tool the server has dropped is
        // forgotten; a later page adds to it.
        if !continued {
            entry.clear();
        }
        for (name, digest) in seen {
            match digest {
                Some(d) => entry.insert(name, d),
                None => entry.remove(&name),
            };
        }
    }
}

/// Whether an answer's headers say it is an event stream.
pub fn is_event_stream(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|t| t.starts_with("text/event-stream"))
}

/// The JSON-RPC answers in a body: one JSON object, or the `data:` lines of an event stream.
pub fn replies(event_stream: bool, body: &[u8]) -> Vec<Value> {
    if event_stream {
        String::from_utf8_lossy(body)
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect()
    } else {
        serde_json::from_slice(body).ok().into_iter().collect()
    }
}

/// What an operator must be able to see (spec §3.4: observe mode protects nothing, and what
/// it failed to record must not be silent). `not_recorded` is also kept in the spool's state,
/// where it outlives a restart.
#[derive(Debug, Default)]
pub struct Counters {
    /// Tool calls forwarded with a receipt before and after.
    pub recorded: AtomicU64,
    /// Tool calls forwarded with no receipt, because the spool was full or failed.
    pub not_recorded: AtomicU64,
    /// Tool calls the connector map could not classify.
    pub unmapped: AtomicU64,
    /// Rounds of shipping in which the Ledger took nothing.
    pub ship_no_progress: AtomicU64,
    /// Tool calls whose agent proved its key (through the shim).
    pub proven: AtomicU64,
    /// Tool calls that presented a session and did not prove the key. Kept apart from calls
    /// that offered no proof at all (review finding GR-91).
    pub proof_failed: AtomicU64,
}

pub struct ForwarderConfig {
    /// The one upstream MCP endpoint. Every request goes there and nowhere else.
    pub upstream: String,
    pub map: ServerMap,
    /// How long the server has to answer a request before the gate gives up on it.
    pub call_timeout: Duration,
    /// Sessions with the shim. Without it the gate opens none, and every agent is recorded
    /// as claimed.
    pub sessions: Option<SessionsConfig>,
    /// The most tool calls recorded at once ([`MAX_IN_FLIGHT`] unless a deployment says
    /// otherwise).
    pub max_in_flight: usize,
}

pub struct Forwarder<S> {
    gate: Arc<AsyncMutex<Gate<S>>>,
    config: ForwarderConfig,
    http: reqwest::Client,
    /// For each MCP session, the digest of each tool's definition as the server last listed
    /// it there. One session's listing never stands in for another's.
    listed: Mutex<Listings>,
    /// One round of shipping at a time (review finding GR-79).
    shipping: AsyncMutex<()>,
    sessions: Option<Mutex<Sessions>>,
    /// Calls in flight that are being recorded. Each holds its answer in memory until the
    /// server is done, whether or not its client stays (review finding GR-99).
    in_flight: Arc<Semaphore>,
    clock: fn() -> i64,
    pub counters: Counters,
}

fn wall_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// What the server answered, as much of it as the gate holds.
struct Answer {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Bytes,
}

impl<S: Spool + 'static> Forwarder<S> {
    pub fn new(gate: Gate<S>, mut config: ForwarderConfig) -> Result<Arc<Self>, GateError> {
        config.map.validate()?;
        let sessions = config.sessions.take().map(|c| Mutex::new(Sessions::new(c)));
        let in_flight = Arc::new(Semaphore::new(config.max_in_flight));
        Ok(Arc::new(Forwarder {
            gate: Arc::new(AsyncMutex::new(gate)),
            config,
            // The upstream is one fixed address: no redirect is followed.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| GateError::Link(e.to_string()))?,
            listed: Mutex::new(Listings::default()),
            shipping: AsyncMutex::new(()),
            sessions,
            in_flight,
            clock: wall_clock,
            counters: Counters::default(),
        }))
    }

    /// The gate itself, for trimming, which the gate's owner drives. While this is held,
    /// calls wait.
    pub async fn gate(&self) -> tokio::sync::MutexGuard<'_, Gate<S>> {
        self.gate.lock().await
    }

    /// Do something with the gate on a thread that may block. Every receipt is a file
    /// written and synced; that work does not belong on the threads that serve requests
    /// (review finding GR-82).
    async fn with_gate<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Gate<S>) -> R + Send + 'static,
    ) -> R {
        let gate = self.gate.clone();
        tokio::task::spawn_blocking(move || work(&mut gate.blocking_lock()))
            .await
            .expect("the gate's work does not panic")
    }

    /// Read the witness and trim the spool to what it covers. The witness is read before
    /// the gate is taken, on a thread that may block, so no call waits for it.
    pub async fn trim(&self, witness: Arc<dyn WitnessReader>) -> Result<Trimmed, GateError> {
        let read = tokio::task::spawn_blocking(move || WitnessRead::from(&*witness))
            .await
            .expect("reading the witness does not panic")
            .map_err(GateError::Witness)?;
        self.with_gate(move |gate| gate.trim_on(&read)).await
    }

    /// One round of shipping to the Ledger. The gate is held only to collect the batch and
    /// to apply the answer, never while the Ledger is being waited for, so a slow Ledger does
    /// not hold up a single call (review finding GR-67).
    pub async fn ship(&self, link: &dyn LedgerLink) -> Result<Shipped, GateError> {
        // Two rounds at once would prepare the same batch. This is not the gate's lock:
        // calls go on while a round waits here.
        let _round = self.shipping.lock().await;
        let Some(shipment) = self.with_gate(|gate| gate.prepare_shipment()).await? else {
            return Ok(Shipped::NothingToSend);
        };
        let answer = tokio::time::timeout(
            self.config.call_timeout,
            link.ship(&shipment.request, &shipment.batch),
        )
        .await
        .map_err(|_| GateError::Link("the Ledger did not answer in time".into()))?
        .map_err(GateError::Link)?;
        let shipped = self
            .with_gate(move |gate| gate.apply_answer(&shipment, answer))
            .await?;
        if matches!(shipped, Shipped::NoProgress { .. }) {
            self.counters
                .ship_no_progress
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(shipped)
    }

    /// The router: `/mcp` is forwarded to the upstream endpoint.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/mcp", any(handle::<S>))
            .route("/v1/sessions", post(open_session::<S>))
            .with_state(self)
    }

    fn request(&self, method: Method, headers: &HeaderMap) -> reqwest::RequestBuilder {
        let mut request = self.http.request(method, &self.config.upstream);
        for (name, value) in headers {
            if passes(name) {
                request = request.header(name, value);
            }
        }
        // The gate does not decompress, so it asks for an answer it can read (GR-68).
        request.header("accept-encoding", "identity")
    }

    /// Send one request and read the whole answer, up to the cap, within the timeout.
    async fn exchange(&self, headers: &HeaderMap, body: Bytes) -> Result<Answer, ()> {
        let exchange = async {
            let mut up = self
                .request(Method::POST, headers)
                .body(body)
                .send()
                .await
                .map_err(|_| ())?;
            let (status, head) = (up.status(), up.headers().clone());
            // A redirect would send the client, and its credential, somewhere the gate is
            // not (GR-69).
            if status.is_redirection() {
                return Err(());
            }
            if let Some(session) = head.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                self.listed
                    .lock()
                    .expect("the tool list's lock")
                    .issue(session);
            }
            // Read with a running cap: a hostile server cannot make the gate hold more
            // than this (GR-67).
            let mut body = Vec::new();
            while let Some(chunk) = up.chunk().await.map_err(|_| ())? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Answer {
                status,
                headers: head,
                body: Bytes::from(body),
            })
        };
        tokio::time::timeout(self.config.call_timeout, exchange)
            .await
            .map_err(|_| ())?
    }

    async fn not_recorded(&self) {
        self.counters.not_recorded.fetch_add(1, Ordering::Relaxed);
        // Best effort: if even this cannot be written, the counter in memory still shows it.
        let _ = self.with_gate(|gate| gate.note_not_recorded()).await;
    }
}

/// Headers that belong to one hop, or that the client library sets itself, are not copied.
/// `x-gov-agent` is for the gate and does not go upstream.
fn passes(name: &HeaderName) -> bool {
    !matches!(
        name.as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "content-encoding"
            | "accept-encoding"
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "location"
    ) && !name.as_str().starts_with("x-gov-")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    mandate: Envelope,
    request: Envelope,
}

/// Open a session for the shim. The answer to a refusal says only that it was refused, or
/// that the agent has too many sessions waiting for their first proof.
async fn open_session<S: Spool + 'static>(
    State(f): State<Arc<Forwarder<S>>>,
    body: Bytes,
) -> Response {
    let Some(sessions) = &f.sessions else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(asked) = gov_core::strict::parse(&body).and_then(|v| {
        serde_json::from_value::<OpenRequest>(v).map_err(|_| gov_core::Error::Malformed("session"))
    }) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let opened = sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .open(&asked.mandate, &asked.request, (f.clock)());
    match opened {
        Ok((session, challenge)) => {
            axum::Json(json!({"session": session, "challenge": challenge})).into_response()
        }
        Err(OpenRefused::Busy) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(OpenRefused::Invalid) => StatusCode::FORBIDDEN.into_response(),
    }
}

/// What a call presented besides itself: a session, and for it a proof and a salt.
struct Shim {
    session: Id,
    proof: Option<Envelope>,
    salt: Option<Salt>,
}

impl Shim {
    /// From the `x-gov-session`, `x-gov-proof` and `x-gov-salt` headers. `None` when the call
    /// names no session: a stock client.
    fn from(headers: &HeaderMap) -> Option<Self> {
        let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let session = Id::new(text("x-gov-session")?).ok()?;
        let proof = text("x-gov-proof")
            .and_then(|p| URL_SAFE_NO_PAD.decode(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let salt = text("x-gov-salt")
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .and_then(|bytes| Salt::try_from(bytes).ok());
        Some(Shim {
            session,
            proof,
            salt,
        })
    }
}

fn refusal(status: StatusCode, message: &str) -> Response {
    let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": message}});
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("a response")
}

fn bad_gateway() -> Response {
    refusal(
        StatusCode::BAD_GATEWAY,
        "the gate could not get an answer from the server",
    )
}

/// Copy the upstream's status and headers onto an answer with this body.
fn answer(status: StatusCode, headers: &reqwest::header::HeaderMap, body: Body) -> Response {
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if passes(name) {
            response = response.header(name, value);
        }
    }
    response.body(body).unwrap_or_else(|_| bad_gateway())
}

fn answers(headers: &reqwest::header::HeaderMap, body: &[u8]) -> Vec<Value> {
    let content_type = headers.get("content-type").and_then(|v| v.to_str().ok());
    replies(is_event_stream(content_type), body)
}

async fn handle<S: Spool + 'static>(
    State(f): State<Arc<Forwarder<S>>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // A GET is the client's stream and a DELETE the end of its session: neither carries a
    // message, so neither is given a body to carry one in. Nothing else is taken.
    if method == Method::GET || method == Method::DELETE {
        return match f.request(method, &headers).send().await {
            Ok(up) if !up.status().is_redirection() => {
                let (status, head) = (up.status(), up.headers().clone());
                answer(status, &head, Body::from_stream(up.bytes_stream()))
            }
            _ => bad_gateway(),
        };
    }
    if method != Method::POST {
        return refusal(
            StatusCode::METHOD_NOT_ALLOWED,
            "the gate takes POST, GET and DELETE",
        );
    }
    if body.len() > MAX_BODY {
        return refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the request is too large for the gate",
        );
    }
    // One meaning or none: a body the gate cannot read, or could read two ways, is not
    // forwarded for the server to read its own way.
    let Ok(message) = gov_core::strict::parse(&body) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "the gate could not read the request as one message",
        );
    };
    // A batch could carry tool calls past the receipts. The protocol no longer has batches.
    if !message.is_object() {
        return refusal(
            StatusCode::BAD_REQUEST,
            "the gate does not take batched requests",
        );
    }
    let body = Bytes::from(serde_json::to_vec(&message).expect("a value serialises"));
    let session = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    match message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "tools/call" => call(f, headers, message, body, session).await,
        "tools/list" => list(f, headers, message, body, session).await,
        _ => match f.exchange(&headers, body).await {
            Ok(a) => answer(a.status, &a.headers, Body::from(a.body)),
            Err(()) => bad_gateway(),
        },
    }
}

/// A listing: forwarded, and remembered for this session so that each later call can be
/// checked against the definition the server showed (threat T14).
async fn list<S: Spool + 'static>(
    f: Arc<Forwarder<S>>,
    headers: HeaderMap,
    message: Value,
    body: Bytes,
    session: String,
) -> Response {
    let Ok(a) = f.exchange(&headers, body).await else {
        return bad_gateway();
    };
    let content_type = a.headers.get("content-type").and_then(|v| v.to_str().ok());
    f.listed.lock().expect("the tool list's lock").remember(
        &session,
        &message,
        a.status.is_success(),
        is_event_stream(content_type),
        &a.body,
    );
    answer(a.status, &a.headers, Body::from(a.body))
}

/// A tool call: a receipt before it goes and one after, whatever becomes of the client.
async fn call<S: Spool + 'static>(
    f: Arc<Forwarder<S>>,
    headers: HeaderMap,
    message: Value,
    body: Bytes,
    session: String,
) -> Response {
    // An answer is matched to its call by the call's id. A call without one has no answer
    // to read, so it is not taken (GR-70).
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        return refusal(
            StatusCode::BAD_REQUEST,
            "a tool call through the gate carries an id",
        );
    }
    let params = message.get("params");
    let tool = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or(json!({}));
    let agent = headers
        .get("x-gov-agent")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Id::new(v).ok())
        .unwrap_or_else(|| Id::new("unnamed-agent").expect("an identifier"));
    // A session is only looked at where sessions are served; elsewhere the headers are
    // dropped like any other of the gate's own.
    let shim = f.sessions.as_ref().and_then(|_| Shim::from(&headers));
    // No more calls are recorded at once than the gate will hold answers for. Past that,
    // observe mode still blocks nothing: the call goes through, counted as not recorded,
    // and held only for as long as its client stays (review finding GR-99).
    let Ok(permit) = f.in_flight.clone().try_acquire_owned() else {
        f.not_recorded().await;
        return match f.exchange(&headers, body).await {
            Ok(a) => answer(a.status, &a.headers, Body::from(a.body)),
            Err(()) => bad_gateway(),
        };
    };
    let listed = f
        .listed
        .lock()
        .expect("the tool list's lock")
        .digest(&session, &tool);
    // From the intent receipt to the outcome receipt, the call runs in a task of its own.
    // If the client goes away at any point after this line, even between the intent being
    // written and the request being sent, or if the server never answers, the intent is
    // still closed: as failed, or as `unknown` when the gate cannot say what the server did
    // (review finding GR-63). An intent is never written by code the client can abandon.
    let task = tokio::spawn(async move {
        let _permit = permit;
        let mut reopen = false;
        // With the shim, the salt is the one it drew and signed over; with a stock client, or
        // a salt that cannot be read, the gate draws one itself, fresh for this call (spec
        // §4.3). Mapping never fails, so there is always something to record (GR-61).
        let sent_salt = shim.as_ref().and_then(|s| s.salt);
        let begun = match sent_salt.map_or_else(gov_core::random_salt, Ok) {
            Ok(salt) => {
                let mapped = f.config.map.map(&tool, listed.as_ref(), &args, &salt);
                if !mapped.known {
                    f.counters.unmapped.fetch_add(1, Ordering::Relaxed);
                }
                let now = (f.clock)();
                // Who made the call. Proven is said by `Sessions::prove` alone, for the
                // action computed just above from this gate's own map and listing.
                let who = match (&shim, &f.sessions) {
                    (Some(shim), Some(sessions)) => {
                        let canonical = gov_core::action::args_digest(&args, &salt).is_ok();
                        let proved = sessions
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .prove(
                                &shim.session,
                                shim.proof.as_ref(),
                                shim.salt.as_ref(),
                                &mapped.action.digest(),
                                canonical,
                                now,
                            );
                        match &proved {
                            Proved::Proven(_) => {
                                f.counters.proven.fetch_add(1, Ordering::Relaxed);
                            }
                            Proved::Failed { why, .. } => {
                                f.counters.proof_failed.fetch_add(1, Ordering::Relaxed);
                                // The shim opens another session before its next call.
                                reopen = matches!(
                                    why,
                                    ProofFailure::UnknownSession
                                        | ProofFailure::MandateExpired
                                        | ProofFailure::SessionSpent
                                );
                            }
                        }
                        Who::from_session(&proved)
                    }
                    // A stock client only says who it is.
                    _ => Who::claimed(agent),
                };
                f.with_gate(move |gate| {
                    let observed = Observed {
                        who,
                        action: &mapped.action,
                        reversal: mapped.reversal,
                        // The agent holds its own credential; the gate is only in the path.
                        coverage: Coverage::InLine,
                    };
                    gate.begin(&observed, now)
                })
                .await
            }
            Err(e) => Err(GateError::from(e)),
        };
        let intent = match begun {
            Ok(seq) => Some(seq),
            // Observe mode blocks nothing. The call goes through and is counted as not recorded,
            // where an operator can see it.
            Err(_) => {
                f.not_recorded().await;
                None
            }
        };
        {
            let result = f.exchange(&headers, body).await;
            let status = match &result {
                Err(()) => OutcomeStatus::Unknown,
                Ok(a) => {
                    let replies = answers(&a.headers, &a.body);
                    let reply = replies.iter().find(|r| r.get("id") == Some(&id));
                    let failed = !a.status.is_success()
                        || reply.is_none_or(|r| {
                            r.get("error").is_some()
                                || r.get("result").and_then(|x| x.get("isError"))
                                    == Some(&Value::Bool(true))
                        });
                    if failed {
                        OutcomeStatus::Failed
                    } else {
                        OutcomeStatus::Ok
                    }
                }
            };
            if let Some(seq) = intent {
                let now = (f.clock)();
                let finished = f
                    .with_gate(move |gate| gate.finish(seq, status, None, now))
                    .await;
                match finished {
                    Ok(_) => {
                        f.counters.recorded.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => f.not_recorded().await,
                }
            }
            (result, reopen)
        }
    });
    match task.await {
        Ok((Ok(a), reopen)) => {
            let mut response = answer(a.status, &a.headers, Body::from(a.body));
            if reopen {
                // The call was forwarded, once. This only tells the shim that its session is
                // gone, so it opens another before the next call and never sends this one
                // again (review finding GR-89).
                response
                    .headers_mut()
                    .insert("x-gov-reopen", HeaderValue::from_static("1"));
            }
            response
        }
        _ => bad_gateway(),
    }
}
