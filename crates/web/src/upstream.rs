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
//! [`ASSERTION_SESSION`]. Its `roles` are the PM's roles as the user database answered them
//! for this request (`Runtime::for_user`, `ToolServer::for_user`; spec §9): informational,
//! since receivers check roles in their own data.
//!
//! Tokens are only ever sent as `Authorization` headers to the configured base URLs. Error
//! values carry a plain reason, never a token, a key or an upstream body.

use std::sync::Arc;
use std::time::Duration;

use pmos_assertion::{Claims, KeyRing, RequestBinding};
use reqwest::header::{HeaderName, HeaderValue};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How long to wait for an upstream.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The length of a refused sign-in's `attempt` hash, in hex characters (the tool server's
/// `ATTEMPT_HEX`).
pub const ATTEMPT_HEX: usize = 12;

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
    /// The highest data class the task's sessions read, once it finished (or its floor
    /// before then): `public`, `internal` or `restricted`.
    #[serde(default)]
    pub sources_sensitivity: Option<String>,
    /// The persona (marketplace bot) the task runs as, `null` for Nudge (the bot marketplace,
    /// slice R1). A runtime from before personas doesn't send it: every task is then Nudge's.
    #[serde(default)]
    pub persona: Option<String>,
    /// A call of the task was answered by a fallback model (`docs/model-providers-spec.md`
    /// §4.10, slice MP7): the app marks the reply. A runtime from before routes doesn't send it.
    #[serde(default)]
    pub fallback_used: bool,
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
    /// The tool server's hash of `args`: an approve names it (`expected_args_hash`), so the
    /// tool server refuses it as `stale` if the draft changed since this read.
    #[serde(default)]
    pub args_hash: String,
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
    /// For a Slack draft, its channel as the tool server last read it from Slack: kind,
    /// name, and for a DM the team member it is with. An older tool server doesn't send it.
    #[serde(default)]
    pub target_channel: Option<TargetChannel>,
    /// The version of the draft's text (slice U6a): bumped by each change to its arguments.
    /// An older tool server doesn't send it.
    #[serde(default)]
    pub revision: Option<i64>,
    /// Held (slice M1): pending, drafted by a bot no longer offered, and not released by an
    /// Admin. The owner may reject it; approving or editing it is refused (`409 held`). The
    /// tool server leaves the field out otherwise.
    #[serde(default)]
    pub held: bool,
    /// Reviewers' live recommendations on the draft (slice U6a), as its owner reads them. An
    /// older tool server doesn't send them.
    #[serde(default)]
    pub recommendations: Vec<UpstreamRecommendation>,
    /// The persona whose session drafted the item, `null` for Nudge (slice B1; copied from
    /// the drafting session and never changed). An older tool server doesn't send it.
    #[serde(default)]
    pub persona: Option<String>,
}

/// One of the tool server's `recommendations` on an approval item (`docs/web-api.md`,
/// Drafts): untrusted text (`reviewer_name`, `note`) by way of the tool server; `drafts.rs`
/// keeps only what is shaped as the contract says.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct UpstreamRecommendation {
    #[serde(default)]
    pub reviewer: String,
    #[serde(default)]
    pub reviewer_name: Option<String>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub note_removed: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub stale: bool,
}

/// The tool server's `target_channel` (`docs/web-api.md`, Drafts). Untrusted text from
/// Slack by way of the tool server: `drafts.rs` shows a name only if it is shaped like one.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct TargetChannel {
    /// `channel`, `private_channel`, `im` or `mpim`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    /// A DM's other person, as a team handle.
    #[serde(default)]
    pub with: Option<String>,
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
    /// The `args_hash` of the draft as the PM was shown it: the tool server refuses the
    /// decision as `409 stale` if the item holds other arguments now. Required on `approve`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_args_hash: Option<String>,
}

