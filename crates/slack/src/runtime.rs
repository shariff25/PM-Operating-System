//! The bots runtime (`pmos-bots`) API, as the Slack bot uses it (spec §3): the bot roster,
//! tasks, the notification feed and routines. The Slack bot is the runtime's `slack` client
//! (`PMOS_BOTS_TOKEN`); every call made for a PM carries their handle in `X-Pmos-User`, so the
//! runtime applies that PM's access, never the bot's.

use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::Deserialize;
use serde_json::{json, Value};

const USER_HEADER: &str = "x-pmos-user";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("PM OS bots are unavailable right now")]
    Unavailable,
    /// A `4xx` with the runtime's reason.
    #[error("{0}")]
    Refused(String),
    /// `422 {question}` from routine parsing: the request was unclear.
    #[error("{0}")]
    Question(String),
}

/// One entry of `GET /v1/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BotInfo {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Not in the spec's list; used if the runtime sends it.
    #[serde(default)]
    pub display_name: Option<String>,
}

impl BotInfo {
    pub fn display_name(&self) -> String {
        self.display_name
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| display_name(&self.name))
    }
}

/// The name a bot posts under: `docs` → "Docs", `pm-ana` → "PM Ana".
pub fn display_name(bot: &str) -> String {
    bot.split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            if w == "pm" {
                "PM".to_string()
            } else {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A routine or task id that is safe to put in a URL path.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Clone)]
pub struct Runtime {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Runtime {
    pub fn new(base: impl Into<String>, token: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
    }

    fn request(&self, method: Method, path: &str, user: Option<&str>) -> RequestBuilder {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(user) = user {
            req = req.header(USER_HEADER, user);
        }
        req
    }

    async fn send(&self, req: RequestBuilder) -> Result<Value, RuntimeError> {
        let res = req.send().await.map_err(|e| {
            tracing::warn!(error = %e, "bots runtime unreachable");
            RuntimeError::Unavailable
        })?;
        let status = res.status();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(json);
        }
        if status.is_server_error() {
            tracing::warn!(%status, "bots runtime error");
            return Err(RuntimeError::Unavailable);
        }
        if status.as_u16() == 422 {
            if let Some(q) = json["question"].as_str() {
                return Err(RuntimeError::Question(q.to_string()));
            }
        }
        Err(RuntimeError::Refused(
            json["error"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("refused ({status})")),
        ))
    }

    /// `GET /v1/bots`.
    pub async fn bots(&self) -> Result<Vec<BotInfo>, RuntimeError> {
        let json = self
            .send(self.request(Method::GET, "/v1/bots", None))
            .await?;
        let list = list_of(json, "bots");
        Ok(list
            .into_iter()
            .filter_map(|b| serde_json::from_value(b).ok())
            .collect())
    }

    /// `GET /v1/bots/{name}`: one bot with its role (`role.skills` lists the skills it may
    /// use).
    pub async fn bot(&self, name: &str) -> Result<Value, RuntimeError> {
        if !safe_id(name) {
            return Err(RuntimeError::Refused("not a bot name".into()));
        }
        self.send(self.request(Method::GET, &format!("/v1/bots/{name}"), None))
            .await
    }

    /// The skill names a bot's role lists, from [`Runtime::bot`]'s answer.
    pub fn role_skills(detail: &Value) -> Vec<String> {
        detail["role"]["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(String::from))
            .collect()
    }

    /// `POST /v1/tasks` as `user`, from Slack, replying in `channel`/`thread_ts`.
    pub async fn create_task(
        &self,
        user: &str,
        bot: &str,
        text: &str,
        channel: &str,
        thread_ts: &str,
    ) -> Result<Value, RuntimeError> {
        self.send(
            self.request(Method::POST, "/v1/tasks", Some(user))
                .json(&json!({
                    "bot": bot,
                    "requested_by": user,
                    "text": text,
                    "source": "slack",
                    "slack": {"channel": channel, "thread_ts": thread_ts},
                })),
        )
        .await
    }

    /// `GET /v1/notifications`: long-polls up to `wait`.
    pub async fn notifications(
        &self,
        after: Option<&str>,
        wait: Duration,
    ) -> Result<Value, RuntimeError> {
        let wait_ms = wait.as_millis().to_string();
        let mut query = vec![("wait_ms", wait_ms.as_str())];
        if let Some(after) = after {
            query.push(("after", after));
        }
        self.send(
            self.request(Method::GET, "/v1/notifications", None)
                .query(&query)
                .timeout(wait + Duration::from_secs(10)),
        )
        .await
    }

    /// `POST /v1/routines/parse`: `{draft, summary}`, or [`RuntimeError::Question`].
    pub async fn parse_routine(
        &self,
        owner: &str,
        bot: &str,
        text: &str,
        tz: &str,
    ) -> Result<Value, RuntimeError> {
        self.send(
            self.request(Method::POST, "/v1/routines/parse", Some(owner))
                .timeout(Duration::from_secs(60))
                .json(&json!({"bot": bot, "owner": owner, "text": text, "tz": tz})),
        )
        .await
    }

    /// `POST /v1/routines {draft}`.
    pub async fn create_routine(&self, owner: &str, draft: &Value) -> Result<Value, RuntimeError> {
        self.send(
            self.request(Method::POST, "/v1/routines", Some(owner))
                .json(&json!({ "draft": draft })),
        )
        .await
    }

    /// `GET /v1/routines?owner=`.
    pub async fn routines(&self, owner: &str) -> Result<Vec<Value>, RuntimeError> {
        let json = self
            .send(
                self.request(Method::GET, "/v1/routines", Some(owner))
                    .query(&[("owner", owner)]),
            )
            .await?;
        Ok(list_of(json, "routines"))
    }

    /// `PATCH /v1/routines/{id} {enabled}`.
    pub async fn set_routine_enabled(
        &self,
        owner: &str,
        id: &str,
        enabled: bool,
    ) -> Result<Value, RuntimeError> {
        if !safe_id(id) {
            return Err(RuntimeError::Refused("not a routine id".into()));
        }
        self.send(
            self.request(Method::PATCH, &format!("/v1/routines/{id}"), Some(owner))
                .json(&json!({ "enabled": enabled })),
        )
        .await
    }

    /// `DELETE /v1/routines/{id}`.
    pub async fn delete_routine(&self, owner: &str, id: &str) -> Result<(), RuntimeError> {
        if !safe_id(id) {
            return Err(RuntimeError::Refused("not a routine id".into()));
        }
        self.send(self.request(Method::DELETE, &format!("/v1/routines/{id}"), Some(owner)))
            .await
            .map(|_| ())
    }
}

/// A list returned bare (`[…]`) or wrapped (`{"<key>": […]}` or `{"items": […]}`).
fn list_of(json: Value, key: &str) -> Vec<Value> {
    match json {
        Value::Array(items) => items,
        Value::Object(mut obj) => match obj.remove(key).or_else(|| obj.remove("items")) {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        assert_eq!(display_name("docs"), "Docs");
        assert_eq!(display_name("pm-ana"), "PM Ana");
        assert_eq!(display_name("release-notes"), "Release Notes");
        assert!(safe_id("rtn_0123abc"));
        assert!(!safe_id("../internal"));
        assert!(!safe_id(""));
        assert_eq!(list_of(json!([1]), "bots"), vec![json!(1)]);
        assert_eq!(list_of(json!({"bots": [2]}), "bots"), vec![json!(2)]);
        assert_eq!(list_of(json!({"items": [3]}), "routines"), vec![json!(3)]);
        assert!(list_of(json!(null), "bots").is_empty());
    }
}
