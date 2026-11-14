//! The tool server's internal API, as the bot uses it: a short session for the PM and the
//! skill, one tool call, then the session is revoked. The tool server applies the skill's
//! allowlist and audits the call, exactly as for an agent.
//!
//! Approval cards use the same token to read one inbox item and record the PM's decision
//! (spec §2): `decided_by` is the clicking PM's handle, and the tool server checks it.
//!
//! The bot has its own token (`PMOS_BOT_TOKEN`), which the tool server limits to sessions
//! for the bot's skills. Session tokens go in the `X-Pmos-Session` header, never in a URL.

use std::time::Duration;

use serde_json::{json, Value};

/// The header that carries a session token on the tool server's `/internal` API.
const SESSION_HEADER: &str = "x-pmos-session";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallError {
    #[error("PM OS is unavailable right now")]
    Unavailable,
    #[error("{0}")]
    Refused(String),
}

#[derive(Clone)]
pub struct ToolServer {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ToolServer {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client builds"),
        }
    }

    async fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Value,
    ) -> Result<Value, CallError> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body);
        if let Some(token) = session {
            req = req.header(SESSION_HEADER, token);
        }
        Self::finish(req).await
    }

    async fn finish(req: reqwest::RequestBuilder) -> Result<Value, CallError> {
        let res = req.send().await.map_err(|e| {
            tracing::warn!(error = %e, "tool server unreachable");
            CallError::Unavailable
        })?;
        let status = res.status();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(json)
        } else if status.is_server_error() && status.as_u16() != 502 {
            tracing::warn!(%status, "tool server error");
            Err(CallError::Unavailable)
        } else {
            Err(CallError::Refused(
                json["error"].as_str().unwrap_or("refused").to_string(),
            ))
        }
    }

    /// Run `tool` once as `user` under `skill`, in a session that ends with the call.
    pub async fn call_once(
        &self,
        user: &str,
        skill: &str,
        run_id: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, CallError> {
        let session = self
            .post(
                "/internal/sessions",
                None,
                json!({"user": user, "skill": skill, "run_id": run_id, "ttl_minutes": 5}),
            )
            .await?;
        let token = session["token"]
            .as_str()
            .ok_or(CallError::Unavailable)?
            .to_string();
        let result = self
            .post(
                "/internal/call",
                Some(&token),
                json!({"tool": tool, "args": args}),
            )
            .await;
        // End the session either way; it was for this one call.
        let _ = self
            .http
            .delete(format!("{}/internal/session", self.base))
            .bearer_auth(&self.token)
            .header(SESSION_HEADER, &token)
            .send()
            .await;
        Ok(result?["result"].clone())
    }

    /// `GET /internal/approvals/{id}`: one inbox item.
    pub async fn approval(&self, id: &str) -> Result<Value, CallError> {
        if !approval_id_ok(id) {
            return Err(CallError::Refused("not an approval id".into()));
        }
        Self::finish(
            self.http
                .get(format!("{}/internal/approvals/{id}", self.base))
                .bearer_auth(&self.token)
                .timeout(Duration::from_secs(5)),
        )
        .await
    }

    /// `POST /internal/approvals/{id}/decision`. `decision` is `approve`, `edit` (with `args`)
    /// or `reject`. Returns the item as decided; an approved write has run by then.
    pub async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: &str,
        args: Option<Value>,
    ) -> Result<Value, CallError> {
        if !approval_id_ok(id) {
            return Err(CallError::Refused("not an approval id".into()));
        }
        let mut body = json!({"decided_by": decided_by, "decision": decision});
        if let Some(args) = args {
            body["args"] = args;
        }
        Self::finish(
            self.http
                .post(format!("{}/internal/approvals/{id}/decision", self.base))
                .bearer_auth(&self.token)
                .timeout(Duration::from_secs(60))
                .json(&body),
        )
        .await
    }
}

/// `apr_` and hex, as the tool server makes them; anything else never reaches a URL.
pub fn approval_id_ok(id: &str) -> bool {
    id.strip_prefix("apr_").is_some_and(|rest| {
        !rest.is_empty() && rest.len() <= 64 && rest.chars().all(|c| c.is_ascii_alphanumeric())
    })
}
