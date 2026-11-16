//! The console's two upstreams: the bot runtime (`pmos-bots`, as client `console`) and the
//! tool server (approvals, with the console's own tool server token). Shapes follow
//! `docs/bot-runtime-spec.md` §3 and `docs/toolserver.md`; fields the console doesn't show
//! are ignored, and lists may come bare (`[…]`) or wrapped (`{"items": […]}`).
//!
//! With a key for an upstream (`PMOS_ASSERTION_KEYS`), every call that names a person (one
//! PM's tasks, routines or approvals) also carries a signed user assertion for that person
//! (`X-Pmos-Assertion`, `docs/user-assertion.md`): issuer `console`, valid for 60 seconds,
//! bound to the method, path, query and body sent, signed afresh for every call. Reads of
//! everything, and routine changes (which name no one), carry none.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pmos_assertion::{Claims, KeyRing, RequestBinding};
use pmos_contract::model::TeamFile;
use reqwest::header::{HeaderName, HeaderValue};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

/// How long the console waits for an upstream before showing an error page.
pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// The upstream answered `404`.
    NotFound,
    /// Connection failure, timeout, an unexpected status, or a body that didn't parse.
    Failed { service: &'static str, why: String },
}

pub type UpstreamResult<T> = Result<T, UpstreamError>;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum List<T> {
    Bare(Vec<T>),
    Wrapped { items: Vec<T> },
}

impl<T> List<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::Bare(v) | Self::Wrapped { items: v } => v,
        }
    }
}

/// One bot in the roster (`GET /v1/bots`).
#[derive(Debug, Clone, Deserialize)]
pub struct BotSummary {
    pub name: String,
    pub role: String,
    /// `team` or `personal`.
    pub scope: String,
    pub owner: String,
    #[serde(default)]
    pub description: String,
}

/// What a team bot may read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Reads {
    #[serde(default, alias = "jira-projects")]
    pub jira_projects: Vec<String>,
    #[serde(default, alias = "confluence-spaces")]
    pub confluence_spaces: Vec<String>,
    #[serde(default, alias = "slack-channels")]
    pub slack_channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RoleRef {
    Name(String),
    Object { name: String },
}

#[derive(Debug, Clone, Deserialize)]
struct ActionEntry {
    tool: String,
    setting: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Actions {
    Map(BTreeMap<String, String>),
    List(Vec<ActionEntry>),
}

#[derive(Debug, Clone, Deserialize)]
struct BotDetailWire {
    #[serde(default)]
    role: Option<RoleRef>,
    #[serde(default)]
    reads: Option<Reads>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default, alias = "action_settings", alias = "settings")]
    actions: Option<Actions>,
}

/// One bot in full (`GET /v1/bots/{name}`): its tools and each write tool's setting.
#[derive(Debug, Clone, Default)]
pub struct BotDetail {
    pub role: Option<String>,
    pub reads: Option<Reads>,
    pub tools: Vec<String>,
    /// Write tool → `approve` or `never`.
    pub actions: BTreeMap<String, String>,
}

