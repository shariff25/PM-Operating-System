//! The Slack reply assistant, Slack's half (`docs/slack-reply-spec.md` §1 and §5), against
//! the fake Slack (`common`), a mock bots runtime (bots, routines, notifications) and a mock
//! tool server approvals API with the audience check's answers.
//!
//! - The reply card: where it goes, the thread link, the draft with its links, the audience,
//!   and Post as you, Edit, Snooze and Reject; never the question's text.
//! - The tool server's refusals: `audience_blocked`, `needs_external_confirm` (and
//!   **Post to external channel** resending with `confirm_external`), `expired`,
//!   `audience_unavailable`.
//! - Snooze: the choices that fit before the expiry, the card back at its time, nothing
//!   back for an item decided or expired meanwhile, and snoozes surviving a restart.
//! - `reply assistant on`, `off` and status, for members with and without a personal bot,
//!   and for someone outside the team.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use common::{serve, until, Call, FakeSlack, APP_TOKEN, SLACK_BOT_TOKEN};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::Commands;
use pmos_slack::notify::Notifier;
use pmos_slack::reply::{Snoozer, PROMPT, SNOOZES_FILE};
use pmos_slack::runtime::Runtime;
use pmos_slack::slackapi::SlackApi;
use pmos_slack::socket::{Ended, Socket};
use pmos_slack::toolserver::ToolServer;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "test-bots-token-slack-0123456789abcdef";
const TOOL_BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const TEAM: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n  - {handle: bob, slack-id: U0BOB, roles: [pm]}\n  - {handle: cal, slack-id: U0CAL, roles: [pm]}\n";
/// Words of the question in the thread. They must never reach a card.
const QUESTION: &str = "QUESTION-TEXT when does SSO ship";

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

fn pmos_user(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-pmos-user")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

fn refuse(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

// ---- Mock runtime ----

/// A routine write: (method, routine id, X-Pmos-User, body).
type RoutineWrite = (String, String, Option<String>, Value);

#[derive(Default)]
struct MockRuntime {
    feed: Mutex<Vec<Value>>,
    routines: Mutex<Vec<Value>>,
    /// Routine writes: (method, routine id, X-Pmos-User, body).
    writes: Mutex<Vec<RoutineWrite>>,
}

type Rt = State<Arc<MockRuntime>>;

async fn rt_bots(headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!([
        {"name": "docs", "owner": null},
        {"name": "pm-ana", "scope": "personal", "owner": "ana"},
        {"name": "pm-cal", "scope": "personal", "owner": "cal"},
    ]))
    .into_response()
}

async fn rt_notifications(State(rt): Rt, Query(q): Query<HashMap<String, String>>) -> Response {
    let from: usize = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let items: Vec<Value> = rt.feed.lock().unwrap().iter().skip(from).cloned().collect();
    if items.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Json(json!({"cursor": (from + items.len()).to_string(), "items": items})).into_response()
}

async fn rt_create_routine(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let user = pmos_user(&headers);
    rt.writes
        .lock()
        .unwrap()
        .push(("POST".into(), String::new(), user, body.clone()));
    let mut routines = rt.routines.lock().unwrap();
    let mut routine = body["draft"].clone();
    routine["id"] = json!(format!("rtn_{}", routines.len() + 1));
    routine["enabled"] = json!(true);
    routine["next_run_at"] = json!("2026-11-16T08:00:00Z");
    routines.push(routine.clone());
    (StatusCode::CREATED, Json(routine)).into_response()
}

async fn rt_routines(
    State(rt): Rt,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let owner = q.get("owner").cloned().unwrap_or_default();
    if pmos_user(&headers).as_deref() != Some(owner.as_str()) {
        return refuse(StatusCode::FORBIDDEN, json!({"error": "own routines only"}));
    }
    let mine: Vec<Value> = rt
        .routines
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["owner"] == owner.as_str())
        .cloned()
        .collect();
    Json(json!(mine)).into_response()
}

async fn rt_delete_routine(State(rt): Rt, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let user = pmos_user(&headers);
    rt.writes
        .lock()
        .unwrap()
        .push(("DELETE".into(), id.clone(), user.clone(), Value::Null));
    let mut routines = rt.routines.lock().unwrap();
    let before = routines.len();
    routines.retain(|r| !(r["id"] == id.as_str() && r["owner"].as_str() == user.as_deref()));
    if routines.len() == before {
        return refuse(StatusCode::NOT_FOUND, json!({"error": "no such routine"}));
    }
    StatusCode::NO_CONTENT.into_response()
}

fn runtime_routes(rt: Arc<MockRuntime>) -> Router {
    Router::new()
        .route("/v1/bots", get(rt_bots))
        .route("/v1/notifications", get(rt_notifications))
        .route("/v1/routines", post(rt_create_routine).get(rt_routines))
        .route(
            "/v1/routines/{id}",
            axum::routing::delete(rt_delete_routine),
        )
        .with_state(rt)
}

