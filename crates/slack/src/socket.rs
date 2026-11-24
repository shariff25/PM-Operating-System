//! Slack Socket Mode: the bot dials out to Slack, so PM OS's host needs no inbound connection.
//!
//! 1. `apps.connections.open` with the app-level token (`xapp-…`) returns a `wss://` URL.
//! 2. Slack sends `hello`, then envelopes `{envelope_id, type, payload, …}`.
//! 3. Each envelope is acknowledged with `{envelope_id, payload?}` within 3 seconds.
//! 4. A `disconnect` (`warning`, `refresh_requested`, `link_disabled`) means reconnect.
//!    Slack refreshes connections every few hours, so this is normal.
//!
//! The acknowledgement goes first; slow work (a `/docs` search, a DM becoming a task, an
//! approval being decided) runs in a follow-up after it. An interaction whose answer has to
//! be in the ack (the memory-repo modal's field errors) is worked on beside the loop, and its
//! ack is sent when ready, at most [`LATE_ACK_WITHIN`] after it arrived; other envelopes are
//! acknowledged meanwhile.
//!
//! A connection can die silently (a NAT or firewall drops it). The bot pings every
//! `ping_every` and reconnects if nothing at all arrives for `read_deadline`.

use std::sync::Mutex;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::bots::Bots;
use crate::commands::{Commands, FollowUp};

/// The longest a late ack waits before it is sent without an answer (Slack wants every ack
/// within 3 seconds).
pub const LATE_ACK_WITHIN: Duration = Duration::from_millis(2500);

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
    /// DMs, approval cards and routines. Without it only `/docs` works; events and
    /// interactions are acknowledged and dropped.
    pub bots: Option<Bots>,
    pub http: reqwest::Client,
    /// Reconnect if nothing arrives for this long. Default 90 s.
    pub read_deadline: Duration,
    /// Send a ping this often, so a live connection always has traffic. Default 30 s.
    pub ping_every: Duration,
    /// Replies still being worked on, so shutdown can wait for them.
    follow_ups: Mutex<JoinSet<()>>,
}

/// Why one connection ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Slack asked us to reconnect, closed the socket, or went silent.
    Reconnect,
    /// Socket Mode was turned off for the app: stop.
    Disabled,
    /// The process is shutting down.
    Shutdown,
}

