//! The MCP front door in observe mode (spec §5.2, §5.3): the gate stands between an agent's
//! MCP client and one upstream MCP server, passes every message through unchanged, and
//! writes a receipt before and after each tool call.
//!
//! In observe mode the agent keeps its own credential, so that credential passes through
//! this code in the request's headers. It is forwarded and nothing else: never logged, never
//! stored, never put in a receipt. Nothing is blocked. A stock client's identity is only what
//! it says in the `x-gov-agent` header, and the receipt records it as claimed.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use gov_core::action::Salt;
use gov_core::connector::{tool_digest, ServerMap};
use gov_core::receipt::{Coverage, OutcomeStatus, Proof};
use gov_core::{Digest, Id};
use serde_json::{json, Value};

use crate::{Gate, GateError, Observed, Spool};

/// The largest request or buffered answer the forwarder will hold.
const MAX_BODY: usize = 4 * 1024 * 1024;

/// What an operator must be able to see (spec §3.4: observe mode protects nothing, and what
/// it failed to record must not be silent).
#[derive(Debug, Default)]
pub struct Counters {
    /// Tool calls passed through with a receipt before and after.
    pub recorded: AtomicU64,
    /// Tool calls passed through with no receipt, because the spool was full or failed.
    pub not_recorded: AtomicU64,
    /// Tool calls the connector map could not classify.
    pub unmapped: AtomicU64,
}

pub struct Forwarder<S> {
    gate: AsyncMutex<Gate<S>>,
    /// The one upstream MCP endpoint. Every request goes there and nowhere else.
    upstream: String,
    map: ServerMap,
    http: reqwest::Client,
    /// The digest of each tool's definition as the server last listed it.
    listed: Mutex<BTreeMap<String, Digest>>,
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

impl<S: Spool + 'static> Forwarder<S> {
    pub fn new(gate: Gate<S>, upstream: &str, map: ServerMap) -> Result<Arc<Self>, GateError> {
        map.validate()?;
        Ok(Arc::new(Forwarder {
            gate: AsyncMutex::new(gate),
            upstream: upstream.to_string(),
            map,
            // No redirects: the upstream is one fixed address.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| GateError::Link(e.to_string()))?,
            listed: Mutex::new(BTreeMap::new()),
            clock: wall_clock,
            counters: Counters::default(),
        }))
    }

