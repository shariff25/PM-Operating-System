//! The two upstreams: the bot runtime (`pmos-bots`, as client `web`, every call naming the
//! PM in `X-Pmos-User`) and the tool server (approvals, with the service token). Shapes
//! follow `docs/bot-runtime-spec.md` §3 and `docs/toolserver.md`; fields not used here are
//! ignored, and lists may come bare (`[…]`) or wrapped (`{"items": […]}`).
//!
//! Tokens are only ever sent as `Authorization` headers to the configured base URLs. Error
//! values carry a plain reason, never a token or an upstream body.

use std::time::Duration;

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
    /// The upstream answered with another client error, and what it said: `code` and
    /// `error` when the body had them.
    Refused {
        service: &'static str,
        status: u16,
        code: Option<String>,
        error: Option<String>,
    },
    /// Connection failure, timeout, a server error, or a body that didn't parse.
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

/// An HTTP client for one upstream.
#[derive(Clone)]
struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
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

    async fn send(&self, req: reqwest::RequestBuilder) -> UpstreamResult<reqwest::Response> {
        let res = req
            .bearer_auth(&self.token)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                self.failed(if e.is_timeout() {
                    "timed out"
                } else {
                    "didn't answer"
                })
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
            return Err(UpstreamError::Refused {
                service: self.service,
                status: status.as_u16(),
                code: text("code"),
                error: text("error"),
            });
        }
        Err(self.failed(format!("answered {status}")))
    }

    async fn read<T: DeserializeOwned>(&self, res: reqwest::Response) -> UpstreamResult<T> {
        res.json::<T>()
            .await
            .map_err(|_| self.failed("sent a reply pmos-web can't read"))
    }
}

/// The bot runtime, as client `web`.
#[derive(Clone)]
pub struct Runtime(Client);

impl Runtime {
    pub fn new(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            service: "bot runtime",
        })
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
            .send(self.0.http.get(url).query(query).header(USER_HEADER, user))
            .await?;
        self.0.read(res).await
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

/// The tool server, with the service token.
#[derive(Clone)]
pub struct ToolServer(Client);

impl ToolServer {
    pub fn new(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            service: "tool server",
        })
    }

    /// Every item in `user`'s inbox, newest first.
    pub async fn approvals(&self, user: &str) -> UpstreamResult<Vec<Approval>> {
        let url = self.0.url(&["internal", "approvals"])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]))
            .await?;
        Ok(self.0.read::<List<Approval>>(res).await?.into_vec())
    }

    pub async fn approval(&self, id: &str) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id])?;
        let res = self.0.send(self.0.http.get(url)).await?;
        self.0.read(res).await
    }

    /// What the run that drafted item `id` read, for its own `user` only.
    pub async fn sources(&self, id: &str, user: &str) -> UpstreamResult<Vec<ApprovalSource>> {
        let url = self.0.url(&["internal", "approvals", id, "sources"])?;
        let res = self
            .0
            .send(self.0.http.get(url).query(&[("user", user)]))
            .await?;
        Ok(self.0.read::<SourcesReply>(res).await?.sources)
    }

    pub async fn decide(&self, id: &str, body: &DecisionBody) -> UpstreamResult<Approval> {
        let url = self.0.url(&["internal", "approvals", id, "decision"])?;
        let res = self.0.send(self.0.http.post(url).json(body)).await?;
        self.0.read(res).await
    }
}
