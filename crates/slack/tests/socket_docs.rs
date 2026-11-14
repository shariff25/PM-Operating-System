//! `/docs` end to end: a fake Slack (Socket Mode websocket, `apps.connections.open`, a
//! `response_url` catcher) and a real tool server with the fake docs connector.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use pmos_slack::commands::Commands;
use pmos_slack::socket::{Ended, Socket};
use pmos_slack::toolserver::ToolServer;
use pmos_toolserver::audit::MemoryAudit;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const APP_TOKEN: &str = "xapp-1-test";

#[derive(Default)]
struct FakeSlack {
    base: Mutex<String>,
    opens: Mutex<usize>,
    /// Messages to send on each successive connection.
    scripts: Mutex<VecDeque<Vec<Value>>>,
    acks: Mutex<Vec<(Instant, Value)>>,
    responses: Mutex<Vec<(Instant, Value)>>,
}

async fn open(State(s): State<Arc<FakeSlack>>, headers: HeaderMap) -> Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth != format!("Bearer {APP_TOKEN}") {
        return Json(json!({"ok": false, "error": "invalid_auth"}));
    }
    *s.opens.lock().unwrap() += 1;
    let base = s.base.lock().unwrap().replace("http://", "ws://");
    Json(json!({"ok": true, "url": format!("{base}/socket")}))
}

async fn socket(State(s): State<Arc<FakeSlack>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| play(s, socket))
}

