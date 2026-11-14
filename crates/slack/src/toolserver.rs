//! The tool server's internal API, as the bot uses it: a short session for the PM and the
//! skill, one tool call, then the session is revoked. The tool server applies the skill's
//! allowlist and audits the call, exactly as for an agent.
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
}
