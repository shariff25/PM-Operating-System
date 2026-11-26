//! The shim (spec §5.3.1): a small program an agent's runtime starts beside the agent. To the
//! agent it is the MCP server; it forwards every message to the gate and adds, to each tool
//! call, the proof that the agent's key is held here.
//!
//! It holds the agent's private key and mandate. Whatever can send it a request with its
//! secret acts as the agent, so the secret is made at each start and written where only the
//! runtime that started the shim can read it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use gov_core::connector::ServerMap;
use gov_core::mandate::Mandate;
use gov_core::request::{RequestProof, SessionOpen};
use gov_core::{random_id, random_salt, seal, Envelope, Id, Scope, SigningKey, VERSION};
use gov_gate::forward::{is_event_stream, Listings};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use crate::{carries_secret, now};

/// The largest request or answer the shim will hold: the gate's own limit.
const MAX_BODY: usize = 4 * 1024 * 1024;

pub struct ShimConfig {
    pub scope: Scope,
    /// The gate's name, as its receipts carry it.
    pub gate: Id,
    /// The gate's address, for example `http://127.0.0.1:8791`.
    pub gate_url: String,
    pub key: SigningKey,
    /// The agent's mandate chain, as issued.
    pub mandate: Envelope,
    /// The same connector map the gate uses.
    pub map: ServerMap,
    /// Required on every request, in `x-gov-shim-key`.
    pub secret: String,
    pub call_timeout: Duration,
}

/// A session open at the gate.
struct Open {
    id: Id,
    challenge: Id,
    counter: u64,
}

pub struct Shim {
    config: ShimConfig,
    mandate_id: Id,
    http: reqwest::Client,
    /// The address this shim listens on: the only `Host` it answers to.
    host: String,
    /// The session, and with it the order of tool calls: one is sent at a time, so that the
    /// counter rises in the order the gate sees (review finding GR-92).
    session: AsyncMutex<Option<Open>>,
    listed: Mutex<Listings>,
}

impl Shim {
    pub fn new(config: ShimConfig, addr: SocketAddr) -> anyhow::Result<Arc<Self>> {
        config
            .map
            .validate()
            .map_err(|_| anyhow::anyhow!("the connector map does not pass its lint"))?;
        // The mandate's own id, read from the mandate. The shim does not judge the mandate:
        // the gate does.
        let mandate: Mandate = URL_SAFE_NO_PAD
            .decode(&config.mandate.payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| anyhow::anyhow!("the mandate file does not hold a mandate"))?;
        anyhow::ensure!(
            mandate.agent.key == config.key.public(),
            "the mandate is for another key than the one in the key file"
        );
        Ok(Arc::new(Shim {
            mandate_id: mandate.id,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            host: addr.to_string(),
            session: AsyncMutex::new(None),
            listed: Mutex::new(Listings::default()),
            config,
        }))
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new().route("/mcp", any(handle)).with_state(self)
    }

    fn mcp(&self) -> String {
        format!("{}/mcp", self.config.gate_url.trim_end_matches('/'))
    }

    /// Open a session at the gate, on a request signed with the agent's key.
    async fn open(&self) -> Option<Open> {
        let c = &self.config;
        let request = seal(
            &SessionOpen {
                v: VERSION,
                tenant: c.scope.tenant.clone(),
                env: c.scope.env.clone(),
                gate: c.gate.clone(),
                mandate: self.mandate_id.clone(),
                nonce: random_id("open").ok()?,
                at: now(),
            },
            &c.key,
        )
        .ok()?;
        let url = format!("{}/v1/sessions", c.gate_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .timeout(c.call_timeout)
            .json(&json!({"mandate": c.mandate, "request": request}))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: Value = response.json().await.ok()?;
        Some(Open {
            id: Id::new(body.get("session")?.as_str()?).ok()?,
            challenge: Id::new(body.get("challenge")?.as_str()?).ok()?,
            counter: 0,
        })
    }

    /// Send a request on to the gate and read the whole answer, up to the cap.
    async fn send(
        &self,
        headers: &HeaderMap,
        body: Bytes,
        extra: &[(&str, String)],
    ) -> Result<(StatusCode, reqwest::header::HeaderMap, Bytes), ()> {
        let exchange = async {
            let mut request = self.http.post(self.mcp());
            for (name, value) in headers {
                if passes(name) {
                    request = request.header(name, value);
                }
            }
            for (name, value) in extra {
                request = request.header(*name, value);
            }
            let mut up = request.body(body).send().await.map_err(|_| ())?;
            let (status, head) = (up.status(), up.headers().clone());
            if status.is_redirection() {
                return Err(());
            }
            let mut body = Vec::new();
            while let Some(chunk) = up.chunk().await.map_err(|_| ())? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(());
                }
                body.extend_from_slice(&chunk);
            }
            Ok((status, head, Bytes::from(body)))
        };
        tokio::time::timeout(self.config.call_timeout, exchange)
            .await
            .map_err(|_| ())?
    }

    /// A tool call: a salt, the action as the gate will compute it, a proof over it, and the
    /// call sent once. It is never sent a second time: when the gate says the session is
    /// gone, the call has already been forwarded, and only the next call gets a new session
    /// (review finding GR-89).
    async fn call(
        &self,
        headers: &HeaderMap,
        message: &Value,
        body: Bytes,
        mcp_session: &str,
    ) -> Result<(StatusCode, reqwest::header::HeaderMap, Bytes), ()> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = self.open().await;
        }
        let signed = session.as_mut().and_then(|open| {
            let c = &self.config;
            let params = message.get("params");
            let tool = params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let salt = random_salt().ok()?;
            let listed = self
                .listed
                .lock()
                .expect("the tool list's lock")
                .digest(mcp_session, tool);
            let action = c.map.map(tool, listed.as_ref(), &args, &salt).action;
            let proof = seal(
                &RequestProof {
                    v: VERSION,
                    tenant: c.scope.tenant.clone(),
                    env: c.scope.env.clone(),
                    gate: c.gate.clone(),
                    session: open.id.clone(),
                    challenge: open.challenge.clone(),
                    counter: open.counter + 1,
                    mandate: self.mandate_id.clone(),
                    action_digest: action.digest(),
                },
                &c.key,
            )
            .ok()?;
            open.counter += 1;
            Some(vec![
                ("x-gov-session", open.id.as_str().to_string()),
                (
                    "x-gov-proof",
                    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&proof).ok()?),
                ),
                ("x-gov-salt", URL_SAFE_NO_PAD.encode(salt)),
            ])
        });
        // With no session to be had (the gate refused one, or is not there), the call still
        // goes: the gate records it as a client that offered no proof.
        let sent = self.send(headers, body, &signed.unwrap_or_default()).await;
        if let Ok((_, head, _)) = &sent {
            if head.contains_key("x-gov-reopen") {
                *session = None;
                // A gate that has lost the session has most likely restarted, and then it
                // has lost its record of the tool listings too. The shim forgets its own,
                // so that both map the next call the same way (as `unknown`, until the
                // tools are listed again) and its proof holds (review finding GR-103).
                *self.listed.lock().expect("the tool list's lock") = Listings::default();
            }
        }
        sent
    }
}

