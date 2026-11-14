//! A fake Slack for the Socket Mode tests: `apps.connections.open`, the Socket Mode
//! websocket (scripted per connection, or fed live by the test), and the Web API calls the
//! bot makes (`chat.postMessage`, `chat.update`, `chat.postEphemeral`, `views.open`,
//! `users.info`), each recorded with when it arrived.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const APP_TOKEN: &str = "xapp-1-test";
pub const SLACK_BOT_TOKEN: &str = "xoxb-test";

/// One Web API call.
#[derive(Debug, Clone)]
pub struct Call {
    pub at: Instant,
    pub method: String,
    pub body: Value,
}

#[derive(Default)]
pub struct FakeSlack {
    pub base: Mutex<String>,
    pub opens: Mutex<usize>,
    /// Messages to send on each successive connection.
    pub scripts: Mutex<VecDeque<Vec<Value>>>,
    /// When there's no script left: envelopes the test sends while the bot is connected.
    pub live: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    /// Envelopes sent to the bot, with when.
    pub sent: Mutex<Vec<(Instant, Value)>>,
    pub acks: Mutex<Vec<(Instant, Value)>>,
    pub responses: Mutex<Vec<(Instant, Value)>>,
    pub calls: Mutex<Vec<Call>>,
    next_ts: Mutex<u64>,
    /// `users.info` time zones by user id; anyone else is `user_not_found`.
    pub tz: Mutex<HashMap<String, String>>,
}

impl FakeSlack {
    /// The Web API calls of one method so far.
    pub fn calls(&self, method: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.method == method)
            .cloned()
            .collect()
    }

    pub fn ack(&self, envelope_id: &str) -> Option<(Instant, Value)> {
        self.acks
            .lock()
            .unwrap()
            .iter()
            .find(|(_, a)| a["envelope_id"] == envelope_id)
            .cloned()
    }

    pub fn sent_at(&self, envelope_id: &str) -> Option<Instant> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .find(|(_, e)| e["envelope_id"] == envelope_id)
            .map(|(at, _)| *at)
    }
}

/// The routes: Socket Mode, and the Web API under `/api/`.
pub fn routes() -> Router<Arc<FakeSlack>> {
    Router::new()
        .route("/api/apps.connections.open", post(open))
        .route("/api/users.info", get(users_info))
        .route("/api/{method}", post(web_api))
        .route("/socket", get(socket))
}

async fn open(State(s): State<Arc<FakeSlack>>, headers: HeaderMap) -> Json<Value> {
    if bearer(&headers) != APP_TOKEN {
        return Json(json!({"ok": false, "error": "invalid_auth"}));
    }
    *s.opens.lock().unwrap() += 1;
    let base = s.base.lock().unwrap().replace("http://", "ws://");
    Json(json!({"ok": true, "url": format!("{base}/socket")}))
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string()
}

async fn web_api(
    State(s): State<Arc<FakeSlack>>,
    Path(method): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    if bearer(&headers) != SLACK_BOT_TOKEN {
        return Json(json!({"ok": false, "error": "invalid_auth"}));
    }
    s.calls.lock().unwrap().push(Call {
        at: Instant::now(),
        method: method.clone(),
        body: body.clone(),
    });
    match method.as_str() {
        "chat.postMessage" => {
            let ts = {
                let mut n = s.next_ts.lock().unwrap();
                *n += 1;
                format!("1700000000.{:06}", *n)
            };
            // Posting to a user id opens the app's DM with them.
            let channel = body["channel"].as_str().unwrap_or_default();
            let channel = match channel.strip_prefix('U') {
                Some(rest) => format!("D{rest}"),
                None => channel.to_string(),
            };
            Json(json!({"ok": true, "channel": channel, "ts": ts}))
        }
        "chat.update" => Json(json!({"ok": true, "channel": body["channel"], "ts": body["ts"]})),
        "chat.postEphemeral" => Json(json!({"ok": true, "message_ts": "1.0"})),
        "views.open" => Json(json!({"ok": true})),
        _ => Json(json!({"ok": false, "error": "unknown_method"})),
    }
}

async fn users_info(
    State(s): State<Arc<FakeSlack>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let user = q.get("user").cloned().unwrap_or_default();
    s.calls.lock().unwrap().push(Call {
        at: Instant::now(),
        method: "users.info".into(),
        body: json!({"user": user}),
    });
    match s.tz.lock().unwrap().get(&user) {
        Some(tz) => Json(json!({"ok": true, "user": {"id": user, "tz": tz}})),
        None => Json(json!({"ok": false, "error": "user_not_found"})),
    }
}

async fn socket(State(s): State<Arc<FakeSlack>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| play(s, socket))
}

/// Send one envelope and record its acknowledgement. `false` when the connection ends.
async fn exchange(s: &FakeSlack, ws: &mut WebSocket, msg: Value) -> bool {
    s.sent.lock().unwrap().push((Instant::now(), msg.clone()));
    if ws
        .send(Message::Text(msg.to_string().into()))
        .await
        .is_err()
    {
        return false;
    }
    // Read the acknowledgement for this envelope.
    loop {
        match ws.recv().await {
            Some(Ok(Message::Text(t))) => {
                s.acks
                    .lock()
                    .unwrap()
                    .push((Instant::now(), serde_json::from_str(&t).unwrap()));
                return true;
            }
            Some(Ok(_)) => continue,
            _ => return false,
        }
    }
}

async fn play(s: Arc<FakeSlack>, mut ws: WebSocket) {
    let script = s.scripts.lock().unwrap().pop_front();
    let hello = json!({"type": "hello", "num_connections": 1});
    if ws
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let Some(script) = script else {
        let live = s.live.lock().unwrap().take();
        if let Some(mut live) = live {
            while let Some(msg) = live.recv().await {
                let is_disconnect = msg["type"] == "disconnect";
                if is_disconnect {
                    let _ = ws.send(Message::Text(msg.to_string().into())).await;
                    return;
                }
                if !exchange(&s, &mut ws, msg).await {
                    return;
                }
            }
        }
        return;
    };
    for msg in script {
        if msg["type"] == "test_go_silent" {
            // A connection that died without closing: nothing more arrives, nothing is read.
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
        if msg["type"] == "disconnect" {
            // Give the bot time to send the acks for what came before.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = ws.send(Message::Text(msg.to_string().into())).await;
            return;
        }
        if !exchange(&s, &mut ws, msg).await {
            return;
        }
    }
}

pub async fn serve(app: Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, cancel)
}

/// Poll `f` until it returns something, or fail the test after 10 seconds.
pub async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}