/// One PM's inbox as the tool server lists it: every pending item (and the newest decided
/// ones), and `truncated` if it ever left pending items out past its safety cap.
#[derive(Debug, Clone, Default)]
pub struct Inbox {
    pub items: Vec<Approval>,
    pub truncated: bool,
    pub total_pending: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InboxWire {
    Paged {
        items: Vec<Approval>,
        #[serde(default)]
        truncated: bool,
        #[serde(default)]
        total_pending: Option<usize>,
    },
    Bare(Vec<Approval>),
}

/// Where the text of a saved edit came from, when a bot wrote it (an accepted suggestion):
/// the revision task, and the level the runtime recorded for it. The tool server raises the
/// draft's level to what its own sessions for that run read; the level here is only a floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub run_id: String,
    pub sensitivity: Option<String>,
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
    pub fn new(issuer: &'static str, audience: &str, ring: KeyRing) -> Self {
        Self {
            issuer,
            audience: audience.to_string(),
            ring,
        }
    }

    /// An assertion that `subject`, holding `roles` (as the user database answered for this
    /// request; informational: receivers check roles themselves), is the person `request` is
    /// for, in the session `session` refers to (the `sid` claim), bound to the request
    /// exactly as it will be sent.
    pub fn sign(
        &self,
        subject: &str,
        roles: &[pmos_contract::model::Role],
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
            &roles
                .iter()
                .filter(|r| r.is_known())
                .cloned()
                .collect::<Vec<_>>(),
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
    /// The PM's roles for assertions, as the user database answered for this request.
    roles: Arc<Vec<pmos_contract::model::Role>>,
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
        let res = self.execute(req, subject, TIMEOUT).await?;
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

    /// Build, sign (as in [`Self::send`]) and send `req` with `timeout`, and give back the
    /// response whatever its status. Only a request that couldn't be sent, or whose answer
    /// was lost, is an error.
    async fn execute(
        &self,
        req: reqwest::RequestBuilder,
        subject: Option<&str>,
        timeout: Duration,
    ) -> UpstreamResult<reqwest::Response> {
        let mut request = req
            .bearer_auth(&self.token)
            .timeout(timeout)
            .build()
            .map_err(|_| self.failed("couldn't build the request"))?;
        if let (Some(signer), Some(subject)) = (&self.signer, subject) {
            let session = self.session.as_deref().unwrap_or(ASSERTION_SESSION);
            let value = signer
                .sign(subject, &self.roles, session, &request)
                .map_err(|why| self.failed(why))?;
            request
                .headers_mut()
                .insert(HeaderName::from_static(pmos_assertion::HEADER), value);
        }
        self.http.execute(request).await.map_err(|e| {
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
        })
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
            roles: Arc::default(),
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

    /// [`Self::for_session`], with `roles` (the PM's, from the user database for this
    /// request) in every assertion it signs.
    pub fn for_user(&self, session_ref: &str, roles: &[pmos_contract::model::Role]) -> Self {
        let mut c = self.0.clone();
        c.session = Some(session_ref.to_string());
        c.roles = Arc::new(roles.to_vec());
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
        self.create_task_at(user, bot, text, None).await
    }

    /// [`Self::create_task`] as `persona` (a marketplace bot's key; `None` is Nudge, and is
    /// sent as no `persona` at all, as before personas).
    pub async fn create_task_as(
        &self,
        user: &str,
        bot: &str,
        text: &str,
        persona: Option<&str>,
    ) -> UpstreamResult<Task> {
        self.create_task_with(user, bot, text, None, persona).await
    }

    /// [`Self::create_task`] whose session starts at least at `floor` (`public`, `internal`
    /// or `restricted`): a revision of a draft, whose prompt carries the draft's text at the
    /// draft's level.
    pub async fn create_task_at(
        &self,
        user: &str,
        bot: &str,
        text: &str,
        floor: Option<&str>,
    ) -> UpstreamResult<Task> {
        self.create_task_with(user, bot, text, floor, None).await
    }

    async fn create_task_with(
        &self,
        user: &str,
        bot: &str,
        text: &str,
        floor: Option<&str>,
        persona: Option<&str>,
    ) -> UpstreamResult<Task> {
        let url = self.0.url(&["v1", "tasks"])?;
        let mut body = json!({
            "bot": bot, "requested_by": user, "text": text, "source": "web",
        });
        if let Some(floor) = floor {
            body["sensitivity_floor"] = json!(floor);
        }
        if let Some(persona) = persona {
            body["persona"] = json!(persona);
        }
        let res = self
            .0
            .send(
                self.0.http.post(url).header(USER_HEADER, user).json(&body),
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

    /// The PM's most recent tasks that ran as `persona` (a key, or `nudge`), newest first
    /// (`GET /v1/tasks?persona=`, slice R1).
    pub async fn tasks_as(
        &self,
        user: &str,
        persona: &str,
        limit: usize,
    ) -> UpstreamResult<Vec<Task>> {
        let limit = limit.to_string();
        Ok(self
            .get::<List<Task>>(
                user,
                &["v1", "tasks"],
                &[
                    ("requested_by", user),
                    ("persona", persona),
                    ("limit", &limit),
                ],
            )
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
            roles: Arc::default(),
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

    /// [`Self::for_session`], with `roles` (the PM's, from the user database for this
    /// request) in every assertion it signs.
    pub fn for_user(&self, session_ref: &str, roles: &[pmos_contract::model::Role]) -> Self {
        let mut c = self.0.clone();
        c.session = Some(session_ref.to_string());
        c.roles = Arc::new(roles.to_vec());
        Self(c)
    }

    /// `user`'s inbox, newest first: every pending item, and the newest decided ones.
    pub async fn approvals(&self, user: &str) -> UpstreamResult<Inbox> {
        let url = self.0.url(&["internal", "approvals"])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]), Some(user))
            .await?;
        Ok(match self.0.read::<InboxWire>(res).await? {
            InboxWire::Bare(items) => Inbox {
                items,
                ..Inbox::default()
            },
            InboxWire::Paged {
                items,
                truncated,
                total_pending,
            } => Inbox {
                items,
                truncated,
                total_pending,
            },
        })
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
    ///
    /// Text a bot revised (`revision`) names its run, so the tool server raises the draft's
    /// level to what that run read; text the PM typed names none.
    pub async fn save_draft(
        &self,
        user: &str,
        id: &str,
        args: &Value,
        revision: Option<&Revision>,
    ) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id, "draft"])?;
        let mut body = json!({ "args": args });
        if let Some(r) = revision {
            body["revision_run"] = json!(r.run_id);
            if let Some(level) = &r.sensitivity {
                body["revision_sensitivity"] = json!(level);
            }
        }
        let res = self
            .0
            .send(
                self.0.http.put(url).query(&[("user", user)]).json(&body),
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

    /// `GET /internal/admin/policy/proofs?user=`: the newest host proof of each bot
    /// (`docs/openshell-prover-and-sandbox-audit-spec.md` §A.5). pmos-web runs no prover.
    pub async fn policy_proofs(&self, viewer: &str) -> UpstreamResult<Value> {
        let url = self.0.url(&["internal", "admin", "policy", "proofs"])?;
        let res = self
            .0
            .send(
                self.0.http.get(url).query(&[("user", viewer)]),
                Some(viewer),
            )
            .await?;
        self.0.read(res).await
    }

    /// Record a sign-in event (`POST /internal/admin/events`, `docs/web-api.md`, "Sign-in
    /// events"): for `member` (named as `user=`, with an assertion for them) on a sign-in or
    /// sign-out, for no one on a refusal. `body` holds fixed action names and codes only.
    pub async fn record_sign_in(&self, member: Option<&str>, body: &Value) -> UpstreamResult<()> {
        let url = self.0.url(&["internal", "admin", "events"])?;
        let mut req = self.0.http.post(url).json(body);
        if let Some(m) = member {
            req = req.query(&[("user", m)]);
        }
        let res = self.0.send(req, member).await?;
        self.0.read::<Value>(res).await.map(drop)
    }

    /// A short keyed hash of a name someone tried to sign in with: [`ATTEMPT_HEX`] hex
    /// characters of HMAC-SHA256 under a key derived from this server's tool server token, of
    /// the name trimmed and lowercased. Repeated attempts with one name match; the name can't
    /// be read back, or guessed from a list of emails without the token.
    pub fn attempt_hash(&self, name: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut key = Hmac::<sha2::Sha256>::new_from_slice(self.0.token.as_bytes())
            .expect("HMAC takes any key length");
        key.update(b"pmos-web-sign-in-attempt-v1");
        let derived = key.finalize().into_bytes();
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&derived).expect("HMAC takes any key length");
        mac.update(name.trim().to_lowercase().as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .take(ATTEMPT_HEX / 2)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Record an admin action (`POST /internal/admin/{route}?user=<viewer>`, JSON `body`), with
    /// an assertion for `viewer` bound to that body. The tool server checks their role itself.
    pub async fn admin_record(
        &self,
        viewer: &str,
        route: &str,
        body: &Value,
    ) -> UpstreamResult<Value> {
        let url = self.0.url(&["internal", "admin", route])?;
        let res = self
            .0
            .send(
                self.0.http.post(url).query(&[("user", viewer)]).json(body),
                Some(viewer),
            )
            .await?;
        self.0.read(res).await
    }
}

/// How long opening a proposal may take: the tool server checks the change again and makes
/// several GitHub calls.
pub const PROPOSAL_TIMEOUT: Duration = Duration::from_secs(90);

impl ToolServer {
    /// Ask the tool server to open a proposal for `admin` (`POST /internal/admin/proposals/open
    /// ?user=<admin>`, JSON `body`, an assertion for them bound to that body). The tool server
    /// checks everything again and opens the pull request with the admin's own linked GitHub
    /// account. Gives back its status and JSON body, whatever the status (`Value::Null` if the
    /// body isn't JSON): the route relays the outcome.
    pub async fn open_proposal(&self, admin: &str, body: &Value) -> UpstreamResult<(u16, Value)> {
        let url = self.0.url(&["internal", "admin", "proposals", "open"])?;
        let res = self
            .0
            .execute(
                self.0.http.post(url).query(&[("user", admin)]).json(body),
                Some(admin),
                PROPOSAL_TIMEOUT,
            )
            .await?;
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// The user database's Admin API (`/internal/admin/{segments…}?user=<viewer>`, slice U5a):
    /// `method`, with `body` as JSON when given, and an assertion for `viewer` bound to exactly
    /// that request. Gives back the tool server's status and JSON body (`Value::Null` when it
    /// isn't JSON) whatever the status: the route words its own answer from the code.
    pub async fn admin_users(
        &self,
        viewer: &str,
        method: reqwest::Method,
        segments: &[&str],
        body: Option<&Value>,
    ) -> UpstreamResult<(u16, Value)> {
        let mut path = vec!["internal", "admin"];
        path.extend_from_slice(segments);
        let url = self.0.url(&path)?;
        let mut req = self.0.http.request(method, url).query(&[("user", viewer)]);
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = self.0.execute(req, Some(viewer), TIMEOUT).await?;
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// Password sign-in's routes (`/internal/password/{segments…}`, `docs/password-sign-in.md`):
    /// for a signed-in `user`, named as `user=` with an assertion for them bound to exactly
    /// this request; with no `user`, unsigned (nobody is signed in yet). Gives back the tool
    /// server's status, JSON body (`Value::Null` when it isn't JSON) and `Retry-After`,
    /// whatever the status. The body holds what the person typed: never logged here.
    pub async fn password_call(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        user: Option<&str>,
        body: Option<&Value>,
    ) -> UpstreamResult<(u16, Value, Option<reqwest::header::HeaderValue>)> {
        let mut path = vec!["internal", "password"];
        path.extend_from_slice(segments);
        let url = self.0.url(&path)?;
        let mut req = self.0.http.request(method, url);
        if let Some(u) = user {
            req = req.query(&[("user", u)]);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = self.0.execute(req, user, TIMEOUT).await?;
        let status = res.status().as_u16();
        let retry_after = res.headers().get(reqwest::header::RETRY_AFTER).cloned();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body, retry_after))
    }

    /// The Reviewer's routes (`/internal/reviews/{segments…}?user=<reviewer>`, slice U6a):
    /// `method`, with `body` as JSON when given, and an assertion for `reviewer` bound to
    /// exactly that request. Gives back the tool server's status and JSON body (`Value::Null`
    /// when it isn't JSON) whatever the status: the route words its own answer from the code.
    pub async fn reviews(
        &self,
        reviewer: &str,
        method: reqwest::Method,
        segments: &[&str],
        body: Option<&Value>,
    ) -> UpstreamResult<(u16, Value)> {
        let mut path = vec!["internal", "reviews"];
        path.extend_from_slice(segments);
        let url = self.0.url(&path)?;
        let mut req = self
            .0
            .http
            .request(method, url)
            .query(&[("user", reviewer)]);
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = self.0.execute(req, Some(reviewer), TIMEOUT).await?;
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// The bot catalog (`GET /internal/personas`, slice B1): the offered personas, Nudge first,
    /// with an assertion for `user` (the tool server needs none here; one sent is checked).
    pub async fn personas(&self, user: &str) -> UpstreamResult<Value> {
        let url = self.0.url(&["internal", "personas"])?;
        let res = self.0.send(self.0.http.get(url), Some(user)).await?;
        self.0.read(res).await
    }

    /// `user`'s own team (`GET /internal/team/{handle}`), or with `change` a change to it
    /// (`PUT`, JSON `{add, remove, via}`), with an assertion for `user` bound to exactly that
    /// request. Gives back the tool server's status and JSON body (`Value::Null` when it isn't
    /// JSON) whatever the status: the route words its own answer from the code.
    pub async fn team(&self, user: &str, change: Option<&Value>) -> UpstreamResult<(u16, Value)> {
        let url = self.0.url(&["internal", "team", user])?;
        let req = match change {
            None => self.0.http.get(url),
            Some(b) => self.0.http.put(url).json(b),
        };
        let res = self.0.execute(req, Some(user), TIMEOUT).await?;
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// `GET /internal/admin/{route}?user=<viewer>&…`, as [`Self::admin`], but giving back the
    /// tool server's status and JSON body whatever the status.
    pub async fn admin_raw(
        &self,
        viewer: &str,
        route: &str,
        query: &[(&str, &str)],
    ) -> UpstreamResult<(u16, Value)> {
        let url = self.0.url(&["internal", "admin", route])?;
        let mut q = vec![("user", viewer)];
        q.extend_from_slice(query);
        let res = self
            .0
            .execute(self.0.http.get(url).query(&q), Some(viewer), TIMEOUT)
            .await?;
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

impl Runtime {
    /// `owner`'s own routines (`GET /v1/routines?owner=<handle>`), as the runtime lists them;
    /// the caller keeps only the fields it serves (`app/bots.rs`).
    pub async fn routines(&self, owner: &str) -> UpstreamResult<Vec<Value>> {
        Ok(self
            .get::<List<Value>>(owner, &["v1", "routines"], &[("owner", owner)])
            .await?
            .into_vec())
    }

    /// Change one of `owner`'s routines (`PATCH /v1/routines/{id}`, JSON `body`): the bot
    /// marketplace moves a removed bot's routines to Nudge (`{"persona": "nudge"}`) or pauses
    /// them (`{"enabled": false}`).
    pub async fn patch_routine(&self, owner: &str, id: &str, body: &Value) -> UpstreamResult<()> {
        let url = self.0.url(&["v1", "routines", id])?;
        let res = self
            .0
            .send(
                self.0.http.patch(url).header(USER_HEADER, owner).json(body),
                Some(owner),
            )
            .await?;
        self.0.read::<Value>(res).await.map(drop)
    }

    /// The bots the runtime has loaded (`GET /v1/bots`), as it lists them: names, roles and
    /// scopes. For the admin's harness view.
    pub async fn bots(&self, user: &str) -> UpstreamResult<Vec<Value>> {
        Ok(self
            .get::<List<Value>>(user, &["v1", "bots"], &[])
            .await?
            .into_vec())
    }
}
