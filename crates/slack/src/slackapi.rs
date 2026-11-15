//! The Slack Web API, as the bot uses it: post and edit its messages, private notes, the
//! Edit modal, a PM's time zone, and for onboarding a person's name and email
//! (`users:read.email`) and a DM with someone who hasn't written yet (`im:write`). It uses the
//! bot token (`xoxb-…`).
//!
//! A message is never sent twice on purpose: a call is retried only when Slack certainly
//! didn't take it (no connection, `429` rate limit, `503`). If Slack refuses the Block Kit
//! (`invalid_blocks`), the plain-text version is sent instead, so a PM never misses a reply
//! because of formatting.

use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SlackError {
    #[error("Slack is unreachable: {0}")]
    Unreachable(String),
    #[error("Slack refused {method}: {error}")]
    Refused { method: String, error: String },
}

#[derive(Clone)]
pub struct SlackApi {
    /// `https://slack.com`
    base: String,
    token: String,
    http: reqwest::Client,
}

/// Where a posted message landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub channel: String,
    pub ts: String,
}

impl SlackApi {
    pub fn new(base: impl Into<String>, token: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
    }

    async fn call(&self, method: &str, body: &Value) -> Result<Value, SlackError> {
        let url = format!("{}/api/{method}", self.base);
        for attempt in 0..3 {
            let last = attempt == 2;
            let res = self
                .http
                .post(&url)
                .bearer_auth(&self.token)
                .json(body)
                .send()
                .await;
            let res = match res {
                Ok(r) => r,
                Err(e) if e.is_connect() && !last => {
                    tracing::warn!(method, error = %e, attempt, "Slack unreachable; retrying");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => return Err(SlackError::Unreachable(e.to_string())),
            };
            let status = res.status().as_u16();
            if (status == 429 || status == 503) && !last {
                let wait = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(10);
                tracing::warn!(method, status, wait_s = wait, "Slack asked to slow down");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            let json: Value = res
                .json()
                .await
                .map_err(|e| SlackError::Unreachable(format!("{method}: {e}")))?;
            if json["ok"].as_bool() == Some(true) {
                return Ok(json);
            }
            return Err(SlackError::Refused {
                method: method.to_string(),
                error: json["error"]
                    .as_str()
                    .unwrap_or(if status >= 500 {
                        "server error"
                    } else {
                        "unknown error"
                    })
                    .to_string(),
            });
        }
        Err(SlackError::Unreachable(format!("{method}: gave up")))
    }

    /// Call `method`; if Slack refuses the blocks, send the text alone.
    async fn call_with_text_fallback(
        &self,
        method: &str,
        mut body: Value,
    ) -> Result<Value, SlackError> {
        match self.call(method, &body).await {
            Err(SlackError::Refused { error, .. })
                if error == "invalid_blocks" && body.get("blocks").is_some() =>
            {
                tracing::warn!(method, "Slack refused the blocks; sending plain text");
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("blocks");
                }
                self.call(method, &body).await
            }
            other => other,
        }
    }

    /// `chat.postMessage`. `message` carries `channel`, `text` and optionally `thread_ts`,
    /// `username` (with `chat:write.customize`) and `blocks`.
    pub async fn post_message(&self, message: Value) -> Result<Posted, SlackError> {
        let fallback_channel = message["channel"].as_str().unwrap_or_default().to_string();
        let res = self
            .call_with_text_fallback("chat.postMessage", message)
            .await?;
        Ok(Posted {
            channel: res["channel"]
                .as_str()
                .map(String::from)
                .unwrap_or(fallback_channel),
            ts: res["ts"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// `chat.update`: replace a message the bot posted.
    pub async fn update(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
        blocks: Value,
    ) -> Result<(), SlackError> {
        self.call_with_text_fallback(
            "chat.update",
            json!({"channel": channel, "ts": ts, "text": text, "blocks": blocks}),
        )
        .await
        .map(|_| ())
    }

    /// `chat.postEphemeral`: a note only `user` sees.
    pub async fn ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), SlackError> {
        let mut body = json!({"channel": channel, "user": user, "text": text});
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        self.call("chat.postEphemeral", &body).await.map(|_| ())
    }

    /// `chat.postEphemeral` with blocks: a note with buttons only `user` sees.
    pub async fn ephemeral_blocks(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
        blocks: Value,
    ) -> Result<(), SlackError> {
        let mut body = json!({"channel": channel, "user": user, "text": text, "blocks": blocks});
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        self.call_with_text_fallback("chat.postEphemeral", body)
            .await
            .map(|_| ())
    }

    /// `conversations.open` (scope `im:write`): the app's DM channel with `user`.
    pub async fn open_dm(&self, user: &str) -> Result<String, SlackError> {
        let res = self
            .call("conversations.open", &json!({"users": user}))
            .await?;
        res["channel"]["id"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(String::from)
            .ok_or_else(|| SlackError::Refused {
                method: "conversations.open".into(),
                error: "no channel".into(),
            })
    }

    /// Reply through an interaction's `response_url` (an ephemeral message's only handle).
    pub async fn respond(&self, response_url: &str, message: &Value) {
        crate::commands::post_reply(&self.http, response_url, message).await;
    }

    /// `views.open`: show a modal. `trigger_id` is valid for 3 seconds after the click.
    pub async fn open_view(&self, trigger_id: &str, view: Value) -> Result<(), SlackError> {
        self.call(
            "views.open",
            &json!({"trigger_id": trigger_id, "view": view}),
        )
        .await
        .map(|_| ())
    }

    /// `views.publish`: replace `user`'s App Home with `view` (`docs/slack-mentions-and-home-spec.md`
    /// §4). Needs no scope beyond the Home tab being enabled.
    pub async fn publish_view(&self, user: &str, view: Value) -> Result<(), SlackError> {
        self.call("views.publish", &json!({"user_id": user, "view": view}))
            .await
            .map(|_| ())
    }

    /// `chat.getPermalink`: a link to one message. Slack answers it only for conversations
    /// the app can see, so a caller falls back to a plain channel mention. Only an
    /// `https://` link is returned.
    pub async fn permalink(&self, channel: &str, message_ts: &str) -> Result<String, SlackError> {
        let refused = |error: &str| SlackError::Refused {
            method: "chat.getPermalink".into(),
            error: error.into(),
        };
        let res = self
            .http
            .get(format!("{}/api/chat.getPermalink", self.base))
            .bearer_auth(&self.token)
            .query(&[("channel", channel), ("message_ts", message_ts)])
            .send()
            .await
            .map_err(|e| SlackError::Unreachable(e.to_string()))?;
        let json: Value = res
            .json()
            .await
            .map_err(|e| SlackError::Unreachable(format!("chat.getPermalink: {e}")))?;
        if json["ok"].as_bool() != Some(true) {
            return Err(refused(json["error"].as_str().unwrap_or("unknown error")));
        }
        json["permalink"]
            .as_str()
            .filter(|u| u.starts_with("https://") && !u.contains(['|', '<', '>', ' ']))
            .map(String::from)
            .ok_or_else(|| refused("no permalink"))
    }

    /// `users.info` (scopes `users:read`, `users:read.email`): the user object, with
    /// `real_name` and `profile.email`.
    pub async fn user_info(&self, user: &str) -> Result<Value, SlackError> {
        let res = self
            .http
            .get(format!("{}/api/users.info", self.base))
            .bearer_auth(&self.token)
            .query(&[("user", user)])
            .send()
            .await
            .map_err(|e| SlackError::Unreachable(e.to_string()))?;
        let json: Value = res
            .json()
            .await
            .map_err(|e| SlackError::Unreachable(format!("users.info: {e}")))?;
        if json["ok"].as_bool() != Some(true) {
            return Err(SlackError::Refused {
                method: "users.info".into(),
                error: json["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        Ok(json["user"].clone())
    }

    /// The user's IANA time zone from `users.info` (scope `users:read`), if Slack has one.
    pub async fn user_tz(&self, user: &str) -> Option<String> {
        let res = self
            .http
            .get(format!("{}/api/users.info", self.base))
            .bearer_auth(&self.token)
            .query(&[("user", user)])
            .send()
            .await
            .ok()?;
        let json: Value = res.json().await.ok()?;
        if json["ok"].as_bool() != Some(true) {
            tracing::info!(error = %json["error"], "users.info failed; using the default time zone");
            return None;
        }
        json["user"]["tz"]
            .as_str()
            .filter(|tz| !tz.is_empty())
            .map(String::from)
    }
}