impl From<BotDetailWire> for BotDetail {
    fn from(w: BotDetailWire) -> Self {
        Self {
            role: w.role.map(|r| match r {
                RoleRef::Name(n) | RoleRef::Object { name: n } => n,
            }),
            reads: w.reads,
            tools: w.tools,
            actions: match w.actions {
                None => BTreeMap::new(),
                Some(Actions::Map(m)) => m,
                Some(Actions::List(l)) => l.into_iter().map(|e| (e.tool, e.setting)).collect(),
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChecklistItem {
    pub item: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackRef {
    pub channel: String,
    pub thread_ts: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub bot: String,
    pub requested_by: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub text: String,
    pub status: String,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub approvals: Vec<String>,
    #[serde(default)]
    pub slack: Option<SlackRef>,
    #[serde(default)]
    pub routine_id: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskEvent {
    pub at: String,
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub checklist: Option<Vec<ChecklistItem>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TraceEntry {
    pub at: String,
    pub kind: String,
    #[serde(default)]
    pub body: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    pub cron: String,
    #[serde(default)]
    pub tz: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Routine {
    pub id: String,
    pub bot: String,
    pub owner: String,
    pub enabled: bool,
    pub schedule: Schedule,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
}

/// A pending write in a PM's inbox (`GET /internal/approvals`).
#[derive(Debug, Clone, Deserialize)]
pub struct Approval {
    pub id: String,
    pub user: String,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub bot: Option<String>,
    #[serde(default)]
    pub run_id: String,
    pub tool: String,
    #[serde(default)]
    pub args: Value,
    pub status: String,
    #[serde(default)]
    pub created_at: String,
}

/// How long an assertion lives. The receiver allows a few seconds of clock skew on top.
pub const ASSERTION_LIFETIME_SECS: u64 = 60;

/// The `sid` claim. The console's upstream calls are made with a handle only, not the
/// viewer's session, so every assertion carries this fixed value.
pub const ASSERTION_SESSION: &str = "unknown";

/// Signs user assertions for one upstream (an audience) with that pair's key ring.
pub struct Signer {
    issuer: &'static str,
    audience: String,
    ring: KeyRing,
    /// Where `team.yaml` is: the subject's roles are read from it at each call.
    content_root: PathBuf,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("key_id", &self.ring.current_id())
            .finish()
    }
}

impl Signer {
    /// As `issuer` (the console is `console`) for `audience` (`bots` or `toolserver`).
    pub fn new(issuer: &'static str, audience: &str, ring: KeyRing, content_root: PathBuf) -> Self {
        Self {
            issuer,
            audience: audience.to_string(),
            ring,
            content_root,
        }
    }

    /// The subject's known roles in `team.yaml`; none if the file or the member is missing.
    /// Receivers check roles against `team.yaml` themselves: these are informational.
    fn roles(&self, subject: &str) -> Vec<pmos_contract::model::Role> {
        TeamFile::load(&self.content_root)
            .ok()
            .and_then(|t| t.by_handle(subject).map(|m| m.roles.clone()))
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.is_known())
            .collect()
    }

    /// An assertion that `subject` is the person `request` is for, bound to it exactly as it
    /// will be sent.
    pub fn sign(
        &self,
        subject: &str,
        request: &reqwest::Request,
    ) -> Result<HeaderValue, &'static str> {
        let url = request.url();
        let target = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let body: &[u8] = match request.body() {
            None => b"",
            Some(b) => b.as_bytes().ok_or("can't sign a streamed body")?,
        };
        let binding = RequestBinding::new(request.method().as_str(), &target, body)
            .map_err(|_| "can't bind this request")?;
        let claims = Claims::new(
            self.issuer,
            &self.audience,
            subject,
            &self.roles(subject),
            ASSERTION_SESSION,
        )
        .map_err(|_| "can't sign for this person")?;
        let value = self
            .ring
            .sign(
                &claims,
                pmos_assertion::unix_now(),
                ASSERTION_LIFETIME_SECS,
                Some(&binding),
            )
            .map_err(|_| "couldn't sign the user assertion")?;
        HeaderValue::from_str(&value).map_err(|_| "couldn't sign the user assertion")
    }
}

/// What the console presents to one upstream: its client token and, when it has a key for
/// that upstream, the [`Signer`] for its user assertions. `Debug` never shows the token.
#[derive(Clone)]
pub struct Credentials {
    token: String,
    signer: Option<Arc<Signer>>,
}

impl Credentials {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            signer: None,
        }
    }

    /// Also sign a user assertion on every call that names a person.
    pub fn with_signer(mut self, signer: Signer) -> Self {
        self.signer = Some(Arc::new(signer));
        self
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn signs(&self) -> bool {
        self.signer.is_some()
    }
}

impl From<&str> for Credentials {
    fn from(token: &str) -> Self {
        Self::new(token)
    }
}

impl From<String> for Credentials {
    fn from(token: String) -> Self {
        Self::new(token)
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &"<redacted>")
            .field("signer", &self.signer)
            .finish()
    }
}

/// An HTTP client for one upstream.
#[derive(Clone)]
pub struct Upstream {
    http: reqwest::Client,
    base: String,
    token: String,
    signer: Option<Arc<Signer>>,
    service: &'static str,
}

impl Upstream {
    pub fn new(
        http: reqwest::Client,
        base: &str,
        credentials: &Credentials,
        service: &'static str,
    ) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: credentials.token.clone(),
            signer: credentials.signer.clone(),
            service,
        }
    }

    /// `base` plus path segments, each percent-encoded, so an id can't change the path.
    fn url(&self, segments: &[&str]) -> UpstreamResult<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base).map_err(|e| self.failed(e))?;
        url.path_segments_mut()
            .map_err(|_| self.failed("the base URL can't take a path"))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    fn failed(&self, why: impl std::fmt::Display) -> UpstreamError {
        UpstreamError::Failed {
            service: self.service,
            why: why.to_string(),
        }
    }

