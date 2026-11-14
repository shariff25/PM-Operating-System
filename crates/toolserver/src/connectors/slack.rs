//! Slack, called with the PM's own user token (`xoxp`), never a bot token: reads see what
//! the PM can see, and posts appear as the PM, marked as drafted with PM OS (SLK-7).
//!
//! Methods used (checked against docs.slack.dev, November 2026):
//! - `assistant.search.context`: the Real-time Search API. Results are never stored: they
//!   go back to the model for this call only, and the audit log keeps ids only.
//! - `conversations.replies`: a thread.
//! - `chat.postMessage`: a reply (`thread_ts`) or a message.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::http::{arg, client, limit};
use super::{Caller, Connector, ConnectorError, Item, SourceRef, DRAFTED_MARKER};
use crate::oauth::TokenSource;
use crate::vault::Provider;

pub struct Slack {
    tokens: Arc<dyn TokenSource>,
    /// `https://slack.com`
    base: String,
    http: reqwest::Client,
}

/// A Slack conversation id (`C…`, `G…`, `D…`) or user id (`U…`, `W…`).
fn slack_id(value: &str) -> Result<&str, ConnectorError> {
    if !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a Slack id"
        )))
    }
}

/// A message timestamp, such as `1731500000.000100`.
fn slack_ts(value: &str) -> Result<&str, ConnectorError> {
    let ok = value.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty() && !b.is_empty() && (a.to_string() + b).chars().all(|c| c.is_ascii_digit())
    });
    if ok {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a Slack message timestamp"
        )))
    }
}

fn archive_url(channel: &str, ts: &str) -> String {
    format!(
        "https://slack.com/archives/{channel}/p{}",
        ts.replace('.', "")
    )
}

/// Text as posted: the PM's words plus the marker.
pub fn with_marker(text: &str) -> String {
    format!("{}\n\n_{DRAFTED_MARKER}_", text.trim_end())
}

impl Slack {
    pub fn new(tokens: Arc<dyn TokenSource>, base: impl Into<String>) -> Self {
        Self {
            tokens,
            base: base.into(),
            http: client(),
        }
    }

    /// Call a Web API method. Slack reports most errors as `ok: false` in a 200 response.
    async fn call(
        &self,
        user: &str,
        method: &str,
        form: &[(&str, &str)],
        what: &str,
    ) -> Result<Value, ConnectorError> {
        let token = self.tokens.token(user, Provider::Slack).await?;
        let res = self
            .http
            .post(format!("{}/api/{method}", self.base.trim_end_matches('/')))
            .bearer_auth(token.token.expose())
            .form(form)
            .send()
            .await
            .map_err(|e| ConnectorError::Unavailable(format!("Slack: {e}")))?;
        if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = res
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            return Err(ConnectorError::RateLimited(wait));
        }
        if !res.status().is_success() {
            return Err(ConnectorError::Unavailable(format!(
                "Slack returned {}",
                res.status()
            )));
        }
        let body: Value = res
            .json()
            .await
            .map_err(|e| ConnectorError::Unavailable(format!("Slack: bad response: {e}")))?;
        if body["ok"].as_bool() == Some(true) {
            return Ok(body);
        }
        let error = body["error"].as_str().unwrap_or("unknown_error");
        Err(match error {
            "invalid_auth" | "not_authed" | "token_revoked" | "token_expired"
            | "account_inactive" => ConnectorError::Reconnect(
                "Slack rejected your sign-in; reconnect Slack in PM OS".into(),
            ),
            "missing_scope" => ConnectorError::Reconnect(
                "PM OS needs more Slack permissions; reconnect Slack in PM OS".into(),
            ),
            "channel_not_found" | "thread_not_found" | "message_not_found" => {
                ConnectorError::NotFound(what.to_string())
            }
            "not_in_channel" | "access_denied" | "restricted_action" | "is_archived" => {
                ConnectorError::Forbidden(format!("{what} ({error})"))
            }
            "ratelimited" => ConnectorError::RateLimited(30),
            other => ConnectorError::BadArgs(format!("Slack refused {what}: {other}")),
        })
    }

    async fn post(
        &self,
        caller: Caller<'_>,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SourceRef, ConnectorError> {
        let text = with_marker(text);
        let mut form = vec![("channel", channel), ("text", text.as_str())];
        if let Some(ts) = thread_ts {
            form.push(("thread_ts", ts));
        }
        let body = self
            .call(
                caller.user,
                "chat.postMessage",
                &form,
                &format!("channel {channel}"),
            )
            .await?;
        let posted_channel = body["channel"].as_str().unwrap_or(channel);
        let ts = body["ts"].as_str().unwrap_or_default();
        Ok(SourceRef {
            system: "slack".into(),
            id: format!("{posted_channel}:{ts}"),
            url: archive_url(posted_channel, ts),
        })
    }
}