impl Socket {
    pub fn new(
        api_base: impl Into<String>,
        app_token: impl Into<String>,
        commands: Commands,
        http: reqwest::Client,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            app_token: app_token.into(),
            commands,
            bots: None,
            http,
            read_deadline: Duration::from_secs(90),
            ping_every: Duration::from_secs(30),
            follow_ups: Mutex::new(JoinSet::new()),
        }
    }

    /// Serve DMs, approval cards and routines too.
    pub fn with_bots(mut self, bots: Bots) -> Self {
        self.bots = Some(bots);
        self
    }

    fn spawn_follow_up(&self, work: FollowUp) {
        let mut set = self.follow_ups.lock().expect("follow-ups lock");
        while set.try_join_next().is_some() {}
        set.spawn(work);
    }

    /// Wait up to `deadline` for replies still in flight. Returns how many were cut off.
    pub async fn drain(&self, deadline: Duration) -> usize {
        let mut set = std::mem::take(&mut *self.follow_ups.lock().expect("follow-ups lock"));
        let finished =
            tokio::time::timeout(deadline, async { while set.join_next().await.is_some() {} })
                .await;
        if finished.is_err() {
            let left = set.len();
            set.abort_all();
            left
        } else {
            0
        }
    }

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

    /// One connection, until Slack asks to reconnect, the socket drops or goes silent, or
    /// `shutdown` fires.
    pub async fn run_once(&self, shutdown: &CancellationToken) -> Result<Ended, SocketError> {
        let url = self.open().await?;
        let proxy = crate::proxy::proxy_for(&url, |k| std::env::var(k).ok());
        let ws = crate::proxy::connect(&url, proxy.as_ref())
            .await
            .map_err(SocketError::Ws)?;
        let (mut tx, mut rx) = ws.split();
        let mut ping = tokio::time::interval(self.ping_every);
        ping.tick().await;
        let mut last_heard = tokio::time::Instant::now();
        // Acks that waited for an answer, sent on this connection when ready.
        let (late_tx, mut late_rx) = mpsc::unbounded_channel::<Value>();
        loop {
            let msg = tokio::select! {
                _ = shutdown.cancelled() => {
                    let _ = tx.send(Message::Close(None)).await;
                    return Ok(Ended::Shutdown);
                }
                Some(ack) = late_rx.recv() => {
                    tx.send(Message::Text(ack.to_string().into()))
                        .await
                        .map_err(|e| SocketError::Ws(e.to_string()))?;
                    continue;
                }
                _ = ping.tick() => {
                    tx.send(Message::Ping(Default::default()))
                        .await
                        .map_err(|e| SocketError::Ws(e.to_string()))?;
                    continue;
                }
                _ = tokio::time::sleep_until(last_heard + self.read_deadline) => {
                    tracing::warn!(
                        silent_s = self.read_deadline.as_secs(),
                        "nothing from Slack; reconnecting"
                    );
                    return Ok(Ended::Reconnect);
                }
                next = rx.next() => match next {
                    Some(msg) => msg.map_err(|e| SocketError::Ws(e.to_string()))?,
                    None => return Ok(Ended::Reconnect),
                },
            };
            last_heard = tokio::time::Instant::now();
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
            let payload = &envelope["payload"];
            if envelope["type"] == "interactive" {
                if let Some(late) = self.bots.as_ref().and_then(|b| b.late_ack(payload)) {
                    let late_tx = late_tx.clone();
                    let envelope_id = envelope_id.to_string();
                    self.spawn_follow_up(Box::pin(async move {
                        let (reply, work) = tokio::time::timeout(LATE_ACK_WITHIN, late)
                            .await
                            .unwrap_or_else(|_| {
                                tracing::warn!(
                                    "an answer for an ack took too long; acking without it"
                                );
                                (None, None)
                            });
                        let mut ack = json!({"envelope_id": envelope_id});
                        if let Some(reply) = reply {
                            ack["payload"] = reply;
                        }
                        // The connection may have gone meanwhile; Slack then retries.
                        let _ = late_tx.send(ack);
                        if let Some(work) = work {
                            work.await;
                        }
                    }));
                    continue;
                }
            }
            let (ack, follow_up) = match (envelope["type"].as_str(), &self.bots) {
                (Some("slash_commands"), _) => {
                    let reply = self.commands.handle(payload, envelope_id);
                    (
                        json!({"envelope_id": envelope_id, "payload": reply.ack}),
                        reply.follow_up,
                    )
                }
                (Some("events_api"), Some(bots)) => {
                    (json!({"envelope_id": envelope_id}), bots.event(payload))
                }
                (Some("interactive"), Some(bots)) => {
                    let (reply, work) = bots.interactive(payload);
                    let mut ack = json!({"envelope_id": envelope_id});
                    if let Some(reply) = reply {
                        ack["payload"] = reply;
                    }
                    (ack, work)
                }
                // Anything else is acknowledged, so Slack doesn't retry it.
                _ => (json!({"envelope_id": envelope_id}), None),
            };
            let sent = tx.send(Message::Text(ack.to_string().into())).await;
            // The work runs even if the ack was lost: a retried event is then recognized and
            // skipped, rather than handled twice or not at all.
            if let Some(work) = follow_up {
                self.spawn_follow_up(work);
            }
            sent.map_err(|e| SocketError::Ws(e.to_string()))?;
        }
    }

    /// Stay connected: reconnect when asked, back off on errors (1 s doubling to 60 s).
    /// Returns [`Ended::Disabled`] if Socket Mode is turned off, [`Ended::Shutdown`] when
    /// `shutdown` fires.
    pub async fn run(&self, shutdown: CancellationToken) -> Ended {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.run_once(&shutdown).await {
                Ok(Ended::Reconnect) => backoff = Duration::from_secs(1),
                Ok(Ended::Disabled) => {
                    tracing::error!("Socket Mode is disabled for this Slack app; stopping");
                    return Ended::Disabled;
                }
                Ok(Ended::Shutdown) => return Ended::Shutdown,
                Err(e) => {
                    tracing::warn!(error = %e, wait_s = backoff.as_secs(), "Socket Mode error; retrying");
                    tokio::select! {
                        _ = shutdown.cancelled() => return Ended::Shutdown,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
}