    /// The gate itself, for shipping and trimming, which the gate's owner drives. While this
    /// is held, calls wait.
    pub async fn gate(&self) -> tokio::sync::MutexGuard<'_, Gate<S>> {
        self.gate.lock().await
    }

    /// The router: every method on `/mcp` is forwarded to the upstream endpoint.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/mcp", any(handle::<S>))
            .with_state(self)
    }

    async fn forward(
        &self,
        method: Method,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self.http.request(method, &self.upstream);
        for (name, value) in headers {
            if passes(name) {
                request = request.header(name, value);
            }
        }
        request.body(body).send().await
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
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-authenticate"
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
        "the gate could not reach the server",
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

/// The JSON-RPC answers in a buffered body: one JSON object, or the `data:` lines of an
/// event stream.
fn answers(content_type: &str, body: &[u8]) -> Vec<Value> {
    if content_type.starts_with("text/event-stream") {
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
    if body.len() > MAX_BODY {
        return refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the request is too large for the gate",
        );
    }
    // Anything but a POST carries no call: a client's stream, or the end of its session.
    // It is passed through as it arrives.
    if method != Method::POST {
        return match f.forward(method, &headers, body).await {
            Ok(up) => {
                let (status, head) = (up.status(), up.headers().clone());
                answer(status, &head, Body::from_stream(up.bytes_stream()))
            }
            Err(_) => bad_gateway(),
        };
    }
    let message: Option<Value> = serde_json::from_slice(&body).ok();
    // A batch could carry tool calls past the receipts. The protocol no longer has batches;
    // the gate does not take them.
    if matches!(message, Some(Value::Array(_))) {
        return refusal(
            StatusCode::BAD_REQUEST,
            "the gate does not take batched requests",
        );
    }
    let rpc_method = message
        .as_ref()
        .and_then(|m| m.get("method"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // A tool call: a receipt before it goes.
    let mut intent = None;
    if rpc_method == "tools/call" {
        let params = message.as_ref().and_then(|m| m.get("params"));
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
            .get(tool)
            .cloned();
        // With no shim the gate draws the salt itself, fresh for this call (spec §4.3).
        let mapped = gov_core::random_salt()
            .map_err(GateError::from)
            .and_then(|salt: Salt| Ok(f.map.map(tool, listed.as_ref(), &args, &salt)?));
        let begun = match mapped {
            Ok(mapped) => {
                if !mapped.known {
                    f.counters.unmapped.fetch_add(1, Ordering::Relaxed);
                }
                let call = Observed {
                    agent: &agent,
                    // A stock client only says who it is. Proven comes with the shim.
                    proof: Proof::Claimed,
                    action: &mapped.action,
                    reversal: mapped.reversal,
                    // The agent holds its own credential; the gate is only in the path.
                    coverage: Coverage::InLine,
                };
                f.gate().await.begin(&call, (f.clock)())
            }
            Err(e) => Err(e),
        };
        match begun {
            Ok(seq) => intent = Some(seq),
            // Observe mode blocks nothing. The call goes through and is counted as not
            // recorded, where an operator can see it.
            Err(_) => {
                f.counters.not_recorded.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let upstream = f.forward(Method::POST, &headers, body).await;
    let finish = |status: OutcomeStatus| {
        let f = f.clone();
        async move {
            if let Some(seq) = intent {
                match f.gate().await.finish(seq, status, None, (f.clock)()) {
                    Ok(_) => f.counters.recorded.fetch_add(1, Ordering::Relaxed),
                    Err(_) => f.counters.not_recorded.fetch_add(1, Ordering::Relaxed),
                };
            }
        }
    };
    let up = match upstream {
        Ok(up) => up,
        Err(_) => {
            // Whether the server acted is not known.
            finish(OutcomeStatus::Unknown).await;
            return bad_gateway();
        }
    };
    let (status, head) = (up.status(), up.headers().clone());
    let content_type = head
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    // Only what the gate must read is held in memory: the answer to a tool call or to a
    // listing. Everything else streams through.
    if rpc_method != "tools/call" && rpc_method != "tools/list" {
        return answer(status, &head, Body::from_stream(up.bytes_stream()));
    }
    let bytes = match up.bytes().await {
        Ok(b) if b.len() <= MAX_BODY => b,
        _ => {
            finish(OutcomeStatus::Unknown).await;
            return bad_gateway();
        }
    };
    let replies = answers(&content_type, &bytes);
    if rpc_method == "tools/list" && status.is_success() {
        let mut listed = f.listed.lock().expect("the tool list's lock");
        for tool in replies
            .iter()
            .filter_map(|r| r.get("result")?.get("tools")?.as_array())
            .flatten()
        {
            if let (Some(name), Ok(digest)) =
                (tool.get("name").and_then(Value::as_str), tool_digest(tool))
            {
                listed.insert(name.to_string(), digest);
            }
        }
    }
    if rpc_method == "tools/call" {
        let id = message.as_ref().and_then(|m| m.get("id"));
        let reply = replies.iter().find(|r| r.get("id") == id);
        let failed = !status.is_success()
            || reply.is_none_or(|r| {
                r.get("error").is_some()
                    || r.get("result").and_then(|x| x.get("isError")) == Some(&Value::Bool(true))
            });
        finish(if failed {
            OutcomeStatus::Failed
        } else {
            OutcomeStatus::Ok
        })
        .await;
    }
    answer(status, &head, Body::from(bytes))
}