// ---- Mock tool server: approvals and the audience check ----

/// What the audience check finds for an item's channel.
#[derive(Clone, Copy, Default)]
struct Audience {
    /// Members not in `team.yaml`.
    outsiders: u64,
    /// Slack Connect.
    external: bool,
    /// Slack's channel calls fail.
    down: bool,
    /// The item passed its expiry, though the sweeper hasn't marked it yet.
    expired: bool,
}

#[derive(Default)]
struct MockTools {
    items: Mutex<HashMap<String, Value>>,
    audience: Mutex<HashMap<String, Audience>>,
    /// `POST /internal/approvals/{id}/decision`: (id, body).
    decisions: Mutex<Vec<(String, Value)>>,
}

type Ts = State<Arc<MockTools>>;

/// `GET /internal/approvals/{id}?user=`: `404` unless the item is `user`'s (S2).
async fn ts_get(
    State(ts): Ts,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if bearer(&headers) != TOOL_BOT_TOKEN {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match ts.items.lock().unwrap().get(&id) {
        Some(item) if q.get("user").is_some_and(|u| item["user"] == u.as_str()) => {
            Json(item.clone()).into_response()
        }
        _ => refuse(
            StatusCode::NOT_FOUND,
            json!({"error": "no such approval item"}),
        ),
    }
}

/// The decision as the spec has the tool server answer it (§3): expired, then the audience
/// rules in order, then the write.
async fn ts_decide(
    State(ts): Ts,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if bearer(&headers) != TOOL_BOT_TOKEN {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ts.decisions
        .lock()
        .unwrap()
        .push((id.clone(), body.clone()));
    let audience = ts
        .audience
        .lock()
        .unwrap()
        .get(&id)
        .copied()
        .unwrap_or_default();
    let mut items = ts.items.lock().unwrap();
    let Some(item) = items.get_mut(&id) else {
        return refuse(StatusCode::NOT_FOUND, json!({"error": "not found"}));
    };
    if item["user"] != body["decided_by"] {
        return refuse(
            StatusCode::FORBIDDEN,
            json!({"error": "only the item's own user can decide it"}),
        );
    }
    if item["status"] == "expired" || audience.expired {
        return refuse(
            StatusCode::CONFLICT,
            json!({"code": "expired", "error": "this draft expired"}),
        );
    }
    if item["status"] != "pending" {
        return refuse(
            StatusCode::CONFLICT,
            json!({"error": "approval item is already decided"}),
        );
    }
    // As the tool server (R4): an approve names what the PM was shown.
    match body["expected_args_hash"].as_str() {
        None if body["decision"] == "approve" => {
            return refuse(
                StatusCode::BAD_REQUEST,
                json!({"code": "expected_args_hash_required", "error": "required"}),
            )
        }
        Some(h) if !item["args_hash"].as_str().unwrap_or("").starts_with(h) => {
            return refuse(
                StatusCode::CONFLICT,
                json!({"code": "stale", "error": "changed"}),
            )
        }
        _ => {}
    }
    let posting = matches!(body["decision"].as_str(), Some("approve" | "edit"));
    if posting {
        if audience.down {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"code": "audience_unavailable", "error": "Slack didn't answer"}),
            );
        }
        if item["sources_sensitivity"] == "restricted"
            && (audience.outsiders > 0 || audience.external)
        {
            return refuse(
                StatusCode::CONFLICT,
                json!({"code": "audience_blocked", "error": "restricted sources", "outsiders": audience.outsiders}),
            );
        }
        if audience.external && body["confirm_external"] != true {
            return refuse(
                StatusCode::CONFLICT,
                json!({"code": "needs_external_confirm", "error": "external channel"}),
            );
        }
    }
    item["decided_by"] = body["decided_by"].clone();
    match body["decision"].as_str() {
        Some("reject") => item["status"] = json!("rejected"),
        Some(d @ ("approve" | "edit")) => {
            if d == "edit" {
                item["args"] = body["args"].clone();
            }
            item["status"] = json!("approved");
            item["result"] =
                json!({"url": "https://slack.com/archives/C0LAUNCH/p1731500000000999"});
        }
        _ => return refuse(StatusCode::BAD_REQUEST, json!({"error": "bad decision"})),
    }
    Json(item.clone()).into_response()
}

