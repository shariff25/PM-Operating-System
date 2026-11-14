//! The console's two upstreams: the bot runtime (`pmos-bots`, as client `console`) and the
//! tool server (approvals, with the service token). Shapes follow
//! `docs/bot-runtime-spec.md` §3 and `docs/toolserver.md`; fields the console doesn't show
//! are ignored, and lists may come bare (`[…]`) or wrapped (`{"items": […]}`).

use std::collections::BTreeMap;
use std::time::Duration;

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

/// An HTTP client for one upstream.
#[derive(Clone)]
pub struct Upstream {
    http: reqwest::Client,
    base: String,
    token: String,
    service: &'static str,
}

impl Upstream {
    pub fn new(http: reqwest::Client, base: &str, token: &str, service: &'static str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
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

    async fn send(&self, req: reqwest::RequestBuilder) -> UpstreamResult<reqwest::Response> {
        let res = req
            .bearer_auth(&self.token)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| {
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

    async fn get<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> UpstreamResult<T> {
        let url = self.url(segments)?;
        let res = self.send(self.http.get(url).query(query)).await?;
        res.json::<T>()
            .await
            .map_err(|e| self.failed(format!("sent a reply the console can't read: {e}")))
    }

    async fn list<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> UpstreamResult<Vec<T>> {
        Ok(self.get::<List<T>>(segments, query).await?.into_vec())
    }

    // The bot runtime.

    pub async fn bots(&self) -> UpstreamResult<Vec<BotSummary>> {
        self.list(&["v1", "bots"], &[]).await
    }

    pub async fn bot(&self, name: &str) -> UpstreamResult<BotDetail> {
        Ok(self
            .get::<BotDetailWire>(&["v1", "bots", name], &[])
            .await?
            .into())
    }

    pub async fn tasks(&self, requested_by: Option<&str>, limit: u32) -> UpstreamResult<Vec<Task>> {
        let limit = limit.to_string();
        let mut query = vec![("limit", limit.as_str())];
        if let Some(who) = requested_by {
            query.push(("requested_by", who));
        }
        self.list(&["v1", "tasks"], &query).await
    }

    pub async fn task(&self, id: &str) -> UpstreamResult<Task> {
        self.get(&["v1", "tasks", id], &[]).await
    }

    pub async fn task_events(&self, id: &str) -> UpstreamResult<Vec<TaskEvent>> {
        self.list(&["v1", "tasks", id, "events"], &[]).await
    }

    pub async fn task_trace(&self, id: &str) -> UpstreamResult<Vec<TraceEntry>> {
        self.list(&["v1", "tasks", id, "trace"], &[]).await
    }

    pub async fn routines(&self, owner: Option<&str>) -> UpstreamResult<Vec<Routine>> {
        let query: Vec<(&str, &str)> = owner.map(|o| ("owner", o)).into_iter().collect();
        self.list(&["v1", "routines"], &query).await
    }

    pub async fn set_routine_enabled(&self, id: &str, enabled: bool) -> UpstreamResult<()> {
        let url = self.url(&["v1", "routines", id])?;
        self.send(
            self.http
                .patch(url)
                .json(&serde_json::json!({ "enabled": enabled })),
        )
        .await
        .map(drop)
    }

    pub async fn delete_routine(&self, id: &str) -> UpstreamResult<()> {
        let url = self.url(&["v1", "routines", id])?;
        self.send(self.http.delete(url)).await.map(drop)
    }

    // The tool server.

    /// A PM's pending approvals, newest first.
    pub async fn pending_approvals(&self, user: &str) -> UpstreamResult<Vec<Approval>> {
        self.list(
            &["internal", "approvals"],
            &[("user", user), ("status", "pending")],
        )
        .await
    }
}