/// Headers that belong to one hop, the shim's own secret, and anything named like the
/// gate's own headers are not copied in either direction.
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

fn refusal(status: StatusCode, message: &str) -> Response {
    let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": message}});
    (
        status,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn answer(status: StatusCode, headers: &reqwest::header::HeaderMap, body: Body) -> Response {
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if passes(name) {
            response = response.header(name, value);
        }
    }
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

async fn handle(
    State(shim): State<Arc<Shim>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Who may use the shim (review finding GR-90). A page in a browser on this machine can
    // reach a local port: it cannot know the secret, it sends an `Origin`, and a name it
    // rebinds to this address is not this address.
    if !carries_secret(&headers, "x-gov-shim-key", &shim.config.secret) {
        return refusal(StatusCode::UNAUTHORIZED, "the shim's secret is required");
    }
    let host = headers.get("host").and_then(|v| v.to_str().ok());
    if headers.contains_key("origin") || host != Some(shim.host.as_str()) {
        return refusal(StatusCode::FORBIDDEN, "the shim answers its runtime only");
    }
    if method == Method::GET || method == Method::DELETE {
        let mut request = shim.http.request(method, shim.mcp());
        for (name, value) in &headers {
            if passes(name) {
                request = request.header(name, value);
            }
        }
        return match request.send().await {
            Ok(up) if !up.status().is_redirection() => {
                let (status, head) = (up.status(), up.headers().clone());
                answer(status, &head, Body::from_stream(up.bytes_stream()))
            }
            _ => StatusCode::BAD_GATEWAY.into_response(),
        };
    }
    if method != Method::POST {
        return refusal(StatusCode::METHOD_NOT_ALLOWED, "POST, GET and DELETE");
    }
    // Exactly one content type, and that one JSON: a request that names two is not judged by
    // the one that suits it.
    let mut types = headers.get_all("content-type").iter();
    let json_body = types
        .next()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.split(';').next().unwrap_or_default().trim() == "application/json")
        && types.next().is_none();
    if !json_body {
        return refusal(StatusCode::UNSUPPORTED_MEDIA_TYPE, "application/json only");
    }
    if body.len() > MAX_BODY {
        return refusal(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large");
    }
    // The same strict reader as the gate, and what is forwarded is what was read: the shim
    // never signs one reading of a request and sends another (review finding GR-94).
    let Ok(message) = gov_core::strict::parse(&body) else {
        return refusal(StatusCode::BAD_REQUEST, "the request is not one message");
    };
    if !message.is_object() {
        return refusal(StatusCode::BAD_REQUEST, "batched requests are not taken");
    }
    let body = Bytes::from(serde_json::to_vec(&message).expect("a value serialises"));
    let mcp_session = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let method_name = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let has_id = message.get("id").is_some_and(|id| !id.is_null());
    let sent = if method_name == "tools/call" && has_id {
        shim.call(&headers, &message, body, &mcp_session).await
    } else {
        shim.send(&headers, body, &[]).await
    };
    let Ok((status, head, bytes)) = sent else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    // Remember what the gate remembers, from the same answers.
    {
        let mut listed = shim.listed.lock().expect("the tool list's lock");
        if let Some(issued) = head.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            listed.issue(issued);
        }
        if method_name == "tools/list" {
            let content_type = head.get("content-type").and_then(|v| v.to_str().ok());
            listed.remember(
                &mcp_session,
                &message,
                status.is_success(),
                is_event_stream(content_type),
                &bytes,
            );
        }
    }
    answer(status, &head, Body::from(bytes))
}
