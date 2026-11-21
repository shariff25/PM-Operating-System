//! The two services the runtime calls: the tool server (bot sessions, approval items) and
//! the gateway (the model, for parsing routines).

use std::time::Duration;

use pmos_contract::Sensitivity;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// Down, unreachable or erroring: worth trying again later.
    #[error("{0} is unavailable right now")]
    Unavailable(&'static str),
    /// It answered and said no; the message is its reason.
    #[error("{0}")]
    Refused(String),
    /// The tool server refused a persona session with one of its persona codes
    /// (`persona_needs_personal_bot`, `persona_unavailable`, `persona_not_on_team`); the
    /// message is its sentence for the PM.
    #[error("{message}")]
    PersonaRefused { code: String, message: String },
}

/// The tool server's codes for a refused persona session (`docs/bot-marketplace-spec.md` §6,
/// "Sessions"; `docs/toolserver.md`).
pub const PERSONA_CODES: [&str; 3] = [
    "persona_needs_personal_bot",
    "persona_unavailable",
    "persona_not_on_team",
];

/// The persona a session runs as, as the tool server reports it (`201 {persona}`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionPersona {
    pub key: String,
    pub display_name: String,
    pub title: String,
    pub role: String,
}

/// A session the tool server started (`POST /internal/sessions`, `201`).
#[derive(Clone, PartialEq, Eq)]
pub struct SessionGrant {
    pub token: String,
    /// The session's tools by harness name (`jira_search`), when the answer listed them.
    pub tools: Option<Vec<String>>,
    /// The persona it runs as; `None` for Nudge.
    pub persona: Option<SessionPersona>,
}

/// The token never reaches a log through `Debug`.
impl std::fmt::Debug for SessionGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGrant")
            .field("token", &"<redacted>")
            .field("tools", &self.tools)
            .field("persona", &self.persona)
            .finish()
    }
}

/// Read a `201` answer of `POST /internal/sessions`. A missing token, or a `persona` that
/// doesn't read, is the tool server misbehaving: unavailable, never a guess.
pub fn read_session_grant(body: &Value) -> Result<SessionGrant, CallError> {
    let token = body["token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or(CallError::Unavailable("the tool server"))?
        .to_string();
    let tools = match body.get("tools") {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|t| t["name"].as_str().or(t.as_str()).map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or(CallError::Unavailable("the tool server"))?,
        ),
        _ => None,
    };
    let persona = match body.get("persona") {
        None | Some(Value::Null) => None,
        Some(p) => Some(
            serde_json::from_value::<SessionPersona>(p.clone())
                .map_err(|_| CallError::Unavailable("the tool server"))?,
        ),
    };
    Ok(SessionGrant {
        token,
        tools,
        persona,
    })
}

fn http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(timeout)
        .build()
        .expect("http client builds")
}

/// Approval ids go into a URL path, so only plain ids are accepted (`apr_…`).
pub fn valid_approval_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The parts of a tool server approval item the runtime reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApprovalState {
    pub id: String,
    #[serde(default)]
    pub tool: Option<String>,
    pub status: String,
    #[serde(default)]
    pub decided_by: Option<String>,
}

impl ApprovalState {
    /// Still waiting for a PM, or approved and being written. Every other state is closed:
    /// `approved`, `rejected`, `failed`, and `expired` (a Slack draft nobody decided within a
    /// day).
    pub fn is_open(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "executing")
    }
}

/// One drafted outbound action, for `GET /v1/tasks/{id}/drafts`: the approval item's tool,
/// arguments (as they stand: edited ones once a PM edited them) and status. An item the tool
/// server no longer has reads `missing`, with no tool or arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDraft {
    pub id: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub args: Option<Value>,
    pub status: String,
}

impl TaskDraft {
    pub fn missing(id: &str) -> Self {
        Self {
            id: id.to_string(),
            tool: None,
            args: None,
            status: "missing".into(),
        }
    }
}

