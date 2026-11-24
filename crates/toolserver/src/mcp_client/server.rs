//! One MCP server, as the tool server uses it (`docs/mcp-servers-spec.md` §2.4, §2.8, §2.10).
//!
//! - **Discovery and the allowlist.** Every new connection calls `initialize` and `tools/list`;
//!   so does a call after `notifications/tools/list_changed` or 10 minutes. A tool is usable only
//!   if the record lists its exact name and the digest of its definition matches. A re-list can
//!   only take tools away: an approved tool that changed or went away is revoked for the life of
//!   the process (it comes back only with a new approved digest, which needs a restart with the
//!   merged record), audited `mcp.tool_changed`, and every call to it is refused before anything
//!   is sent.
//! - **Calls.** At most 8 in flight per server and 60 a minute per user; past either the call
//!   answers "rate limited" without reaching the server. A read may take 10 seconds and a write
//!   30, connecting included. A read is retried once on a fresh connection after a transport
//!   error; a write never is, and one that breaks or times out "may or may not have happened".
//!   `tools/call` is sent once: a server's `input_required` or task answer is refused, never
//!   driven.
//! - **Results.** At most 256 KiB (a streamed event above that is refused by the transport
//!   before it is parsed; rmcp's own cap on a plain JSON answer is 16 MiB, and this cap is
//!   checked after it). `structuredContent` is preferred; otherwise the text blocks (as JSON if
//!   they are JSON). Images, audio, resources and `_meta` are never read.
//! - **Connections.** One shared connection for `service_token` and `none`; one per reader for
//!   `oauth_per_user` (the authorization differs), replaced when the token changes. A
//!   connection unused for 5 minutes is closed before its next use.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pmos_contract::mcp::{AuthKind, ServerRecord, ToolMapping};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use super::handler::{Events, RefusingHandler, CHANGED_EVENT, CONNECT_EVENT};
use super::mapping;
use super::transport::{http_client, TestRoutes};
use crate::audit::{AuditSink, Outcome};
use crate::connectors::ConnectorError;
use crate::vault::Secret;

type Client = RunningService<RoleClient, RefusingHandler>;

/// A PM's (or a team bot's) token for one server, from the vault.
#[async_trait]
pub trait UserTokens: Send + Sync {
    async fn token(&self, user: &str, server: &str) -> Result<Secret, ConnectorError>;
}

/// How the tool server authenticates to a server.
#[derive(Clone)]
pub enum ServerAuth {
    /// A bearer token PM OS holds for its own server.
    Token(Secret),
    /// Public data, no token.
    None,
    /// Each reader's own token.
    PerUser(Arc<dyn UserTokens>),
}

/// The caps of §2.10 and §2.8.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub read: Duration,
    pub write: Duration,
    pub in_flight: usize,
    pub per_user_per_minute: usize,
    pub max_response_bytes: usize,
    pub relist_every: Duration,
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(10),
            write: Duration::from_secs(30),
            in_flight: 8,
            per_user_per_minute: 60,
            max_response_bytes: 256 * 1024,
            relist_every: Duration::from_secs(600),
            idle: Duration::from_secs(300),
        }
    }
}

/// A read or a write: their deadlines and retries differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Read,
    Write,
}

/// What the last `tools/list` found for one approved tool: for Admins (Policy Admin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCheck {
    pub upstream: String,
    pub approved_digest: String,
    /// `approved`, `changed`, `missing`, or `unchecked` before the first connection.
    pub state: &'static str,
    /// The digest the server listed it with at the last check, if it listed it.
    pub listed_digest: Option<String>,
}

/// The server's last check, for Admins.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub checked_at: Option<DateTime<Utc>>,
    /// `serverInfo` as the server declared it: shown, never trusted as identity.
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    /// `ok`, or why the last connection failed.
    pub outcome: Option<String>,
    pub tools: Vec<ToolCheck>,
    /// Every tool the server listed at the last check, by name (cleaned: the names are the
    /// server's), with its digest: for an Admin preparing an approval. Never shown to a bot.
    pub listed: Vec<(String, String)>,
    pub refused_requests: u64,
    pub dropped_notifications: u64,
}