async fn play(s: Arc<FakeSlack>, mut ws: WebSocket) {
    let script = s.scripts.lock().unwrap().pop_front().unwrap_or_default();
    let hello = json!({"type": "hello", "num_connections": 1});
    if ws
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    for msg in script {
        if msg["type"] == "test_go_silent" {
            // A connection that died without closing: nothing more arrives, nothing is read.
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
        let is_disconnect = msg["type"] == "disconnect";
        if is_disconnect {
            // Give the bot time to send the acks for what came before.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if ws
            .send(Message::Text(msg.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        if is_disconnect {
            return;
        }
        // Read the acknowledgement for this envelope.
        if let Some(Ok(Message::Text(t))) = ws.recv().await {
            s.acks
                .lock()
                .unwrap()
                .push((Instant::now(), serde_json::from_str(&t).unwrap()));
        }
    }
}

async fn response(State(s): State<Arc<FakeSlack>>, Json(body): Json<Value>) -> Json<Value> {
    s.responses.lock().unwrap().push((Instant::now(), body));
    Json(json!({"ok": true}))
}

/// A `response_url` that refuses Block Kit, as Slack does for invalid blocks.
async fn picky(
    State(s): State<Arc<FakeSlack>>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, &'static str) {
    if body.get("blocks").is_some() {
        return (axum::http::StatusCode::BAD_REQUEST, "invalid_blocks");
    }
    s.responses.lock().unwrap().push((Instant::now(), body));
    (axum::http::StatusCode::OK, "ok")
}

async fn serve(app: Router) -> (String, CancellationToken) {
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

fn slash(envelope_id: &str, user: &str, text: &str, response_url: &str) -> Value {
    json!({
        "envelope_id": envelope_id,
        "type": "slash_commands",
        "accepts_response_payload": true,
        "payload": {
            "command": "/docs", "text": text, "user_id": user, "channel_id": "C0FIELD",
            "response_url": response_url, "team_id": "T1",
        },
    })
}

async fn start_toolserver() -> (
    String,
    Arc<MemoryAudit>,
    CancellationToken,
    CancellationToken,
) {
    // Tool server with the repo's content (it has the `docs` skill) and fake connectors.
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let catalog = Catalog::load(&content, Utc::now().date_naive()).unwrap();
    let (stores, audit): (Stores, Arc<MemoryAudit>) = Stores::in_memory(&content);
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = Arc::new(ToolService::new(catalog, stores, connectors).unwrap());
    let ts_cancel = CancellationToken::new();
    let (ts_base, stop_ts) = serve(router(
        svc,
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(pmos_toolserver::app::BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            content_root: content,
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: None,
        },
        ts_cancel.clone(),
    ))
    .await;

    (ts_base, audit, stop_ts, ts_cancel)
}

#[tokio::test]
async fn docs_command_over_socket_mode() {
    let (ts_base, audit, stop_ts, ts_cancel) = start_toolserver().await;

    // Fake Slack.
    let slack = Arc::new(FakeSlack::default());
    let app = Router::new()
        .route("/api/apps.connections.open", post(open))
        .route("/socket", get(socket))
        .route("/hooks/response", post(response))
        .with_state(slack.clone());
    let (slack_base, stop_slack) = serve(app).await;
    *slack.base.lock().unwrap() = slack_base.clone();
    let hook = format!("{slack_base}/hooks/response");
    slack.scripts.lock().unwrap().extend([
        vec![
            slash("e1", "U0ANA", "SAML SSO", &hook),
            slash("e2", "U0STRANGER", "SAML", &hook),
            slash("e3", "U0ANA", "", &hook),
            json!({"envelope_id": "e4", "type": "events_api", "payload": {"event": {"type": "app_mention"}}}),
            json!({"type": "disconnect", "reason": "refresh_requested"}),
        ],
        vec![
            slash("e5", "U0ANA", "product:dsm key rotation", &hook),
            json!({"type": "disconnect", "reason": "link_disabled"}),
        ],
    ]);

    // The team: only Ana.
    let dir = tempfile::tempdir().unwrap();
    let team_file = dir.path().join("team.yaml");
    std::fs::write(
        &team_file,
        "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n",
    )
    .unwrap();

    let http = reqwest::Client::new();
    let bot = Socket::new(
        slack_base.clone(),
        APP_TOKEN,
        Commands {
            toolserver: ToolServer::new(ts_base, BOT_TOKEN),
            team_file,
            http: http.clone(),
            response_url_prefix: format!("{slack_base}/hooks/"),
        },
        http,
    );
    // `link_disabled` ends the loop; a hang here fails the test.
    let ended = tokio::time::timeout(Duration::from_secs(20), bot.run(CancellationToken::new()))
        .await
        .expect("the bot stops when Socket Mode is disabled");
    assert_eq!(ended, Ended::Disabled);
    assert_eq!(
        *slack.opens.lock().unwrap(),
        2,
        "reconnected after refresh_requested"
    );

    // Wait for the follow-ups.
    for _ in 0..50 {
        if slack.responses.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let acks = slack.acks.lock().unwrap().clone();
    let by_id = |id: &str| {
        acks.iter()
            .find(|(_, a)| a["envelope_id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no ack for {id}"))
    };
    let (ack1_at, ack1) = by_id("e1");
    assert!(ack1["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("Searching"));
    assert_eq!(ack1["payload"]["response_type"], "ephemeral");
    assert!(by_id("e2").1["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("PM OS is for the Fortanix product team"));
    assert!(by_id("e3").1["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("/docs <"));
    assert!(
        by_id("e4").1.get("payload").is_none(),
        "events are acked, not answered"
    );

    let responses = slack.responses.lock().unwrap().clone();
    assert_eq!(
        responses.len(),
        2,
        "one reply per search, none for the stranger"
    );
    let (first_at, first) = responses
        .iter()
        .find(|(_, r)| r["text"].as_str().unwrap_or_default().contains("SAML SSO"))
        .cloned()
        .expect("SAML results");
    assert!(first_at >= ack1_at, "ack first, results after");
    assert_eq!(first["response_type"], "ephemeral");
    let blocks = serde_json::to_string(&first["blocks"]).unwrap();
    assert!(blocks.contains(
        "<https://support.fortanix.com/docs/dsm-saml-sso|Configure SAML SSO for Fortanix DSM>"
    ));

    // Every search went through the tool server as Ana, under the `docs` skill, and was audited.
    let events: Vec<_> = audit
        .events()
        .into_iter()
        .filter(|e| e.tool == "docs.search")
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|e| e.user == "ana" && e.skill.as_deref() == Some("docs")));
    assert!(events.iter().any(|e| e.run_id == "slack-docs-e5"));

    stop_slack.cancel();
    stop_ts.cancel();
    ts_cancel.cancel();
}

#[tokio::test]
async fn a_silent_connection_is_replaced_and_a_refused_reply_falls_back_to_text() {
    let (ts_base, _audit, stop_ts, ts_cancel) = start_toolserver().await;
    let slack = Arc::new(FakeSlack::default());
    let app = Router::new()
        .route("/api/apps.connections.open", post(open))
        .route("/socket", get(socket))
        .route("/hooks/picky", post(picky))
        .with_state(slack.clone());
    let (slack_base, stop_slack) = serve(app).await;
    *slack.base.lock().unwrap() = slack_base.clone();
    let hook = format!("{slack_base}/hooks/picky");
    slack.scripts.lock().unwrap().extend([
        // The first connection goes quiet without closing.
        vec![json!({"type": "test_go_silent"})],
        // The bot notices, reconnects, and keeps working.
        vec![
            slash("e1", "U0ANA", "SAML SSO", &hook),
            json!({"type": "disconnect", "reason": "link_disabled"}),
        ],
    ]);
    let dir = tempfile::tempdir().unwrap();
    let team_file = dir.path().join("team.yaml");
    std::fs::write(
        &team_file,
        "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n",
    )
    .unwrap();
    let http = reqwest::Client::new();
    let mut bot = Socket::new(
        slack_base.clone(),
        APP_TOKEN,
        Commands {
            toolserver: ToolServer::new(ts_base, BOT_TOKEN),
            team_file,
            http: http.clone(),
            response_url_prefix: format!("{slack_base}/hooks/"),
        },
        http,
    );
    bot.read_deadline = Duration::from_millis(600);
    bot.ping_every = Duration::from_millis(200);

    let started = Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(10), bot.run(CancellationToken::new()))
        .await
        .expect("the bot gave up on the silent connection");
    assert_eq!(ended, Ended::Disabled);
    assert_eq!(
        *slack.opens.lock().unwrap(),
        2,
        "reconnected after the silence"
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    // Shutdown waits for the reply in flight.
    assert_eq!(bot.drain(Duration::from_secs(5)).await, 0);
    let responses = slack.responses.lock().unwrap().clone();
    assert_eq!(responses.len(), 1, "the plain-text fallback arrived");
    let reply = &responses[0].1;
    assert!(reply.get("blocks").is_none());
    assert!(
        reply["text"].as_str().unwrap().contains("SAML SSO"),
        "{reply}"
    );

    stop_slack.cancel();
    stop_ts.cancel();
    ts_cancel.cancel();
}

#[tokio::test]
async fn shutdown_closes_the_connection() {
    let slack = Arc::new(FakeSlack::default());
    let app = Router::new()
        .route("/api/apps.connections.open", post(open))
        .route("/socket", get(socket))
        .with_state(slack.clone());
    let (slack_base, stop_slack) = serve(app).await;
    *slack.base.lock().unwrap() = slack_base.clone();
    slack
        .scripts
        .lock()
        .unwrap()
        .push_back(vec![json!({"type": "test_go_silent"})]);
    let http = reqwest::Client::new();
    let bot = Socket::new(
        slack_base,
        APP_TOKEN,
        Commands {
            toolserver: ToolServer::new("http://127.0.0.1:9", BOT_TOKEN),
            team_file: "/nonexistent/team.yaml".into(),
            http: http.clone(),
            response_url_prefix: "https://hooks.slack.com/".into(),
        },
        http,
    );
    let shutdown = CancellationToken::new();
    let stopper = shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        stopper.cancel();
    });
    let ended = tokio::time::timeout(Duration::from_secs(5), bot.run(shutdown))
        .await
        .expect("shutdown ends the loop");
    assert_eq!(ended, Ended::Shutdown);
    stop_slack.cancel();
}
