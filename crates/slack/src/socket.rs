//! Slack Socket Mode: the bot dials out to Slack, so the Spark needs no inbound connection.
//!
//! 1. `apps.connections.open` with the app-level token (`xapp-…`) returns a `wss://` URL.
//! 2. Slack sends `hello`, then envelopes `{envelope_id, type, payload, …}`.
//! 3. Each envelope is acknowledged with `{envelope_id, payload?}` within 3 seconds.
//! 4. A `disconnect` (`warning`, `refresh_requested`, `link_disabled`) means reconnect.
//!    Slack refreshes connections every few hours, so this is normal.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::commands::Commands;

#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    #[error("apps.connections.open failed: {0}")]
    Open(String),
    #[error("websocket: {0}")]
    Ws(String),
}

pub struct Socket {
    /// `https://slack.com`
    pub api_base: String,
    pub app_token: String,
    pub commands: Commands,
    pub http: reqwest::Client,
}

/// Why one connection ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Slack asked us to reconnect, or closed the socket.
    Reconnect,
    /// Socket Mode was turned off for the app: stop.
    Disabled,
}

impl Socket {
    async fn open(&self) -> Result<String, SocketError> {
        let res: Value = self
            .http
            .post(format!(
                "{}/api/apps.connections.open",
                self.api_base.trim_end_matches('/')
            ))
            .bearer_auth(&self.app_token)
            .send()
            .await
            .map_err(|e| SocketError::Open(e.to_string()))?
            .json()
            .await
            .map_err(|e| SocketError::Open(e.to_string()))?;
        if res["ok"].as_bool() != Some(true) {
            return Err(SocketError::Open(
                res["error"].as_str().unwrap_or("unknown error").to_string(),
            ));
        }
        res["url"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| SocketError::Open("no url".into()))
    }

    /// One connection, until Slack asks to reconnect or the socket drops.
    pub async fn run_once(&self) -> Result<Ended, SocketError> {
        let url = self.open().await?;
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| SocketError::Ws(e.to_string()))?;
        let (mut tx, mut rx) = ws.split();
        while let Some(msg) = rx.next().await {
            let msg = msg.map_err(|e| SocketError::Ws(e.to_string()))?;
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Ping(p) => {
                    tx.send(Message::Pong(p))
                        .await
                        .map_err(|e| SocketError::Ws(e.to_string()))?;
                    continue;
                }
                Message::Close(_) => return Ok(Ended::Reconnect),
                _ => continue,
            };
            let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
                tracing::warn!("non-JSON Socket Mode message ignored");
                continue;
            };
            match envelope["type"].as_str() {
                Some("hello") => tracing::info!("Socket Mode connected"),
                Some("disconnect") => {
                    let reason = envelope["reason"].as_str().unwrap_or_default();
                    tracing::info!(reason, "Slack asked to reconnect");
                    return Ok(if reason == "link_disabled" {
                        Ended::Disabled
                    } else {
                        Ended::Reconnect
                    });
                }
                _ => {}
            }
            let Some(envelope_id) = envelope["envelope_id"].as_str() else {
                continue;
            };
            let ack = match envelope["type"].as_str() {
                Some("slash_commands") => {
                    let reply = self.commands.handle(&envelope["payload"], envelope_id);
                    if let Some(work) = reply.follow_up {
                        tokio::spawn(work);
                    }
                    json!({"envelope_id": envelope_id, "payload": reply.ack})
                }
                // Events and interactions arrive with the full bot (Phase 1). Acknowledge so
                // Slack doesn't retry them.
                _ => json!({"envelope_id": envelope_id}),
            };
            tx.send(Message::Text(ack.to_string().into()))
                .await
                .map_err(|e| SocketError::Ws(e.to_string()))?;
        }
        Ok(Ended::Reconnect)
    }

    /// Stay connected: reconnect when asked, back off on errors (1 s doubling to 60 s),
    /// stop if Socket Mode is turned off.
    pub async fn run(&self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.run_once().await {
                Ok(Ended::Reconnect) => backoff = Duration::from_secs(1),
                Ok(Ended::Disabled) => {
                    tracing::error!("Socket Mode is disabled for this Slack app; stopping");
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, wait_s = backoff.as_secs(), "Socket Mode error; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
}