/// The tool server's `/internal` API, with the orchestrator's service token.
#[derive(Clone)]
pub struct ToolServer {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ToolServer {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: http(Duration::from_secs(10)),
        }
    }

    /// A bot session for one task (spec §2): the tool server applies the bot's role
    /// allowlist, data class and write settings to every call made with it.
    ///
    /// `history` is the highest data class the task's conversation history was built from,
    /// for a task that carries earlier turns: the session starts there (W6). `persona` is the
    /// marketplace bot it runs as (`None`: Nudge, and no `persona` field is sent, so an older
    /// tool server is asked exactly what it was asked before); the tool server checks it is
    /// offered and on the PM's team, and refuses with a persona code otherwise.
    pub async fn create_session(
        &self,
        user: &str,
        bot: &str,
        run_id: &str,
        history: Option<Sensitivity>,
        persona: Option<&str>,
    ) -> Result<SessionGrant, CallError> {
        let mut body = json!({"user": user, "bot": bot, "run_id": run_id});
        if let Some(level) = history {
            body["history_sensitivity"] = json!(level);
        }
        if let Some(persona) = persona {
            body["persona"] = json!(persona);
        }
        let res = self
            .http
            .post(format!("{}/internal/sessions", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "tool server unreachable");
                CallError::Unavailable("the tool server")
            })?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return read_session_grant(&body);
        }
        if status.is_server_error() || status.as_u16() == 429 {
            tracing::warn!(%status, "tool server error creating a session");
            return Err(CallError::Unavailable("the tool server"));
        }
        if let Some(code) = body["code"].as_str().filter(|c| PERSONA_CODES.contains(c)) {
            return Err(CallError::PersonaRefused {
                code: code.to_string(),
                message: body["error"]
                    .as_str()
                    .unwrap_or("the tool server refused this bot's session")
                    .to_string(),
            });
        }
        Err(CallError::Refused(
            body["error"]
                .as_str()
                .unwrap_or("the tool server refused the session")
                .to_string(),
        ))
    }

    /// End a session at once (`DELETE /internal/session`, `X-Pmos-Session`). Best effort: a
    /// session that can't be revoked still lapses at its TTL, and it is never handed to a
    /// worker. Used for a session that doesn't match the task's persona.
    pub async fn revoke_session(&self, token: &str) -> bool {
        match self
            .http
            .delete(format!("{}/internal/session", self.base))
            .bearer_auth(&self.token)
            .header("x-pmos-session", token)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => true,
            Ok(res) => {
                tracing::warn!(status = %res.status(), "the tool server didn't revoke a session");
                false
            }
            Err(_) => {
                tracing::warn!("the tool server was unreachable revoking a session");
                false
            }
        }
    }

    /// The highest data class the run's sessions read (`GET /internal/runs/{id}/sensitivity`),
    /// or `None` when the tool server has no session of it stored.
    pub async fn run_sensitivity(&self, run_id: &str) -> Result<Option<Sensitivity>, CallError> {
        let res = self
            .http
            .get(format!("{}/internal/runs/{run_id}/sensitivity", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CallError::Unavailable("the tool server"))?;
        if !res.status().is_success() {
            tracing::warn!(status = %res.status(), "tool server error reading a run's level");
            return Err(CallError::Unavailable("the tool server"));
        }
        let body: Value = res
            .json()
            .await
            .map_err(|_| CallError::Unavailable("the tool server"))?;
        match &body["sensitivity"] {
            Value::Null => Ok(None),
            Value::String(s) => Sensitivity::parse(s)
                .map(Some)
                .ok_or(CallError::Unavailable("the tool server")),
            _ => Err(CallError::Unavailable("the tool server")),
        }
    }

    /// One approval item's state, or `None` if the tool server doesn't have it.
    pub async fn approval(&self, id: &str) -> Result<Option<ApprovalState>, CallError> {
        self.approval_as(id).await
    }

    /// One approval item as a draft, or `None` if the tool server doesn't have it.
    pub async fn draft(&self, id: &str) -> Result<Option<TaskDraft>, CallError> {
        self.approval_as(id).await
    }

    /// `GET /internal/approvals/{id}`, read as `T`.
    async fn approval_as<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>, CallError> {
        if !valid_approval_id(id) {
            return Ok(None);
        }
        let res = self
            .http
            .get(format!("{}/internal/approvals/{id}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CallError::Unavailable("the tool server"))?;
        let status = res.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            tracing::warn!(%status, approval = %id, "tool server error reading an approval");
            return Err(CallError::Unavailable("the tool server"));
        }
        res.json()
            .await
            .map(Some)
            .map_err(|_| CallError::Unavailable("the tool server"))
    }

    /// The tool server's own `/readyz`.
    pub async fn ready(&self) -> bool {
        self.http
            .get(format!("{}/readyz", self.base))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

/// The model gateway's OpenAI-compatible API.
#[derive(Clone)]
pub struct Gateway {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// The model role routine parsing uses: extraction, not drafting.
pub const EXTRACTION_MODEL: &str = "pmos/extraction";

impl Gateway {
    /// `base` is the OpenAI base, ending in `/v1`.
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: http(Duration::from_secs(60)),
        }
    }

    /// Ask the extraction model for one JSON object. Returns the reply text as it came;
    /// the caller parses and validates it. `user` is the PM who is waiting, so the call
    /// queues as interactive work under their name.
    pub async fn extract_json(
        &self,
        system: &str,
        prompt: &str,
        user: &str,
    ) -> Result<String, CallError> {
        let res = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .header("X-Pmos-User", user)
            .header("X-Pmos-Priority", "interactive")
            .json(&json!({
                "model": EXTRACTION_MODEL,
                "temperature": 0,
                "response_format": {"type": "json_object"},
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": prompt},
                ],
            }))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "gateway unreachable");
                CallError::Unavailable("the model gateway")
            })?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            tracing::warn!(%status, "gateway error parsing a routine");
            return Err(CallError::Unavailable("the model gateway"));
        }
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or(CallError::Unavailable("the model gateway"))
    }

    /// `GET /models` answers with this service's token: the gateway is up and lets us in.
    pub async fn ready(&self) -> bool {
        self.http
            .get(format!("{}/models", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_ids_are_plain() {
        assert!(valid_approval_id("apr_0123abcd"));
        assert!(!valid_approval_id("../sessions"));
        assert!(!valid_approval_id("a/b"));
        assert!(!valid_approval_id(""));
        let open = |s: &str| {
            ApprovalState {
                id: "a".into(),
                tool: None,
                status: s.into(),
                decided_by: None,
            }
            .is_open()
        };
        assert!(open("pending") && open("executing"));
        assert!(!open("approved") && !open("rejected") && !open("failed") && !open("expired"));
    }

    #[test]
    fn drafts_read_from_approval_items() {
        let item = json!({"id": "apr_1", "user": "ana", "bot": "docs", "run_id": "tsk_1",
            "tool": "jira.create_issue", "args": {"summary": "x"}, "args_hash": "h",
            "status": "pending", "created_at": "2026-11-14T10:00:00Z"});
        let draft: TaskDraft = serde_json::from_value(item).unwrap();
        assert_eq!(draft.tool.as_deref(), Some("jira.create_issue"));
        assert_eq!(draft.args, Some(json!({"summary": "x"})));
        assert_eq!(
            serde_json::to_value(TaskDraft::missing("apr_2")).unwrap(),
            json!({"id": "apr_2", "tool": null, "args": null, "status": "missing"})
        );
    }
}
