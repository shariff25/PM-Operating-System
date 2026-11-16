//! A client for the parts of the bot runtime API (`docs/bot-runtime-spec.md` §3) the
//! benchmarks use, as client `evals`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A task as the runtime returns it. Unknown fields are ignored, so the runtime can add some.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTask {
    pub id: String,
    #[serde(default)]
    pub bot: String,
    #[serde(default)]
    pub requested_by: String,
    #[serde(default)]
    pub source: String,
    pub status: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub approvals: Vec<String>,
    #[serde(default)]
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The task statuses (§3).
pub mod status {
    pub const QUEUED: &str = "queued";
    pub const WORKING: &str = "working";
    pub const NEEDS_APPROVAL: &str = "needs_approval";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}

impl RuntimeTask {
    /// Whether the run is over as far as a benchmark is concerned. `needs_approval` counts:
    /// the bot has finished, and no one approves eval drafts.
    pub fn is_settled(&self) -> bool {
        matches!(
            self.status.as_str(),
            status::DONE | status::FAILED | status::CANCELLED | status::NEEDS_APPROVAL
        )
    }
}

/// A task event: `GET /v1/tasks/{id}/events`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// A trace entry: `GET /v1/tasks/{id}/trace`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceEntry {
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
    pub kind: String,
    #[serde(default)]
    pub body: Value,
}

/// A draft: one of the task's approval items, from `GET /v1/tasks/{id}/drafts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub id: String,
    /// The registry name, such as `jira.create_issue`.
    pub tool: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub status: String,
}

#[derive(Deserialize)]
struct Drafts {
    drafts: Vec<Draft>,
}

/// A failed call to the runtime.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{method} {path}: the runtime answered {status}: {body}")]
    Status {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    },
    #[error("{method} {path}: {source}")]
    Transport {
        method: &'static str,
        path: String,
        #[source]
        source: reqwest::Error,
    },
}

/// The runtime, as client `evals`.
#[derive(Clone)]
pub struct Runtime {
    http: reqwest::Client,
    base: String,
    token: String,
    /// Traces are readable by client `console` only (§3). A console token here lets failures
    /// be tagged with the trace; without one the trace is tried with `token` and skipped if
    /// refused.
    trace_token: Option<String>,
}

impl Runtime {
    pub fn new(base: &str, token: &str, trace_token: Option<&str>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("a reqwest client with a timeout always builds");
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            trace_token: trace_token.map(str::to_string),
        }
    }

    async fn send(
        &self,
        method: &'static str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        let url = format!("{}{path}", self.base);
        let req = match method {
            "POST" => self.http.post(&url),
            _ => self.http.get(&url),
        }
        .bearer_auth(token);
        let req = match body {
            Some(b) => req.json(&b),
            None => req,
        };
        let transport = |source| ApiError::Transport {
            method,
            path: path.to_string(),
            source,
        };
        let resp = req.send().await.map_err(transport)?;
        let code = resp.status();
        let text = resp.text().await.map_err(transport)?;
        if !code.is_success() {
            let mut body = text.trim().to_string();
            if body.len() > 300 {
                let cut = (0..=300)
                    .rev()
                    .find(|i| body.is_char_boundary(*i))
                    .unwrap_or(0);
                body.truncate(cut);
                body.push('…');
            }
            return Err(ApiError::Status {
                method,
                path: path.to_string(),
                status: code.as_u16(),
                body,
            });
        }
        serde_json::from_str(&text).map_err(|e| ApiError::Status {
            method,
            path: path.to_string(),
            status: code.as_u16(),
            body: format!("not the JSON expected: {e}"),
        })
    }

    fn decode<T: serde::de::DeserializeOwned>(
        method: &'static str,
        path: &str,
        v: Value,
    ) -> Result<T, ApiError> {
        serde_json::from_value(v).map_err(|e| ApiError::Status {
            method,
            path: path.to_string(),
            status: 200,
            body: format!("not the shape in the runtime spec: {e}"),
        })
    }

    /// `POST /v1/tasks` with `source: "eval"`, and the category's `level` when it has one.
    pub async fn create_task(
        &self,
        bot: &str,
        requested_by: &str,
        text: &str,
        level: Option<&str>,
    ) -> Result<RuntimeTask, ApiError> {
        self.create_task_in(bot, requested_by, text, level, None)
            .await
    }

    /// [`Self::create_task`] as a turn of `conversation`: tasks with the same conversation id,
    /// from the same requester for the same bot, are given earlier turns as `history`.
    pub async fn create_task_in(
        &self,
        bot: &str,
        requested_by: &str,
        text: &str,
        level: Option<&str>,
        conversation: Option<&str>,
    ) -> Result<RuntimeTask, ApiError> {
        let mut body =
            json!({"bot": bot, "requested_by": requested_by, "text": text, "source": "eval"});
        if let Some(level) = level {
            body["level"] = json!(level);
        }
        if let Some(conversation) = conversation {
            body["conversation"] = json!(conversation);
        }
        let v = self
            .send("POST", "/v1/tasks", &self.token, Some(body))
            .await?;
        Self::decode("POST", "/v1/tasks", v)
    }

    /// `GET /v1/tasks/{id}`.
    pub async fn task(&self, id: &str) -> Result<RuntimeTask, ApiError> {
        let path = format!("/v1/tasks/{id}");
        let v = self.send("GET", &path, &self.token, None).await?;
        Self::decode("GET", &path, v)
    }

    /// `GET /v1/tasks/{id}/events`.
    pub async fn events(&self, id: &str) -> Result<Vec<Event>, ApiError> {
        let path = format!("/v1/tasks/{id}/events");
        let v = self.send("GET", &path, &self.token, None).await?;
        Self::decode("GET", &path, v)
    }

    /// `GET /v1/tasks/{id}/drafts`: the task's approval items with their tool and arguments.
    pub async fn drafts(&self, id: &str) -> Result<Vec<Draft>, ApiError> {
        let path = format!("/v1/tasks/{id}/drafts");
        let v = self.send("GET", &path, &self.token, None).await?;
        Self::decode::<Drafts>("GET", &path, v).map(|d| d.drafts)
    }

    /// `GET /v1/tasks/{id}/trace`, or `None` when this client may not read it (401, 403) or
    /// the runtime keeps none for the task (404).
    pub async fn trace(&self, id: &str) -> Result<Option<Vec<TraceEntry>>, ApiError> {
        let path = format!("/v1/tasks/{id}/trace");
        let token = self.trace_token.as_deref().unwrap_or(&self.token);
        match self.send("GET", &path, token, None).await {
            Ok(v) => Self::decode("GET", &path, v).map(Some),
            Err(ApiError::Status {
                status: 401 | 403 | 404,
                ..
            }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
