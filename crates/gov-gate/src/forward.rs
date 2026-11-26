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
use axum::http::{HeaderMap, HeaderName, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use gov_core::connector::{tool_digest, ServerMap};
use gov_core::receipt::{Coverage, OutcomeStatus, Proof};
use gov_core::{Digest, Id};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use crate::{Gate, GateError, LedgerLink, Observed, Shipped, Spool};

/// The largest request, or answer the gate must read, that it will hold.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The most sessions whose tool listings are remembered. Past it the memory is emptied: a
/// forgotten listing only makes a tool `unknown` until it is listed again.
const MAX_SESSIONS: usize = 4096;

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
}

pub struct ForwarderConfig {
    /// The one upstream MCP endpoint. Every request goes there and nowhere else.
    pub upstream: String,
    pub map: ServerMap,
    /// How long the server has to answer a request before the gate gives up on it.
    pub call_timeout: Duration,
}

pub struct Forwarder<S> {
    gate: AsyncMutex<Gate<S>>,
    config: ForwarderConfig,
    http: reqwest::Client,
    /// For each MCP session, the digest of each tool's definition as the server last listed
    /// it there. One session's listing never stands in for another's.
    listed: Mutex<BTreeMap<String, BTreeMap<String, Digest>>>,
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
    pub fn new(gate: Gate<S>, config: ForwarderConfig) -> Result<Arc<Self>, GateError> {
        config.map.validate()?;
        Ok(Arc::new(Forwarder {
            gate: AsyncMutex::new(gate),
            config,
            // The upstream is one fixed address: no redirect is followed.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| GateError::Link(e.to_string()))?,
            listed: Mutex::new(BTreeMap::new()),
            clock: wall_clock,
            counters: Counters::default(),
        }))
    }

    /// The gate itself, for trimming, which the gate's owner drives. While this is held,
    /// calls wait.
    pub async fn gate(&self) -> tokio::sync::MutexGuard<'_, Gate<S>> {
        self.gate.lock().await
    }

    /// One round of shipping to the Ledger. The gate is held only to collect the batch and
    /// to apply the answer, never while the Ledger is being waited for, so a slow Ledger does
    /// not hold up a single call (review finding GR-67).
    pub async fn ship(&self, link: &dyn LedgerLink) -> Result<Shipped, GateError> {
        let Some(shipment) = self.gate().await.prepare_shipment()? else {
            return Ok(Shipped::NothingToSend);
        };
        let answer = tokio::time::timeout(
            self.config.call_timeout,
            link.ship(&shipment.request, &shipment.batch),
        )
        .await
        .map_err(|_| GateError::Link("the Ledger did not answer in time".into()))?
        .map_err(GateError::Link)?;
        let shipped = self.gate().await.apply_answer(&shipment, answer)?;
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
        let _ = self.gate().await.note_not_recorded();
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
            | "x-gov-agent"
    )
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

/// The JSON-RPC answers in a body: one JSON object, or the `data:` lines of an event stream.
fn answers(headers: &reqwest::header::HeaderMap, body: &[u8]) -> Vec<Value> {
    let event_stream = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/event-stream"));
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
    let continued = message
        .get("params")
        .and_then(|p| p.get("cursor"))
        .is_some_and(|c| !c.is_null());
    let mut seen: BTreeMap<String, Option<Digest>> = BTreeMap::new();
    if a.status.is_success() {
        for tool in answers(&a.headers, &a.body)
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
    let mut listed = f.listed.lock().expect("the tool list's lock");
    if listed.len() >= MAX_SESSIONS && !listed.contains_key(&session) {
        listed.clear();
    }
    let entry = listed.entry(session).or_default();
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
    drop(listed);
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
        .unwrap_or_default();
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or(json!({}));
    let agent = headers
        .get("x-gov-agent")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Id::new(v).ok())
        .unwrap_or_else(|| Id::new("unnamed-agent").expect("an identifier"));
    let listed = f
        .listed
        .lock()
        .expect("the tool list's lock")
        .get(&session)
        .and_then(|tools| tools.get(tool))
        .cloned();
    // With no shim the gate draws the salt itself, fresh for this call (spec §4.3). Mapping
    // never fails, so there is always something to record (GR-61).
    let begun = match gov_core::random_salt() {
        Ok(salt) => {
            let mapped = f.config.map.map(tool, listed.as_ref(), &args, &salt);
            if !mapped.known {
                f.counters.unmapped.fetch_add(1, Ordering::Relaxed);
            }
            let observed = Observed {
                agent: &agent,
                // A stock client only says who it is. Proven comes with the shim.
                proof: Proof::Claimed,
                action: &mapped.action,
                reversal: mapped.reversal,
                // The agent holds its own credential; the gate is only in the path.
                coverage: Coverage::InLine,
            };
            f.gate().await.begin(&observed, (f.clock)())
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
    // The exchange and the outcome receipt run in a task of their own. If the client goes
    // away, or the server never answers, the intent is still closed: as failed, or as
    // `unknown` when the gate cannot say what the server did (review finding GR-63).
    let task = tokio::spawn({
        let f = f.clone();
        async move {
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
                let finished = f.gate().await.finish(seq, status, None, (f.clock)());
                match finished {
                    Ok(_) => {
                        f.counters.recorded.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => f.not_recorded().await,
                }
            }
            result
        }
    });
    match task.await {
        Ok(Ok(a)) => answer(a.status, &a.headers, Body::from(a.body)),
        _ => bad_gateway(),
    }
}
