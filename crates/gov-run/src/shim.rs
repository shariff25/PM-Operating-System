//! The shim (spec §5.3.1): a small program an agent's runtime starts beside the agent. To the
//! agent it is the MCP server; it forwards every message to the gate and adds, to each tool
//! call, the proof that the agent's key is held here.
//!
//! It holds the agent's private key and mandate. Whatever can send it a request with its
//! secret acts as the agent, so the secret is made at each start and written where only the
//! runtime that started the shim can read it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
use gov_core::policy::{may_replace, open_bundle, HeldBundle};
use gov_core::request::{RequestProof, SessionOpen};
use gov_core::{random_id, random_salt, seal, Envelope, Id, PublicKey, Scope, SigningKey, VERSION};
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
    /// How this shim follows its gate to another policy bundle while it runs. Without it
    /// the shim keeps the map it started with.
    pub follow: Option<Follow>,
}

/// What a shim needs to take the bundle its gate has moved to (spec §7.6.3, point 7; review
/// finding GR-133). The gate only hands the bundle over: it is believed as far as the root
/// key signed it, and never if it would take the shim back.
pub struct Follow {
    /// The root key this shim pinned itself.
    pub root: PublicKey,
    /// The bundle the shim's map came from.
    pub held: HeldBundle,
    /// Where the newest bundle accepted is noted, before it is used.
    pub state: Option<PathBuf>,
    /// The server whose map this shim uses, as bundles name it.
    pub server: Id,
}

/// What a receipt carries for a bundle: its number and the start of its digest.
fn version_of(held: &HeldBundle) -> Id {
    Id::new(&format!("{}-{}", held.number, &held.digest.as_str()[..16]))
        .expect("digits and hex make an id")
}

/// The shim's map, and the bundle it came from.
struct Using {
    map: Arc<ServerMap>,
    held: Option<HeldBundle>,
    version: Option<Id>,
}

/// A session open at the gate.
struct Open {
    id: Id,
    challenge: Id,
    counter: u64,
    /// The bundle the gate said it decides from, when the session was opened.
    policy: Option<String>,
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
    using: Mutex<Using>,
    /// Bundles taken from the gate, and bundles the gate offered that were not taken.
    pub followed: AtomicU64,
    pub not_followed: AtomicU64,
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
            using: Mutex::new(Using {
                map: Arc::new(config.map.clone()),
                held: config.follow.as_ref().map(|f| f.held.clone()),
                version: config.follow.as_ref().map(|f| version_of(&f.held)),
            }),
            followed: AtomicU64::new(0),
            not_followed: AtomicU64::new(0),
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
            policy: body
                .get("policy")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// The map in use.
    fn map(&self) -> Arc<ServerMap> {
        self.using.lock().expect("the map's lock").map.clone()
    }

    /// The gate says it decides from the bundle `said`. If that is not the one this shim's
    /// map came from, ask the gate for it and take it, by the rules a program starts by:
    /// it opens under the root key this shim pinned, it is the one the gate named, and it
    /// does not take the shim back to an older bundle or to another under the same number.
    /// Anything else is not taken, and the shim keeps its map: its proofs then fail at
    /// the gate, which is recorded there, and nothing is read by a map nobody signed.
    async fn follow(&self, session: &Id, said: &str) {
        let Some(follow) = &self.config.follow else {
            return;
        };
        let held = {
            let using = self.using.lock().expect("the map's lock");
            if using.version.as_ref().is_some_and(|v| v.as_str() == said) {
                return;
            }
            using.held.clone()
        };
        let taken = async {
            let url = format!("{}/v1/bundle", self.config.gate_url.trim_end_matches('/'));
            let mut answer = self
                .http
                .post(url)
                .timeout(self.config.call_timeout)
                .json(&json!({"session": session}))
                .send()
                .await
                .ok()
                .filter(|r| r.status().is_success())?;
            let mut body = Vec::new();
            while let Some(chunk) = answer.chunk().await.ok()? {
                if body.len() + chunk.len() > MAX_BODY {
                    return None;
                }
                body.extend_from_slice(&chunk);
            }
            let value = gov_core::strict::parse(&body).ok()?;
            let envelope: Envelope = serde_json::from_value(value.get("bundle")?.clone()).ok()?;
            let opened = open_bundle(&envelope, &follow.root, &self.config.scope).ok()?;
            if opened.policy_version().as_str() != said {
                return None;
            }
            may_replace(held.as_ref(), &opened.held()).ok()?;
            if let Some(state) = &follow.state {
                crate::policy_source::note(state, &opened.held()).ok()?;
            }
            Some(opened)
        }
        .await;
        let Some(opened) = taken else {
            self.not_followed.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let current = self.map();
        let map = opened
            .bundle
            .connectors
            .iter()
            .find(|m| m.server == follow.server)
            .cloned()
            // The new bundle maps nothing for this server: every tool is then unknown, on
            // both sides alike.
            .unwrap_or(ServerMap {
                tools: Vec::new(),
                labels: Vec::new(),
                clearances: Vec::new(),
                ..(*current).clone()
            });
        *self.using.lock().expect("the map's lock") = Using {
            map: Arc::new(map),
            version: Some(opened.policy_version()),
            held: Some(opened.held()),
        };
        self.followed.fetch_add(1, Ordering::Relaxed);
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
            // The gate says which bundle it decides from: be on the same one before the
            // first proof is made.
            if let Some((id, said)) = session
                .as_ref()
                .and_then(|o| Some((o.id.clone(), o.policy.clone()?)))
            {
                self.follow(&id, &said).await;
            }
        }
        let map = self.map();
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
            let action = map.map(tool, listed.as_ref(), &args, &salt).action;
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
            // The gate has moved to another bundle: follow it before the next call. This
            // call was read by the gate under its new map and proven under the old one, so
            // it may be recorded as a failed proof, once (a stated limit).
            if let (Some(open), Some(said)) = (
                session.as_ref(),
                head.get("x-gov-policy").and_then(|v| v.to_str().ok()),
            ) {
                self.follow(&open.id.clone(), said).await;
            }
            if let Some(reopen) = head.get("x-gov-reopen") {
                *session = None;
                // The gate says whether it still holds a listing for this MCP session. If it
                // has lost it, as after a restart, the shim forgets its own, so that both map
                // the next call the same way (as `unknown`, until the tools are listed
                // again) and its proof holds. If the session only ran out, the gate still
                // remembers, and so does the shim (review findings GR-103 and GR-112).
                if reopen.as_bytes() == b"listing-lost" {
                    *self.listed.lock().expect("the tool list's lock") = Listings::default();
                }
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
        // As the gate does: a session the server answers under is one it knows (GR-113).
        if !mcp_session.is_empty() && status.is_success() {
            listed.issue(&mcp_session);
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