struct Conn {
    client: Client,
    /// The token it was opened with, hashed: a changed token opens a new connection.
    token: Option<String>,
    listed_at: Mutex<Instant>,
    last_used: Mutex<Instant>,
}

enum Attempt {
    /// Couldn't connect, or the connection broke: a read may try once more.
    Transport(Option<Arc<Conn>>, String),
    Final(ConnectorError),
}

/// One server from its record.
pub struct McpServer {
    record: Arc<ServerRecord>,
    http: reqwest::Client,
    /// Where `tools/call` goes: the record's URL (a test build may route it to a local fake).
    uri: String,
    auth: ServerAuth,
    conns: tokio::sync::Mutex<HashMap<String, Arc<Conn>>>,
    /// Approved tools that changed or went away, with why: never usable again in this process.
    revoked: Mutex<BTreeMap<String, &'static str>>,
    in_flight: Arc<Semaphore>,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    limits: Limits,
    events: Arc<Events>,
    status: Mutex<Status>,
}

/// The answer to a call of a revoked tool (threat 2).
pub fn tool_changed(server: &str, upstream: &str) -> ConnectorError {
    ConnectorError::Forbidden(format!(
        "tool_changed: {server}.{upstream}: this tool changed at its server and needs an Admin's review"
    ))
}

impl McpServer {
    pub fn new(
        record: ServerRecord,
        auth: ServerAuth,
        audit: Option<Arc<dyn AuditSink>>,
        routes: &TestRoutes,
    ) -> Result<Self, String> {
        match (&auth, record.auth) {
            (ServerAuth::Token(_), AuthKind::ServiceToken)
            | (ServerAuth::None, AuthKind::None)
            | (ServerAuth::PerUser(_), AuthKind::OauthPerUser) => {}
            _ => {
                return Err(format!(
                    "`{}`: the credentials given don't match its auth `{}`",
                    record.id,
                    record.auth.as_str()
                ))
            }
        }
        let http = http_client(&record, routes)?;
        let limits = Limits::default();
        let events = Events::new(&record.id, audit);
        let status = Status {
            tools: approved_tools(&record)
                .map(|(upstream, digest)| ToolCheck {
                    upstream: upstream.to_string(),
                    approved_digest: digest.to_string(),
                    state: "unchecked",
                    listed_digest: None,
                })
                .collect(),
            ..Default::default()
        };
        Ok(Self {
            uri: routes.rewrite(&record.url),
            record: Arc::new(record),
            http,
            auth,
            conns: Default::default(),
            revoked: Default::default(),
            in_flight: Arc::new(Semaphore::new(limits.in_flight)),
            rate: Default::default(),
            limits,
            events,
            status: Mutex::new(status),
        })
    }

