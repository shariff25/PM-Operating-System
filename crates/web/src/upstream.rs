//! The two upstreams: the bot runtime (`pmos-bots`, as client `web`, every call naming the
//! PM in `X-Pmos-User`) and the tool server (approvals, with pmos-web's own tool server
//! token). Shapes follow `docs/bot-runtime-spec.md` §3 and `docs/toolserver.md`; fields not
//! used here are ignored, and lists may come bare (`[…]`) or wrapped (`{"items": […]}`).
//!
//! With a key for an upstream (`PMOS_ASSERTION_KEYS`), every call made for a PM also carries a
//! signed user assertion (`X-Pmos-Assertion`, `docs/user-assertion.md`): issuer `web`, that
//! upstream as audience, the PM as subject, valid for 60 seconds, bound to the method, path,
//! query and body actually sent, and signed afresh for every call. Its `sid` is the PM's
//! session reference (`session::assertion_ref`, never the session id itself) when the call is
//! made through [`Runtime::for_session`] or [`ToolServer::for_session`], else
//! [`ASSERTION_SESSION`].
//!
//! Tokens are only ever sent as `Authorization` headers to the configured base URLs. Error
//! values carry a plain reason, never a token, a key or an upstream body.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pmos_assertion::{Claims, KeyRing, RequestBinding};
use reqwest::header::{HeaderName, HeaderValue};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How long to wait for an upstream.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The header naming the PM a runtime call is for.
pub const USER_HEADER: &str = "X-Pmos-User";

#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamError {
    /// The upstream answered `404`.
    NotFound,
    /// The upstream answered with another client error (or `503`), and what it said: `code`
    /// and `error` when the body had them, and for a Slack audience refusal what it counted.
    /// `error` is the upstream's own sentence: it is never shown to the browser for a
    /// decision (`app.rs`, `refused_decision`).
    Refused {
        service: &'static str,
        status: u16,
        code: Option<String>,
        error: Option<String>,
        /// `members`, `outsiders` and `external` of a `needs_external_confirm` or
        /// `audience_blocked` refusal; `None` otherwise.
        audience: Option<RefusedAudience>,
    },
    /// The request never reached the upstream (it couldn't be built, signed or connected),
    /// so nothing was done there.
    Failed { service: &'static str, why: String },
    /// The request was sent and may have been acted on, but no usable answer came back: it
    /// timed out, the connection dropped, the upstream failed with a `5xx` other than `503`,
    /// or its success answer couldn't be read. For a decision, the outcome is unknown.
    Unknown { service: &'static str, why: String },
}

/// What a tool server refusal said of a Slack channel's audience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefusedAudience {
    pub members: Option<u64>,
    pub outsiders: Option<u64>,
    pub external: Option<bool>,
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

/// A runtime task (spec §3).
#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub bot: String,
    pub requested_by: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub text: String,
    pub status: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub approvals: Vec<String>,
    /// The worker's latest checklist (spec §3, `[{item, done}]`), for `progress`. Kept as
    /// JSON so a shape this build doesn't know can't make the task unreadable.
    #[serde(default)]
    pub checklist: Value,
    /// How many times a worker has claimed the task; above 1 it was retried.
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
}

/// A tool server approval item (`GET /internal/approvals/{id}`).
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
    #[serde(default)]
    pub decided_at: Option<String>,
    #[serde(default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    /// When a Slack draft expires, as the tool server's own setting has it.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// A link to where the write goes, when the tool server could build one from ids.
    #[serde(default)]
    pub target_url: Option<String>,
    /// For a Slack draft whose audience the tool server has checked: whether the channel is
    /// shared outside the company.
    #[serde(default)]
    pub external: Option<bool>,
    /// Whether a person changed the bot's text: a saved edit (`PUT .../draft`) or an edit
    /// decision. `args` are then theirs. An older tool server doesn't send it.
    #[serde(default)]
    pub edited: bool,
    /// When the PM last saved an edit of this pending draft.
    #[serde(default)]
    pub edited_at: Option<String>,
    /// The highest data class the drafting session had read: `public`, `internal` or
    /// `restricted`. Absent for items made before it was recorded, which count as restricted.
    #[serde(default)]
    pub sources_sensitivity: Option<String>,
}