    /// Send `req`. When it names a person (`subject`) and there is a key for this upstream, a
    /// fresh user assertion is signed over the request as built, and added to it.
    async fn send(
        &self,
        req: reqwest::RequestBuilder,
        subject: Option<&str>,
    ) -> UpstreamResult<reqwest::Response> {
        let mut request = req
            .bearer_auth(&self.token)
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| self.failed("couldn't build the request"))?;
        if let (Some(signer), Some(subject)) = (&self.signer, subject) {
            let value = signer
                .sign(subject, &request)
                .map_err(|why| self.failed(why))?;
            request
                .headers_mut()
                .insert(HeaderName::from_static(pmos_assertion::HEADER), value);
        }
        let res = self.http.execute(request).await.map_err(|e| {
            self.failed(if e.is_timeout() {
                "timed out".to_string()
            } else {
                "didn't answer".to_string()
            })
        })?;
        match res.status() {
            s if s.is_success() => Ok(res),
            reqwest::StatusCode::NOT_FOUND => Err(UpstreamError::NotFound),
            s => Err(self.failed(format!("answered {s}"))),
        }
    }

    /// `GET`, with an assertion for `subject` when the query names that person.
    async fn get<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        subject: Option<&str>,
    ) -> UpstreamResult<T> {
        let url = self.url(segments)?;
        let res = self.send(self.http.get(url).query(query), subject).await?;
        res.json::<T>()
            .await
            .map_err(|e| self.failed(format!("sent a reply the console can't read: {e}")))
    }

    async fn list<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        subject: Option<&str>,
    ) -> UpstreamResult<Vec<T>> {
        Ok(self
            .get::<List<T>>(segments, query, subject)
            .await?
            .into_vec())
    }

    // The bot runtime.

    pub async fn bots(&self) -> UpstreamResult<Vec<BotSummary>> {
        self.list(&["v1", "bots"], &[], None).await
    }

    pub async fn bot(&self, name: &str) -> UpstreamResult<BotDetail> {
        Ok(self
            .get::<BotDetailWire>(&["v1", "bots", name], &[], None)
            .await?
            .into())
    }

    pub async fn tasks(&self, requested_by: Option<&str>, limit: u32) -> UpstreamResult<Vec<Task>> {
        let limit = limit.to_string();
        let mut query = vec![("limit", limit.as_str())];
        if let Some(who) = requested_by {
            query.push(("requested_by", who));
        }
        // One PM's tasks name them; all tasks name no one.
        self.list(&["v1", "tasks"], &query, requested_by).await
    }

    pub async fn task(&self, id: &str) -> UpstreamResult<Task> {
        self.get(&["v1", "tasks", id], &[], None).await
    }

    pub async fn task_events(&self, id: &str) -> UpstreamResult<Vec<TaskEvent>> {
        self.list(&["v1", "tasks", id, "events"], &[], None).await
    }

    pub async fn task_trace(&self, id: &str) -> UpstreamResult<Vec<TraceEntry>> {
        self.list(&["v1", "tasks", id, "trace"], &[], None).await
    }

    pub async fn routines(&self, owner: Option<&str>) -> UpstreamResult<Vec<Routine>> {
        let query: Vec<(&str, &str)> = owner.map(|o| ("owner", o)).into_iter().collect();
        self.list(&["v1", "routines"], &query, owner).await
    }

    pub async fn set_routine_enabled(&self, id: &str, enabled: bool) -> UpstreamResult<()> {
        let url = self.url(&["v1", "routines", id])?;
        // Names no one: the console's own power (docs/console.md).
        self.send(
            self.http
                .patch(url)
                .json(&serde_json::json!({ "enabled": enabled })),
            None,
        )
        .await
        .map(drop)
    }

    pub async fn delete_routine(&self, id: &str) -> UpstreamResult<()> {
        let url = self.url(&["v1", "routines", id])?;
        self.send(self.http.delete(url), None).await.map(drop)
    }

    // The tool server.

    /// A PM's pending approvals, newest first, with an assertion for that PM.
    pub async fn pending_approvals(&self, user: &str) -> UpstreamResult<Vec<Approval>> {
        self.list(
            &["internal", "approvals"],
            &[("user", user), ("status", "pending")],
            Some(user),
        )
        .await
    }
}
