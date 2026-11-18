//! A fake Slack for the Socket Mode tests: `apps.connections.open`, the Socket Mode
//! websocket (scripted per connection, or fed live by the test), and the Web API calls the
//! bot makes (`chat.postMessage`, `chat.update`, `chat.postEphemeral`, `views.open`,
//! `views.publish`, `users.info`, `conversations.open`, `chat.getPermalink`), each recorded
//! with when it arrived.
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
    /// `users.info` user objects by id (name, `profile.email`), for onboarding.
    pub users: Mutex<HashMap<String, Value>>,
    /// `chat.getPermalink` answers `channel_not_found` for these channels.
    pub no_permalink: Mutex<Vec<String>>,
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
        .route("/api/chat.getPermalink", get(permalink))
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
        // The app's DM with a user: `U…` → `D…`, as `chat.postMessage` does.
        "conversations.open" => {
            let user = body["users"].as_str().unwrap_or_default();
            match user.strip_prefix('U') {
                Some(rest) => Json(json!({"ok": true, "channel": {"id": format!("D{rest}")}})),
                None => Json(json!({"ok": false, "error": "user_not_found"})),
            }
        }
        "views.open" => Json(json!({"ok": true})),
        "views.publish" => Json(json!({"ok": true, "view": body["view"]})),
        _ => Json(json!({"ok": false, "error": "unknown_method"})),
    }
}

async fn permalink(
    State(s): State<Arc<FakeSlack>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let channel = q.get("channel").cloned().unwrap_or_default();
    let ts = q.get("message_ts").cloned().unwrap_or_default();
    s.calls.lock().unwrap().push(Call {
        at: Instant::now(),
        method: "chat.getPermalink".into(),
        body: json!({"channel": channel, "message_ts": ts}),
    });
    if s.no_permalink.lock().unwrap().contains(&channel) {
        return Json(json!({"ok": false, "error": "channel_not_found"}));
    }
    Json(json!({
        "ok": true, "channel": channel,
        "permalink": format!("https://acme.slack.com/archives/{channel}/p{}", ts.replace('.', "")),
    }))
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
    if let Some(found) = s.users.lock().unwrap().get(&user) {
        let mut found = found.clone();
        found["id"] = json!(user);
        return Json(json!({"ok": true, "user": found}));
    }
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

// ---- The user directory (slice U4b) ----

/// A seeded user directory for a test stack: what the tool server's `GET /internal/directory`
/// would answer the Slack bot token, in memory, on a clock that moves only when told.
pub struct Directory {
    pub fetcher: Arc<pmos_directory::snapshot::StaticFetcher>,
    pub clock: Arc<pmos_directory::snapshot::FakeClock>,
    pub client: Arc<pmos_directory::snapshot::SnapshotClient>,
}

impl Directory {
    /// The directory with `users`, already fetched once.
    pub async fn seeded(users: Vec<pmos_directory::snapshot::DirectoryUser>) -> Self {
        let fetcher = Arc::new(pmos_directory::snapshot::StaticFetcher::new(users));
        let clock = Arc::new(pmos_directory::snapshot::FakeClock::default());
        let client = Arc::new(pmos_directory::snapshot::SnapshotClient::new(
            fetcher.clone(),
            clock.clone(),
        ));
        client
            .refresh()
            .await
            .expect("the seeded directory answers");
        Self {
            fetcher,
            clock,
            client,
        }
    }

    pub fn people(&self) -> pmos_slack::people::People {
        pmos_slack::people::People::new(self.client.clone())
    }

    /// Change one user (or add them) and poll, as the service would within 10 seconds.
    pub async fn upsert(&self, user: pmos_directory::snapshot::DirectoryUser) {
        self.fetcher.upsert(user);
        self.client.refresh().await.expect("the directory answers");
    }
}

/// An active user with a Slack id and `roles`.
pub fn person(
    handle: &str,
    slack_id: &str,
    roles: &[&str],
) -> pmos_directory::snapshot::DirectoryUser {
    pmos_directory::snapshot::DirectoryUser {
        slack_id: Some(slack_id.to_string()),
        ..pmos_directory::snapshot::user(handle, roles)
    }
}

/// The same user, deactivated.
pub fn deactivated(
    user: pmos_directory::snapshot::DirectoryUser,
) -> pmos_directory::snapshot::DirectoryUser {
    pmos_directory::snapshot::DirectoryUser {
        status: pmos_directory::snapshot::UserStatus::Deactivated,
        ..user
    }
}