/// One source of `GET /internal/approvals/{id}/sources`.
#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalSource {
    pub system: String,
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "ref")]
    pub reference: String,
    /// The ref the bot's source markers name this source by (`src-3f9a1c`). A tool server
    /// from before markers doesn't send it: its drafts have no citations.
    #[serde(default)]
    pub cite: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SourcesReply {
    #[serde(default)]
    sources: Vec<ApprovalSource>,
}

/// A decision for `POST /internal/approvals/{id}/decision`.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionBody {
    pub decided_by: String,
    /// `approve`, `edit` or `reject`.
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub confirm_external: bool,
}

/// How long an assertion lives. The receiver allows a few seconds of clock skew on top.
pub const ASSERTION_LIFETIME_SECS: u64 = 60;

/// The `sid` claim of a call made without a session reference (none is made by a request
/// handler: each passes the PM's, see [`Runtime::for_session`]).
pub const ASSERTION_SESSION: &str = "unknown";

/// Signs user assertions for one upstream (an audience) with that pair's key ring.
pub struct Signer {
    issuer: &'static str,
    audience: String,
    ring: KeyRing,
    /// `team.yaml`: the subject's roles at each call, from the parsed file (re-read when it
    /// changes, `team.rs`).
    team: crate::team::TeamCache,
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
    /// As `issuer` (pmos-web is `web`) for `audience` (`bots` or `toolserver`).
    pub fn new(issuer: &'static str, audience: &str, ring: KeyRing, content_root: PathBuf) -> Self {
        Self {
            issuer,
            audience: audience.to_string(),
            ring,
            team: crate::team::TeamCache::new(content_root),
        }
    }

    /// The subject's known roles in `team.yaml`; none if the file or the member is missing.
    /// Receivers check roles against `team.yaml` themselves: these are informational.
    fn roles(&self, subject: &str) -> Vec<pmos_contract::model::Role> {
        self.team
            .load()
            .ok()
            .and_then(|t| t.by_handle(subject).map(|m| m.roles.clone()))
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.is_known())
            .collect()
    }

    /// An assertion that `subject` is the person `request` is for, in the session `session`
    /// refers to (the `sid` claim), bound to the request exactly as it will be sent.
    pub fn sign(
        &self,
        subject: &str,
        session: &str,
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
            session,
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

/// What pmos-web presents to one upstream: its client token and, when it has a key for that
/// upstream, the [`Signer`] for its user assertions. `Debug` never shows the token.
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

    /// Also sign a user assertion on every call made for a PM.
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
struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    signer: Option<Arc<Signer>>,
    /// The `sid` for assertions: the PM's session reference, when the call is for one.
    session: Option<String>,
    service: &'static str,
}

impl Client {
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