fn tools_routes(ts: Arc<MockTools>) -> Router {
    Router::new()
        .route("/internal/approvals/{id}", get(ts_get))
        .route("/internal/approvals/{id}/decision", post(ts_decide))
        .with_state(ts)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

/// Every fixture item's args hash; cards carry its first 16 characters.
const ARGS_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SHOWN: &str = "0123456789abcdef";

/// A reply draft for Ana in #launch (`C0LAUNCH`), made `age` ago.
fn reply(id: &str, sensitivity: &str, age: chrono::Duration) -> Value {
    json!({
        "id": id, "user": "ana", "skill": "bot:pm-ana", "run_id": "tsk_1",
        "tool": "slack.post_reply", "status": "pending", "args_hash": ARGS_HASH,
        "args": {
            "channel": "C0LAUNCH", "thread_ts": "1731500000.000100",
            "text": "SSO ships in 5.1. See <https://docs.example.com/sso|the SSO guide>.",
        },
        "sources_sensitivity": sensitivity,
        "created_at": rfc3339(Utc::now() - age),
    })
}

// ---- The stack ----

struct Stack {
    slack: Arc<FakeSlack>,
    live: mpsc::UnboundedSender<Value>,
    runtime: Arc<MockRuntime>,
    tools: Arc<MockTools>,
    bots: Bots,
    slack_base: String,
    rt_base: String,
    ts_base: String,
    state: tempfile::TempDir,
    team: tempfile::TempDir,
    stop: Vec<CancellationToken>,
    socket: Option<(CancellationToken, JoinHandle<Ended>)>,
}

impl Stack {
    async fn new() -> Self {
        let slack = Arc::new(FakeSlack::default());
        let (slack_base, stop_slack) = serve(common::routes().with_state(slack.clone())).await;
        *slack.base.lock().unwrap() = slack_base.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        *slack.live.lock().unwrap() = Some(rx);
        // Ana is in London; Cal has no time zone in Slack.
        slack
            .tz
            .lock()
            .unwrap()
            .insert("U0ANA".into(), "Europe/London".into());
        let runtime = Arc::new(MockRuntime::default());
        let (rt_base, stop_rt) = serve(runtime_routes(runtime.clone())).await;
        let tools = Arc::new(MockTools::default());
        let (ts_base, stop_ts) = serve(tools_routes(tools.clone())).await;
        let team = tempfile::tempdir().unwrap();
        std::fs::write(team.path().join("team.yaml"), TEAM).unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut stack = Self {
            slack,
            live: tx,
            runtime,
            tools,
            bots: Bots::new(config(
                &slack_base,
                &rt_base,
                &ts_base,
                team.path(),
                state.path(),
            )),
            slack_base,
            rt_base,
            ts_base,
            state,
            team,
            stop: vec![stop_slack, stop_rt, stop_ts],
            socket: None,
        };
        stack.connect();
        stack
    }

    /// New bots on the same state directory, as after a restart.
    fn restarted_bots(&self) -> Bots {
        Bots::new(config(
            &self.slack_base,
            &self.rt_base,
            &self.ts_base,
            self.team.path(),
            self.state.path(),
        ))
    }

    fn connect(&mut self) {
        let http = reqwest::Client::new();
        let socket = Socket::new(
            self.slack_base.clone(),
            APP_TOKEN,
            Commands {
                toolserver: ToolServer::new("http://127.0.0.1:9", TOOL_BOT_TOKEN),
                team_file: "/nonexistent/team.yaml".into(),
                http: http.clone(),
                response_url_prefix: "https://hooks.slack.com/".into(),
                request_access: true,
            },
            http,
        )
        .with_bots(self.bots.clone());
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let handle = tokio::spawn(async move { socket.run(token).await });
        self.socket = Some((shutdown, handle));
    }

    fn add(&self, item: Value, audience: Audience) {
        let id = item["id"].as_str().unwrap().to_string();
        self.tools
            .audience
            .lock()
            .unwrap()
            .insert(id.clone(), audience);
        self.tools.items.lock().unwrap().insert(id, item);
    }

    /// Decided (or expired) elsewhere, as by the console or the sweeper.
    fn set_status(&self, id: &str, status: &str) {
        let mut items = self.tools.items.lock().unwrap();
        let item = items.get_mut(id).unwrap();
        item["status"] = json!(status);
        item["decided_by"] = json!("ana");
    }

    /// Send an envelope; check it was acked within Slack's 3 seconds.
    async fn send(&self, envelope: Value) -> Value {
        let id = envelope["envelope_id"].as_str().unwrap().to_string();
        self.live.send(envelope).unwrap();
        let (acked, ack) = until(&format!("the ack for {id}"), || self.slack.ack(&id)).await;
        let sent = self.slack.sent_at(&id).unwrap();
        assert!(acked.duration_since(sent) < Duration::from_secs(3), "{id}");
        ack
    }

    async fn click(&self, envelope: &str, user: &str, action: &str, value: &str, ts: &str) {
        self.send(click(envelope, user, action, value, ts)).await;
    }

    /// The latest `chat.update` of the message at `ts` that matches `want`.
    async fn card(&self, ts: &str, what: &str, want: impl Fn(&str) -> bool) -> Call {
        until(what, || {
            self.slack
                .calls("chat.update")
                .into_iter()
                .rev()
                .find(|u| u.body["ts"] == ts && want(&text_of(u)))
        })
        .await
    }

    /// The `n`th `chat.postMessage` (from 1).
    async fn post(&self, n: usize) -> Call {
        until(&format!("{n} posts"), || {
            let calls = self.slack.calls("chat.postMessage");
            (calls.len() >= n).then(|| calls[n - 1].clone())
        })
        .await
    }

    fn decisions(&self) -> Vec<(String, Value)> {
        self.tools.decisions.lock().unwrap().clone()
    }

    fn routine_writes(&self) -> Vec<RoutineWrite> {
        self.runtime.writes.lock().unwrap().clone()
    }

    async fn stop(mut self) {
        if let Some((shutdown, handle)) = self.socket.take() {
            shutdown.cancel();
            assert_eq!(handle.await.unwrap(), Ended::Shutdown);
        }
        for s in &self.stop {
            s.cancel();
        }
    }
}

fn config(
    slack: &str,
    rt: &str,
    ts: &str,
    team: &std::path::Path,
    state: &std::path::Path,
) -> BotsConfig {
    let http = reqwest::Client::new();
    BotsConfig {
        slack: SlackApi::new(slack, SLACK_BOT_TOKEN, http.clone()),
        runtime: Runtime::new(rt, BOTS_TOKEN, http),
        toolserver: ToolServer::new(ts, TOOL_BOT_TOKEN),
        team_file: team.join("team.yaml"),
        default_tz: "America/New_York".into(),
        github_org: None,
        response_url_prefix: "https://hooks.slack.com/".into(),
        state_dir: state.to_path_buf(),
        web_url: None,
    }
}

fn dm(envelope_id: &str, user: &str, ts: &str, text: &str) -> Value {
    let channel = format!("D{}", user.trim_start_matches('U'));
    json!({
        "envelope_id": envelope_id, "type": "events_api",
        "payload": {"type": "event_callback", "event_id": format!("Ev-{envelope_id}"), "event": {
            "type": "message", "channel_type": "im", "channel": channel, "user": user,
            "text": text, "ts": ts, "event_ts": ts,
        }},
    })
}

fn click(envelope_id: &str, user: &str, action_id: &str, value: &str, ts: &str) -> Value {
    // As on the cards the bot posts for a fixture item: the buttons carry what they showed.
    let block_id = match action_id {
        "apr_post_external" if !value.ends_with(":edit") => format!("external:{value}:{SHOWN}"),
        "apr_post_external" => format!("external:{}", value.trim_end_matches(":edit")),
        _ => format!("approval:{value}:{SHOWN}"),
    };
    json!({
        "envelope_id": envelope_id, "type": "interactive",
        "payload": {
            "type": "block_actions", "user": {"id": user}, "trigger_id": format!("trigger-{envelope_id}"),
            "container": {"type": "message", "channel_id": "D0ANA", "message_ts": ts},
            "channel": {"id": "D0ANA"},
            "message": {"ts": ts, "thread_ts": "100.1"},
            "actions": [{"type": "button", "action_id": action_id, "block_id": block_id, "value": value}],
        },
    })
}

fn text_of(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

/// Each button's (label, action id, value), in order.
fn buttons(body: &Value) -> Vec<(String, String, String)> {
    body["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| b["elements"].as_array().cloned().unwrap_or_default())
        .filter(|e| e["type"] == "button")
        .map(|e| {
            (
                e["text"]["text"].as_str().unwrap().to_string(),
                e["action_id"].as_str().unwrap().to_string(),
                e["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn labels(body: &Value) -> Vec<String> {
    buttons(body).into_iter().map(|b| b.0).collect()
}

// ---- The card ----

#[tokio::test]
async fn the_reply_card_shows_the_draft_never_the_question_and_posts_as_you() {
    let stack = Stack::new().await;
    // Even an item that somehow carried the question would not show it.
    let mut item = reply("apr_q1", "internal", chrono::Duration::minutes(5));
    item["args"]["question"] = json!(QUESTION);
    item["audience"] = json!({"members": 14});
    stack.add(item, Audience::default());
    stack.runtime.feed.lock().unwrap().push(json!({
        "id": "n1", "task_id": "tsk_1", "bot": "pm-ana", "requested_by": "ana", "slack": null,
        "kind": "needs_approval", "text": "1 draft waiting.", "approvals": ["apr_q1"],
    }));
    let shutdown = CancellationToken::new();
    let mut notifier = Notifier::new(stack.bots.clone(), stack.state.path());
    notifier.wait = Duration::from_millis(200);
    let polling = tokio::spawn(notifier.run(shutdown.clone()));
    let card = stack.post(2).await;
    shutdown.cancel();
    polling.await.unwrap();

    let s = text_of(&card);
    assert!(
        s.contains("*Reply drafted for <#C0LAUNCH>* · <https://slack.com/archives/C0LAUNCH/p1731500000000100|open the thread>"),
        "{s}"
    );
    assert!(s.contains("> SSO ships in 5.1. See <https://docs.example.com/sso|the SSO guide>."));
    assert!(s.contains("Visible to 14 people"));
    assert!(s.contains("built from internal sources"));
    assert!(
        !s.contains("QUESTION-TEXT"),
        "the question is never on the card"
    );
    assert_eq!(
        labels(&card.body),
        ["Post as you", "Edit", "Snooze", "Reject"]
    );
    assert!(buttons(&card.body).iter().all(|b| b.2 == "apr_q1"));
    let ts = "1700000000.000002";

    // Bob can't act on Ana's card.
    stack.click("c1", "U0BOB", "apr_snooze", "apr_q1", ts).await;
    stack
        .click("c2", "U0BOB", "apr_approve", "apr_q1", ts)
        .await;
    let notes = until("two private notes", || {
        let n = stack.slack.calls("chat.postEphemeral");
        (n.len() >= 2).then_some(n)
    })
    .await;
    assert!(notes
        .iter()
        .all(|n| text_of(n).contains("Only the PM this approval is for can decide it")));

    // Post as you.
    stack
        .click("c3", "U0ANA", "apr_approve", "apr_q1", ts)
        .await;
    let posted = stack
        .card(ts, "the posted card", |t| t.contains("Posted by Ana"))
        .await;
    assert!(text_of(&posted)
        .contains("<https://slack.com/archives/C0LAUNCH/p1731500000000999|see the reply>"));
    assert!(labels(&posted.body).is_empty(), "no buttons once posted");
    assert_eq!(
        stack.decisions(),
        vec![(
            "apr_q1".into(),
            json!({"decided_by": "ana", "decision": "approve", "expected_args_hash": SHOWN})
        )]
    );
    stack.stop().await;
}

#[tokio::test]
async fn each_refusal_gets_its_own_card() {
    let stack = Stack::new().await;
    let age = chrono::Duration::hours(1);
    stack.add(
        reply("apr_b1", "restricted", age),
        Audience {
            outsiders: 3,
            ..Default::default()
        },
    );
    stack.add(
        reply("apr_x1", "internal", age),
        Audience {
            external: true,
            ..Default::default()
        },
    );
    stack.add(
        reply("apr_x2", "internal", age),
        Audience {
            external: true,
            ..Default::default()
        },
    );
    stack.add(
        reply("apr_d1", "internal", age),
        Audience {
            down: true,
            ..Default::default()
        },
    );
    stack.add(
        reply("apr_e1", "internal", chrono::Duration::hours(25)),
        Audience {
            expired: true,
            ..Default::default()
        },
    );

    // Restricted sources and outsiders: why, in plain words, with only Reject (an edit can't
    // unblock a draft built from restricted sources).
    stack
        .click("b1", "U0ANA", "apr_approve", "apr_b1", "1.1")
        .await;
    let blocked = stack
        .card("1.1", "the blocked card", |t| t.contains("restricted"))
        .await;
    assert!(text_of(&blocked).contains(
        "This draft was built from restricted sources and 3 people in <#C0LAUNCH> aren't on the product team"
    ));
    assert_eq!(labels(&blocked.body), ["Reject"]);

    // An external channel: a second confirmation, sent with confirm_external.
    stack
        .click("x1", "U0ANA", "apr_approve", "apr_x1", "1.2")
        .await;
    let confirm = stack
        .card("1.2", "the confirmation", |t| {
            t.contains("apr_post_external")
        })
        .await;
    assert!(text_of(&confirm).contains("is an external channel (Slack Connect)"));
    assert!(text_of(&confirm).contains("External channel"));
    assert_eq!(
        labels(&confirm.body),
        ["Post to external channel", "Edit", "Reject"]
    );
    stack
        .click("x1b", "U0ANA", "apr_post_external", "apr_x1", "1.2")
        .await;
    stack
        .card("1.2", "posted externally", |t| t.contains("Posted by Ana"))
        .await;

    // After an edit, the confirmation resends the edit.
    stack
        .click("x2", "U0ANA", "apr_edit", "apr_x2", "1.3")
        .await;
    let opened = until("the editor", || stack.slack.calls("views.open").pop()).await;
    let view = &opened.body["view"];
    assert_eq!(view["submit"]["text"], "Post as you");
    stack
        .send(json!({
            "envelope_id": "x2s", "type": "interactive",
            "payload": {"type": "view_submission", "user": {"id": "U0ANA"}, "view": {
                "callback_id": view["callback_id"], "private_metadata": view["private_metadata"],
                "state": {"values": {"field:text": {"value": {"type": "plain_text_input", "value": "SSO ships in 5.1, in March."}}}},
            }},
        }))
        .await;
    let confirm = stack
        .card("1.3", "the confirmation after the edit", |t| {
            t.contains("apr_post_external")
        })
        .await;
    assert!(text_of(&confirm).contains("in March"));
    let value = buttons(&confirm.body)
        .into_iter()
        .find(|b| b.1 == "apr_post_external")
        .unwrap()
        .2;
    assert_eq!(value, "apr_x2:edit");
    stack
        .click("x2b", "U0ANA", "apr_post_external", &value, "1.3")
        .await;
    stack
        .card("1.3", "the edit posted", |t| {
            t.contains("Edited and posted by Ana")
        })
        .await;

    // Slack's channel calls failed: nothing posted; try again.
    stack
        .click("d1", "U0ANA", "apr_approve", "apr_d1", "1.4")
        .await;
    let down = stack
        .card("1.4", "the retry", |t| {
            t.contains("couldn't check who is in")
        })
        .await;
    assert_eq!(
        labels(&down.body),
        ["Post as you", "Edit", "Snooze", "Reject"]
    );

    // Expired.
    stack
        .click("e1", "U0ANA", "apr_approve", "apr_e1", "1.5")
        .await;
    let expired = stack
        .card("1.5", "the expired card", |t| t.contains("Expired"))
        .await;
    assert!(text_of(&expired).contains("Expired after 24 hours; nothing was posted"));
    assert!(labels(&expired.body).is_empty());

    let bodies: Vec<(String, Value)> = stack.decisions();
    let of = |id: &str| {
        bodies
            .iter()
            .filter(|(i, _)| i == id)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of("apr_x1"),
        vec![
            (
                "apr_x1".into(),
                json!({"decided_by": "ana", "decision": "approve", "expected_args_hash": SHOWN})
            ),
            (
                "apr_x1".into(),
                json!({"decided_by": "ana", "decision": "approve", "confirm_external": true,
                       "expected_args_hash": SHOWN})
            ),
        ]
    );
    let edit_args = json!({"channel": "C0LAUNCH", "thread_ts": "1731500000.000100", "text": "SSO ships in 5.1, in March."});
    assert_eq!(
        of("apr_x2"),
        vec![
            (
                "apr_x2".into(),
                json!({"decided_by": "ana", "decision": "edit", "args": edit_args})
            ),
            (
                "apr_x2".into(),
                json!({"decided_by": "ana", "decision": "edit", "args": edit_args, "confirm_external": true})
            ),
        ]
    );
    assert_eq!(
        stack.tools.items.lock().unwrap()["apr_b1"]["status"],
        "pending",
        "a blocked draft stays pending"
    );
    stack.stop().await;
}

// ---- Snooze ----

#[tokio::test]
async fn snooze_offers_what_fits_and_brings_the_card_back() {
    let stack = Stack::new().await;
    // Expires in 40 hours: every choice fits.
    let mut fresh = reply("apr_s1", "internal", chrono::Duration::zero());
    fresh["expires_at"] = json!(rfc3339(Utc::now() + chrono::Duration::hours(40)));
    stack.add(fresh, Audience::default());
    // Expires in 2.5 hours: only an hour fits.
    let mut soon = reply("apr_s2", "internal", chrono::Duration::hours(22));
    soon["expires_at"] = json!(rfc3339(Utc::now() + chrono::Duration::minutes(150)));
    stack.add(soon, Audience::default());
    // Expires in 15 minutes: nothing fits.
    stack.add(
        reply(
            "apr_s3",
            "internal",
            chrono::Duration::hours(24) - chrono::Duration::minutes(15),
        ),
        Audience::default(),
    );

    stack
        .click("s1", "U0ANA", "apr_snooze", "apr_s1", "2.1")
        .await;
    let menu = stack
        .card("2.1", "the choices", |t| t.contains("Snooze until when"))
        .await;
    assert_eq!(
        labels(&menu.body),
        ["1 hour", "4 hours", "Tomorrow 09:00", "Back"]
    );
    stack
        .click("s2", "U0ANA", "apr_snooze", "apr_s2", "2.2")
        .await;
    let menu = stack
        .card("2.2", "fewer choices", |t| t.contains("Snooze until when"))
        .await;
    assert_eq!(labels(&menu.body), ["1 hour", "Back"]);
    stack
        .click("s3", "U0ANA", "apr_snooze", "apr_s3", "2.3")
        .await;
    let menu = stack
        .card("2.3", "no choices", |t| t.contains("Too late to snooze"))
        .await;
    assert_eq!(labels(&menu.body), ["Back"]);
    // A choice that doesn't fit, sent anyway, shows the choices again.
    stack
        .click("s3b", "U0ANA", "apr_snooze_4h", "apr_s2", "2.2")
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(stack.bots.snoozes().all().is_empty());
    // Back restores the card.
    stack
        .click("s3c", "U0ANA", "apr_back", "apr_s3", "2.3")
        .await;
    stack
        .card("2.3", "the card back", |t| t.contains("apr_snooze\""))
        .await;

    // 4 hours: kept in the state directory, and the card says until when.
    let before = Utc::now();
    stack
        .click("s4", "U0ANA", "apr_snooze_4h", "apr_s1", "2.1")
        .await;
    let snoozed = stack
        .card("2.1", "snoozed", |t| t.contains("Snoozed until"))
        .await;
    assert!(labels(&snoozed.body).is_empty());
    assert!(text_of(&snoozed).contains("*Reply drafted for <#C0LAUNCH>*"));
    assert!(
        !text_of(&snoozed).contains("SSO ships"),
        "the card is replaced"
    );
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(stack.state.path().join(SNOOZES_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(saved[0]["approval_id"], "apr_s1");
    assert_eq!(saved[0]["channel"], "D0ANA");
    assert_eq!(saved[0]["ts"], "2.1");
    let until_at: DateTime<Utc> = saved[0]["until"].as_str().unwrap().parse().unwrap();
    let four = chrono::Duration::hours(4);
    assert!(until_at >= before + four && until_at <= Utc::now() + four);
    // 1 hour on the second, and a third snoozed then decided elsewhere, a fourth expired.
    stack
        .click("s5", "U0ANA", "apr_snooze_1h", "apr_s2", "2.2")
        .await;
    stack
        .card("2.2", "snoozed", |t| t.contains("Snoozed until"))
        .await;
    for (id, ts, status) in [("apr_s5", "2.5", "rejected"), ("apr_s6", "2.6", "expired")] {
        let mut item = reply(id, "internal", chrono::Duration::hours(1));
        item["expires_at"] = json!(rfc3339(Utc::now() + chrono::Duration::hours(40)));
        stack.add(item, Audience::default());
        stack
            .click(&format!("z-{id}"), "U0ANA", "apr_snooze_1h", id, ts)
            .await;
        stack
            .card(ts, "snoozed", |t| t.contains("Snoozed until"))
            .await;
        stack.set_status(id, status);
    }
    assert_eq!(stack.bots.snoozes().all().len(), 4);

    // Before their time, nothing comes back.
    let posts_before = stack.slack.calls("chat.postMessage").len();
    assert_eq!(Snoozer::new(stack.bots.clone()).wake_due().await, 0);
    assert_eq!(stack.slack.calls("chat.postMessage").len(), posts_before);

    // Two hours on: the hour-long ones are due. The pending one comes back as a new card in
    // the DM; the rejected and expired ones don't, and their messages show why.
    let mut snoozer = Snoozer::new(stack.bots.clone());
    snoozer.clock = Arc::new(|| Utc::now() + chrono::Duration::hours(2));
    assert_eq!(snoozer.wake_due().await, 1);
    let posts = stack.slack.calls("chat.postMessage");
    assert_eq!(posts.len(), posts_before + 1);
    let back = posts.last().unwrap();
    assert_eq!(back.body["channel"], "D0ANA");
    assert_eq!(
        labels(&back.body),
        ["Post as you", "Edit", "Snooze", "Reject"]
    );
    assert!(buttons(&back.body).iter().all(|b| b.2 == "apr_s2"));
    stack
        .card("2.2", "the old card says it's back", |t| {
            t.contains("back, below")
        })
        .await;
    stack
        .card("2.5", "rejected meanwhile", |t| {
            t.contains("Rejected by Ana")
        })
        .await;
    stack
        .card("2.6", "expired meanwhile", |t| {
            t.contains("Expired after 24 hours")
        })
        .await;
    let left: Vec<String> = stack
        .bots
        .snoozes()
        .all()
        .into_iter()
        .map(|s| s.approval_id)
        .collect();
    assert_eq!(left, ["apr_s1"], "the 4-hour one is still waiting");
    stack.stop().await;
}

#[tokio::test]
async fn snoozes_survive_a_restart() {
    let stack = Stack::new().await;
    let mut item = reply("apr_r1", "internal", chrono::Duration::minutes(5));
    item["expires_at"] = json!(rfc3339(Utc::now() + chrono::Duration::hours(40)));
    stack.add(item, Audience::default());
    stack
        .click("r1", "U0ANA", "apr_snooze_1h", "apr_r1", "3.1")
        .await;
    stack
        .card("3.1", "snoozed", |t| t.contains("Snoozed until"))
        .await;

    // A new process: the snooze is read back from the state directory, and the loop posts
    // the card at its time.
    let bots = stack.restarted_bots();
    assert_eq!(bots.snoozes().all().len(), 1);
    let mut snoozer = Snoozer::new(bots.clone());
    snoozer.tick = Duration::from_millis(100);
    snoozer.clock = Arc::new(|| Utc::now() + chrono::Duration::minutes(61));
    let shutdown = CancellationToken::new();
    let running = tokio::spawn(snoozer.run(shutdown.clone()));
    let back = until("the card back", || {
        stack
            .slack
            .calls("chat.postMessage")
            .into_iter()
            .find(|p| text_of(p).contains("apr_approve"))
    })
    .await;
    assert!(buttons(&back.body).iter().all(|b| b.2 == "apr_r1"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    shutdown.cancel();
    running.await.unwrap();
    assert_eq!(
        stack
            .slack
            .calls("chat.postMessage")
            .iter()
            .filter(|p| text_of(p).contains("apr_approve"))
            .count(),
        1,
        "posted once"
    );
    assert!(bots.snoozes().all().is_empty());
    assert_eq!(
        std::fs::read_to_string(stack.state.path().join(SNOOZES_FILE)).unwrap(),
        "[]"
    );
    stack.stop().await;
}

// ---- The commands ----

#[tokio::test]
async fn reply_assistant_on_off_and_status() {
    let stack = Stack::new().await;
    // Ana already has another routine; it is never touched.
    stack.runtime.routines.lock().unwrap().push(json!({
        "id": "rtn_0", "bot": "pm-ana", "owner": "ana", "enabled": true,
        "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"}, "prompt": "Summarize #launch",
    }));

    stack
        .send(dm("e1", "U0ANA", "400.1", "reply assistant"))
        .await;
    let off = stack.post(1).await;
    assert_eq!(off.body["thread_ts"], "400.1");
    assert!(text_of(&off).contains("The reply assistant is off"));

    stack
        .send(dm("e2", "U0ANA", "400.2", "Reply assistant on"))
        .await;
    let on = stack.post(2).await;
    assert!(
        text_of(&on).contains("The reply assistant is on"),
        "{}",
        text_of(&on)
    );
    assert!(text_of(&on).contains("Europe/London"));
    assert!(text_of(&on).contains("Next run: Mon 16 Nov 08:00 GMT"));
    let writes = stack.routine_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "POST");
    assert_eq!(writes[0].2.as_deref(), Some("ana"));
    assert_eq!(
        writes[0].3,
        json!({"draft": {
            "bot": "pm-ana", "owner": "ana",
            "schedule": {"cron": "*/30 8-17 * * 1-5", "tz": "Europe/London"},
            "prompt": PROMPT,
            "output": {"kind": "dm"},
            "approval": "approve",
        }}),
        "exactly the spec's draft"
    );

    stack
        .send(dm("e3", "U0ANA", "400.3", "reply assistant on"))
        .await;
    let again = stack.post(3).await;
    assert!(text_of(&again).contains("already on"));
    assert_eq!(stack.routine_writes().len(), 1, "not created twice");

    stack
        .send(dm("e4", "U0ANA", "400.4", "reply assistant?"))
        .await;
    let status = stack.post(4).await;
    assert!(text_of(&status).contains("The reply assistant is on"));
    assert!(text_of(&status).contains("Next run: Mon 16 Nov 08:00 GMT"));

    stack
        .send(dm("e5", "U0ANA", "400.5", "reply assistant off"))
        .await;
    let turned_off = stack.post(5).await;
    assert!(text_of(&turned_off).contains("The reply assistant is off"));
    let writes = stack.routine_writes();
    assert_eq!(
        (
            writes[1].0.as_str(),
            writes[1].1.as_str(),
            writes[1].2.as_deref()
        ),
        ("DELETE", "rtn_2", Some("ana"))
    );
    let left: Vec<Value> = stack.runtime.routines.lock().unwrap().clone();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["id"], "rtn_0", "the other routine stays");
    stack
        .send(dm("e6", "U0ANA", "400.6", "reply assistant off"))
        .await;
    assert!(text_of(&stack.post(6).await).contains("already off"));

    // Cal has no time zone in Slack: PMOS_DEFAULT_TZ.
    stack
        .send(dm("e7", "U0CAL", "400.7", "reply assistant on"))
        .await;
    stack.post(7).await;
    let writes = stack.routine_writes();
    assert_eq!(
        writes.last().unwrap().3["draft"]["schedule"]["tz"],
        "America/New_York"
    );
    assert_eq!(writes.last().unwrap().3["draft"]["bot"], "pm-cal");

    // Bob has no personal bot: told so, nothing changes.
    stack
        .send(dm("e8", "U0BOB", "400.8", "reply assistant on"))
        .await;
    let bob = stack.post(8).await;
    assert!(text_of(&bob).contains("runs on your personal bot, `pm-bob`"));
    assert!(text_of(&bob).contains("nothing was changed"));

    // Someone outside the team gets the request-access note.
    stack
        .send(dm("e9", "U0STRANGER", "400.9", "reply assistant on"))
        .await;
    let note = until("the private note", || {
        stack.slack.calls("chat.postEphemeral").pop()
    })
    .await;
    assert_eq!(note.body["user"], "U0STRANGER");
    assert!(text_of(&note).contains("PM OS is for the product team"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        stack.routine_writes().len(),
        3,
        "nothing for Bob or the stranger"
    );
    assert_eq!(stack.slack.calls("chat.postMessage").len(), 8);
    stack.stop().await;
}