    /// Other caps (tests).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.in_flight = Arc::new(Semaphore::new(limits.in_flight));
        self.limits = limits;
        self
    }

    pub fn record(&self) -> &ServerRecord {
        &self.record
    }

    pub fn events(&self) -> &Arc<Events> {
        &self.events
    }

    pub fn status(&self) -> Status {
        let mut s = self.status.lock().expect("status lock").clone();
        s.refused_requests = self.events.refused.load(Ordering::Relaxed);
        s.dropped_notifications = self.events.dropped.load(Ordering::Relaxed);
        s
    }

    /// Is `upstream` revoked, and why?
    pub fn revoked(&self, upstream: &str) -> Option<&'static str> {
        self.revoked
            .lock()
            .expect("revoked lock")
            .get(upstream)
            .copied()
    }

    /// Connect and list now (readiness, Policy Admin, preflight). Needs no user for
    /// `service_token` and `none`.
    pub async fn check(&self) -> Result<(), String> {
        if matches!(self.auth, ServerAuth::PerUser(_)) {
            return Ok(());
        }
        let r = tokio::time::timeout(self.limits.read, self.conn("")).await;
        match r {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(Attempt::Transport(_, e))) => Err(e),
            Ok(Err(Attempt::Final(e))) => Err(e.to_string()),
            Err(_) => Err(format!(
                "`{}` didn't answer within {} s",
                self.record.id,
                self.limits.read.as_secs()
            )),
        }
    }

    fn over_rate(&self, reader: &str) -> Option<u64> {
        let now = Instant::now();
        let mut rate = self.rate.lock().expect("rate lock");
        if rate.len() > 10_000 {
            rate.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60))
            });
        }
        let q = rate.entry(reader.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= self.limits.per_user_per_minute {
            let wait = q
                .front()
                .map(|t| 60 - now.duration_since(*t).as_secs().min(59))
                .unwrap_or(60);
            return Some(wait);
        }
        q.push_back(now);
        None
    }

    /// Call the upstream tool `mapping` approves, for `reader`, with `args` (already mapped).
    pub async fn call(
        &self,
        reader: &str,
        mapping: &ToolMapping,
        args: Map<String, Value>,
        kind: CallKind,
    ) -> Result<Value, ConnectorError> {
        self.call_upstream(
            reader,
            &mapping.upstream,
            &mapping.digest,
            &mapping.errors,
            args,
            kind,
        )
        .await
    }

    /// Call `upstream`, approved with `digest`.
    pub async fn call_upstream(
        &self,
        reader: &str,
        upstream: &str,
        digest: &str,
        errors: &pmos_contract::mcp::ErrorMap,
        args: Map<String, Value>,
        kind: CallKind,
    ) -> Result<Value, ConnectorError> {
        if self.revoked(upstream).is_some() {
            return Err(tool_changed(&self.record.id, upstream));
        }
        if let Some(wait) = self.over_rate(reader) {
            return Err(ConnectorError::RateLimited(wait));
        }
        let _permit = self
            .in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| ConnectorError::RateLimited(1))?;
        let (deadline, attempts) = match kind {
            CallKind::Read => (self.limits.read, 2),
            CallKind::Write => (self.limits.write, 1),
        };
        for attempt in 0..attempts {
            let result = tokio::time::timeout(
                deadline,
                self.attempt(reader, upstream, digest, errors, &args),
            )
            .await;
            match result {
                Err(_) => {
                    return Err(ConnectorError::Unavailable(match kind {
                        CallKind::Read => format!(
                            "{} didn't answer within {} s",
                            self.record.id,
                            deadline.as_secs()
                        ),
                        CallKind::Write => format!(
                            "{} didn't answer within {} s; the write may or may not have happened",
                            self.record.id,
                            deadline.as_secs()
                        ),
                    }))
                }
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(Attempt::Final(e))) => return Err(e),
                Ok(Err(Attempt::Transport(conn, e))) => {
                    if let Some(conn) = conn {
                        self.forget(reader, &conn).await;
                    }
                    tracing::warn!(server = %self.record.id, error = %e, "MCP call failed in transport");
                    if attempt + 1 == attempts {
                        return Err(ConnectorError::Unavailable(match kind {
                            CallKind::Read => format!("{}: {e}", self.record.id),
                            CallKind::Write => format!(
                                "{}: {e}; the write may or may not have happened",
                                self.record.id
                            ),
                        }));
                    }
                }
            }
        }
        unreachable!("the loop returns on its last attempt")
    }

    async fn attempt(
        &self,
        reader: &str,
        upstream: &str,
        digest: &str,
        errors: &pmos_contract::mcp::ErrorMap,
        args: &Map<String, Value>,
    ) -> Result<Value, Attempt> {
        let conn = self.conn(reader).await?;
        let stale =
            conn.listed_at.lock().expect("listed lock").elapsed() >= self.limits.relist_every;
        if stale || self.events.list_changed.swap(false, Ordering::SeqCst) {
            self.relist(&conn).await?;
        }
        // Checked again after any re-list, and against the approved digest itself.
        if self.revoked(upstream).is_some() || !self.approves(upstream, digest) {
            return Err(Attempt::Final(tool_changed(&self.record.id, upstream)));
        }
        *conn.last_used.lock().expect("used lock") = Instant::now();
        let sent = conn
            .client
            .call_tool_once(
                CallToolRequestParams::new(upstream.to_string()).with_arguments(args.clone()),
            )
            .await;
        match sent {
            Ok(CallToolResponse::Complete(result)) => {
                self.result(errors, result).map_err(Attempt::Final)
            }
            Ok(CallToolResponse::InputRequired(_)) => {
                self.events.refuse("input_required").await;
                Err(Attempt::Final(ConnectorError::Unavailable(format!(
                    "{} asked for input PM OS never gives (sampling or elicitation); the call stopped there",
                    self.record.id
                ))))
            }
            Ok(_) => {
                self.events.refuse("tasks").await;
                Err(Attempt::Final(ConnectorError::Unavailable(format!(
                    "{} answered with a task, which PM OS doesn't follow",
                    self.record.id
                ))))
            }
            Err(e) => Err(Attempt::Transport(Some(conn), e.to_string())),
        }
    }

    /// Does the record approve `upstream` with exactly `digest`?
    fn approves(&self, upstream: &str, digest: &str) -> bool {
        approved_tools(&self.record).any(|(u, d)| u == upstream && d == digest)
    }

    fn result(
        &self,
        errors: &pmos_contract::mcp::ErrorMap,
        result: CallToolResult,
    ) -> Result<Value, ConnectorError> {
        let text: String = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        let structured = result.structured_content.clone().unwrap_or(Value::Null);
        let size = serde_json::to_vec(&structured).map_or(0, |v| v.len()) + text.len();
        if size > self.limits.max_response_bytes {
            return Err(ConnectorError::Unavailable(format!(
                "{}'s answer is larger than {} KiB",
                self.record.id,
                self.limits.max_response_bytes / 1024
            )));
        }
        if result.is_error == Some(true) {
            return Err(mapping::failure(errors, &structured, &text));
        }
        match structured {
            Value::Null => match serde_json::from_str::<Value>(&text) {
                Ok(v @ Value::Object(_)) => Ok(v),
                _ => Ok(serde_json::json!({ "text": text })),
            },
            v => Ok(v),
        }
    }

    fn conn_key(reader: &str, auth: &ServerAuth) -> String {
        match auth {
            ServerAuth::PerUser(_) => reader.to_string(),
            _ => String::new(),
        }
    }

    async fn conn(&self, reader: &str) -> Result<Arc<Conn>, Attempt> {
        let key = Self::conn_key(reader, &self.auth);
        let token = match &self.auth {
            ServerAuth::Token(t) => Some(t.clone()),
            ServerAuth::None => None,
            ServerAuth::PerUser(tokens) => Some(
                tokens
                    .token(reader, &self.record.id)
                    .await
                    .map_err(Attempt::Final)?,
            ),
        };
        let token_hash = token
            .as_ref()
            .map(|t| crate::approvals::args_hash(&Value::String(t.expose().to_string())));
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&key) {
            let fresh = !c.client.is_closed()
                && c.token == token_hash
                && c.last_used.lock().expect("used lock").elapsed() < self.limits.idle;
            if fresh {
                return Ok(c.clone());
            }
            conns.remove(&key);
        }
        let conn = Arc::new(self.connect(token.as_ref(), token_hash).await?);
        conns.insert(key, conn.clone());
        if conns.len() > 10_000 {
            conns
                .retain(|_, c| c.last_used.lock().expect("used lock").elapsed() < self.limits.idle);
        }
        Ok(conn)
    }

    async fn forget(&self, reader: &str, broken: &Arc<Conn>) {
        let key = Self::conn_key(reader, &self.auth);
        let mut conns = self.conns.lock().await;
        if conns.get(&key).is_some_and(|c| Arc::ptr_eq(c, broken)) {
            conns.remove(&key);
        }
    }

    async fn connect(
        &self,
        token: Option<&Secret>,
        token_hash: Option<String>,
    ) -> Result<Conn, Attempt> {
        let mut config = StreamableHttpClientTransportConfig::with_uri(self.uri.clone())
            .max_sse_event_size(self.limits.max_response_bytes);
        if let Some(t) = token {
            config = config.auth_header(t.expose().to_string());
        }
        let transport = StreamableHttpClientTransport::with_client(self.http.clone(), config);
        let handler = RefusingHandler::new(self.events.clone());
        let client = match handler.serve(transport).await {
            Ok(c) => c,
            Err(e) => {
                // The error is rmcp's, about the transport: never a token.
                let why = format!("couldn't connect: {e}");
                self.events
                    .record(
                        CONNECT_EVENT,
                        Outcome::Error,
                        format!("server {} connection failed", self.record.id),
                    )
                    .await;
                self.status.lock().expect("status lock").outcome = Some(why.clone());
                return Err(Attempt::Transport(None, why));
            }
        };
        let info = client.peer_info().and_then(|i| i.server_info.clone());
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
                .take(64)
                .collect()
        };
        let (name, version) = info
            .map(|i| (clean(&i.name), clean(&i.version)))
            .unwrap_or_default();
        self.events
            .record(
                CONNECT_EVENT,
                Outcome::Ok,
                format!(
                    "server {} connected; it calls itself {name} {version}",
                    self.record.id
                ),
            )
            .await;
        {
            let mut s = self.status.lock().expect("status lock");
            s.server_name = Some(name);
            s.server_version = Some(version);
        }
        let conn = Conn {
            client,
            token: token_hash,
            listed_at: Mutex::new(Instant::now()),
            last_used: Mutex::new(Instant::now()),
        };
        self.relist(&conn).await?;
        Ok(conn)
    }

    /// `tools/list`, then revoke every approved tool that changed or is gone.
    async fn relist(&self, conn: &Conn) -> Result<(), Attempt> {
        let listed = conn
            .client
            .list_all_tools()
            .await
            .map_err(|e| Attempt::Transport(None, format!("tools/list failed: {e}")))?;
        let digests: HashMap<String, String> = listed
            .iter()
            .take(mapping::MAX_ITEMS)
            .map(|t| (t.name.to_string(), super::digest::digest(t)))
            .collect();
        *conn.listed_at.lock().expect("listed lock") = Instant::now();
        let mut newly = Vec::new();
        let mut checks = Vec::new();
        {
            let mut revoked = self.revoked.lock().expect("revoked lock");
            for (upstream, approved) in approved_tools(&self.record) {
                let state = match (revoked.get(upstream), digests.get(upstream)) {
                    (Some(why), _) => *why,
                    (None, Some(d)) if d == approved => "approved",
                    (None, Some(_)) => {
                        revoked.insert(upstream.to_string(), "changed");
                        newly.push((upstream.to_string(), "changed"));
                        "changed"
                    }
                    (None, None) => {
                        revoked.insert(upstream.to_string(), "missing");
                        newly.push((upstream.to_string(), "missing"));
                        "missing"
                    }
                };
                checks.push(ToolCheck {
                    upstream: upstream.to_string(),
                    approved_digest: approved.to_string(),
                    state,
                    listed_digest: digests.get(upstream).cloned(),
                });
            }
        }
        {
            let mut s = self.status.lock().expect("status lock");
            s.checked_at = Some(Utc::now());
            s.outcome = Some("ok".into());
            s.tools = checks;
            let mut listed: Vec<(String, String)> = digests
                .iter()
                .map(|(name, d)| {
                    let clean: String = name
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
                        .take(128)
                        .collect();
                    (clean, d.clone())
                })
                .collect();
            listed.sort();
            s.listed = listed;
        }
        for (upstream, why) in newly {
            tracing::error!(server = %self.record.id, tool = %upstream, why, "an approved MCP tool changed at its server; it is off until an Admin approves it again");
            self.events
                .record(
                    CHANGED_EVENT,
                    Outcome::Denied,
                    format!(
                        "tool_changed: {}.{upstream}: {why} since its approval",
                        self.record.id
                    ),
                )
                .await;
        }
        Ok(())
    }
}

/// Every upstream tool the record approves, with its digest: the mapped tools and the identity
/// check.
pub fn approved_tools(record: &ServerRecord) -> impl Iterator<Item = (&str, &str)> {
    record
        .tools
        .iter()
        .map(|t| (t.upstream.as_str(), t.digest.as_str()))
        .chain(
            record
                .identity_check
                .iter()
                .map(|c| (c.tool.as_str(), c.digest.as_str())),
        )
}