    /// Send `req`. When the call is for a PM (`subject`) and there is a key for this upstream,
    /// a fresh user assertion is signed over the request as built, and added to it.
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
            let session = self.session.as_deref().unwrap_or(ASSERTION_SESSION);
            let value = signer
                .sign(subject, session, &request)
                .map_err(|why| self.failed(why))?;
            request
                .headers_mut()
                .insert(HeaderName::from_static(pmos_assertion::HEADER), value);
        }
        let res = self.http.execute(request).await.map_err(|e| {
            // Refused or unreachable before anything was sent: nothing was done there.
            // Anything later (a timeout waiting for the answer, a dropped connection) may
            // have been acted on.
            if e.is_connect() || e.is_builder() {
                self.failed("didn't answer")
            } else if e.is_timeout() {
                self.unknown("timed out")
            } else {
                self.unknown("dropped the connection")
            }
        })?;
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(UpstreamError::NotFound);
        }
        if status.is_client_error() || status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            let body: Value = res.json().await.unwrap_or(Value::Null);
            let text = |k: &str| body.get(k).and_then(Value::as_str).map(String::from);
            let counted = ["members", "outsiders", "external"]
                .iter()
                .any(|k| body.get(k).is_some());
            let audience = counted.then(|| RefusedAudience {
                members: body.get("members").and_then(Value::as_u64),
                outsiders: body.get("outsiders").and_then(Value::as_u64),
                external: body.get("external").and_then(Value::as_bool),
            });
            return Err(UpstreamError::Refused {
                service: self.service,
                status: status.as_u16(),
                code: text("code"),
                error: text("error"),
                audience,
            });
        }
        Err(self.unknown(format!("answered {status}")))
    }

    fn unknown(&self, why: impl std::fmt::Display) -> UpstreamError {
        UpstreamError::Unknown {
            service: self.service,
            why: why.to_string(),
        }
    }

    /// `GET <base>/healthz` answers `2xx` within two seconds. No token or person is sent.
    async fn healthy(&self) -> bool {
        let Ok(url) = self.url(&["healthz"]) else {
            return false;
        };
        self.http
            .get(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// A success answer's body. One that can't be read is [`UpstreamError::Unknown`]: the
    /// upstream did what was asked, but what it answered isn't known here.
    async fn read<T: DeserializeOwned>(&self, res: reqwest::Response) -> UpstreamResult<T> {
        res.json::<T>()
            .await
            .map_err(|_| self.unknown("sent a reply pmos-web can't read"))
    }
}

/// The bot runtime, as client `web`.
#[derive(Clone)]
pub struct Runtime(Client);

impl Runtime {
    pub fn new(http: reqwest::Client, base: &str, credentials: &Credentials) -> Self {
        Self(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: credentials.token.clone(),
            signer: credentials.signer.clone(),
            session: None,
            service: "bot runtime",
        })
    }

    /// The same upstream, with `session_ref` (`session::assertion_ref`, never the session id)
    /// as the `sid` of every assertion it signs.
    pub fn for_session(&self, session_ref: &str) -> Self {
        let mut c = self.0.clone();
        c.session = Some(session_ref.to_string());
        Self(c)
    }

    async fn get<T: DeserializeOwned>(
        &self,
        user: &str,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> UpstreamResult<T> {
        let url = self.0.url(segments)?;
        let res = self
            .0
            .send(
                self.0.http.get(url).query(query).header(USER_HEADER, user),
                Some(user),
            )
            .await?;
        self.0.read(res).await
    }

    /// Is the runtime up (`GET /healthz`, no token, no person)? For `/readyz`.
    pub async fn healthy(&self) -> bool {
        self.0.healthy().await
    }

    /// `POST /v1/tasks` for `user`, with `source: "web"`.
    pub async fn create_task(&self, user: &str, bot: &str, text: &str) -> UpstreamResult<Task> {
        let url = self.0.url(&["v1", "tasks"])?;
        let res = self
            .0
            .send(
                self.0
                    .http
                    .post(url)
                    .header(USER_HEADER, user)
                    .json(&json!({
                        "bot": bot, "requested_by": user, "text": text, "source": "web",
                    })),
                Some(user),
            )
            .await?;
        self.0.read(res).await
    }

    pub async fn task(&self, user: &str, id: &str) -> UpstreamResult<Task> {
        self.get(user, &["v1", "tasks", id], &[]).await
    }

    pub async fn task_events(&self, user: &str, id: &str) -> UpstreamResult<Vec<TaskEvent>> {
        Ok(self
            .get::<List<TaskEvent>>(user, &["v1", "tasks", id, "events"], &[])
            .await?
            .into_vec())
    }

    /// The PM's most recent tasks, newest first.
    pub async fn tasks(&self, user: &str, limit: usize) -> UpstreamResult<Vec<Task>> {
        let limit = limit.to_string();
        Ok(self
            .get::<List<Task>>(
                user,
                &["v1", "tasks"],
                &[("requested_by", user), ("limit", &limit)],
            )
            .await?
            .into_vec())
    }
}

/// The tool server's approvals routes, with pmos-web's own token (`PMOS_WEB_TOKEN` there).
#[derive(Clone)]
pub struct ToolServer(Client);

impl ToolServer {
    pub fn new(http: reqwest::Client, base: &str, credentials: &Credentials) -> Self {
        Self(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: credentials.token.clone(),
            signer: credentials.signer.clone(),
            session: None,
            service: "tool server",
        })
    }

    /// The same upstream, with `session_ref` (`session::assertion_ref`, never the session id)
    /// as the `sid` of every assertion it signs.
    pub fn for_session(&self, session_ref: &str) -> Self {
        let mut c = self.0.clone();
        c.session = Some(session_ref.to_string());
        Self(c)
    }

    /// Every item in `user`'s inbox, newest first.
    pub async fn approvals(&self, user: &str) -> UpstreamResult<Vec<Approval>> {
        let url = self.0.url(&["internal", "approvals"])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]), Some(user))
            .await?;
        Ok(self.0.read::<List<Approval>>(res).await?.into_vec())
    }

    /// One item of `user`'s, `?user=<handle>`, with an assertion for them: the tool server
    /// answers `404` for anyone else's item, or for a read that names nobody.
    pub async fn approval_for(&self, user: &str, id: &str) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]), Some(user))
            .await?;
        self.0.read(res).await
    }

    /// What the run that drafted item `id` read, for its own `user` only.
    pub async fn sources(&self, id: &str, user: &str) -> UpstreamResult<Vec<ApprovalSource>> {
        let url = self.0.url(&["internal", "approvals", id, "sources"])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]), Some(user))
            .await?;
        Ok(self.0.read::<SourcesReply>(res).await?.sources)
    }

    /// Save `user`'s edit of their pending item `id`: `PUT /internal/approvals/{id}/draft
    /// ?user=<handle>` `{"args": …}`, with an assertion for them. The tool server keeps it on
    /// the item, so the Slack card and the console show it and an approve anywhere writes it.
    pub async fn save_draft(&self, user: &str, id: &str, args: &Value) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id, "draft"])?;
        let res = self
            .0
            .send(
                self.0
                    .http
                    .put(url)
                    .query(&[("user", user)])
                    .json(&json!({ "args": args })),
                Some(user),
            )
            .await?;
        self.0.read(res).await
    }

    /// Is the tool server up (`GET /healthz`, no token, no person)? For `/readyz`.
    pub async fn healthy(&self) -> bool {
        self.0.healthy().await
    }

    /// A decision, with an assertion for the PM in `decided_by`.
    pub async fn decide(&self, id: &str, body: &DecisionBody) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id, "decision"])?;
        let res = self
            .0
            .send(self.0.http.post(url).json(body), Some(&body.decided_by))
            .await?;
        self.0.read(res).await
    }
}

/// The tool server's read-only admin and auditor routes (`GET /internal/admin/{route}`), for
/// `viewer`: the call names them as `user=` and carries an assertion for them, and the tool
/// server checks their role itself and records the read. The reply is left as JSON; the
/// caller keeps only the fields it serves (`app/admin.rs`).
impl ToolServer {
    pub async fn admin(
        &self,
        viewer: &str,
        route: &str,
        query: &[(&str, &str)],
    ) -> UpstreamResult<Value> {
        let url = self.0.url(&["internal", "admin", route])?;
        let mut q = vec![("user", viewer)];
        q.extend_from_slice(query);
        let res = self
            .0
            .send(self.0.http.get(url).query(&q), Some(viewer))
            .await?;
        self.0.read(res).await
    }
}