#[async_trait]
impl Connector for Slack {
    fn system(&self) -> &str {
        "slack"
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        match action {
            "search" => {
                let query = arg(args, "query")?;
                let max = limit(args, 10, 20).to_string();
                let body = self
                    .call(
                        caller.user,
                        "assistant.search.context",
                        &[
                            ("query", query),
                            ("channel_types", "public_channel,private_channel,mpim,im"),
                            ("content_types", "messages"),
                            ("limit", max.as_str()),
                        ],
                        "the search",
                    )
                    .await?;
                Ok(body["results"]["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| {
                        let channel = m["channel_id"].as_str()?;
                        let ts = m["message_ts"].as_str()?;
                        Some(Item {
                            source: SourceRef {
                                system: "slack".into(),
                                id: format!("{channel}:{ts}"),
                                url: m["permalink"]
                                    .as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| archive_url(channel, ts)),
                            },
                            title: format!(
                                "Message in {}",
                                m["channel_name"].as_str().unwrap_or(channel)
                            ),
                            content: json!({
                                "author": m["author_name"].as_str().or(m["author_user_id"].as_str()),
                                "is_bot": m["is_author_bot"],
                                "text": m["content"],
                            }),
                        })
                    })
                    .collect())
            }
            "read_thread" => {
                let channel = slack_id(arg(args, "channel")?)?;
                let ts = slack_ts(arg(args, "ts")?)?;
                let body = self
                    .call(
                        caller.user,
                        "conversations.replies",
                        &[("channel", channel), ("ts", ts), ("limit", "100")],
                        &format!("thread {channel}:{ts}"),
                    )
                    .await?;
                let messages: Vec<Value> = body["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| json!({"user": m["user"], "ts": m["ts"], "text": m["text"]}))
                    .collect();
                Ok(vec![Item {
                    source: SourceRef {
                        system: "slack".into(),
                        id: format!("{channel}:{ts}"),
                        url: archive_url(channel, ts),
                    },
                    title: format!("Thread in {channel}"),
                    content: json!({ "messages": messages }),
                }])
            }
            other => Err(ConnectorError::Unsupported(format!("slack.{other}"))),
        }
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        let channel = slack_id(arg(args, "channel")?)?;
        let text = arg(args, "text")?;
        match action {
            "post_reply" => {
                let ts = slack_ts(arg(args, "thread_ts")?)?;
                self.post(caller, channel, Some(ts), text).await
            }
            "send_message" => self.post(caller, channel, None, text).await,
            other => Err(ConnectorError::Unsupported(format!("slack.{other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_marker() {
        assert!(slack_id("C0FIELD1").is_ok());
        assert!(slack_id("c0field").is_err());
        assert!(slack_id("C1&x=1").is_err());
        assert!(slack_ts("1731500000.000100").is_ok());
        assert!(slack_ts("1731500000").is_err());
        assert!(slack_ts("17315.00a").is_err());
        assert_eq!(
            with_marker("Yes, SSO ships in 4.2.\n"),
            "Yes, SSO ships in 4.2.\n\n_Drafted with PM OS_"
        );
        assert_eq!(
            archive_url("C1", "1731500000.000100"),
            "https://slack.com/archives/C1/p1731500000000100"
        );
    }
}
