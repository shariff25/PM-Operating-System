//! The bots in Slack end to end (spec §5): the fake Slack (`common`), a mock bots runtime
//! (`pmos-bots`, spec §3) and a mock tool server approvals API (spec §2).
//!
//! - DMs go to the right bot, in the spec's order, as the right PM; strangers get a note.
//! - Notifications: one progress message per task edited in place, then a new message for
//!   the result; failures with their reason; the cursor survives a restart.
//! - Approval cards: approve, edit (modal) and reject, decided by their own PM only.
//! - Reject asks why: six reasons and Back, in place; a reason goes with the decision.
//! - Routines: create, question, cancel, list, pause, resume, delete.
//! - Every envelope is acknowledged before its work, even with slow backends.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{serve, until, Call, FakeSlack, APP_TOKEN, SLACK_BOT_TOKEN};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::Commands;
use pmos_slack::notify::{Notifier, CURSOR_FILE};
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
const TEAM: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n  - {handle: bob, slack-id: U0BOB, roles: [pm]}\n";

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

fn refuse(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

// ---- Mock runtime ----

/// A routine write: (method, routine id, X-Pmos-User, body).
type RoutineWrite = (String, String, Option<String>, Value);

#[derive(Default)]
struct MockRuntime {
    delay: Mutex<Duration>,
    /// `POST /v1/tasks`: (when, X-Pmos-User, body).
    tasks: Mutex<Vec<(Instant, Option<String>, Value)>>,
    /// The feed; the cursor is an index into it.
    feed: Mutex<Vec<Value>>,
    /// The `after` of each notifications poll.
    polls: Mutex<Vec<Option<String>>>,
    /// `POST /v1/routines/parse`: (X-Pmos-User, body).
    parses: Mutex<Vec<(Option<String>, Value)>>,
    routines: Mutex<Vec<Value>>,
    routine_writes: Mutex<Vec<RoutineWrite>>,
}

impl MockRuntime {
    async fn pause(&self) {
        let delay = *self.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
    }
}

type Rt = State<Arc<MockRuntime>>;

async fn rt_bots(State(_): Rt, headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return refuse(StatusCode::UNAUTHORIZED, "bad token");
    }
    Json(json!([
        {"name": "docs", "role": "docs-helper", "scope": "team", "owner": null, "description": "Answers from the support docs"},
        {"name": "pm-ana", "role": "pm-assistant", "scope": "personal", "owner": "ana", "description": "Ana's assistant"},
    ]))
    .into_response()
}

async fn rt_create_task(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    rt.pause().await;
    if bearer(&headers) != BOTS_TOKEN {
        return refuse(StatusCode::UNAUTHORIZED, "bad token");
    }
    let user = pmos_user(&headers);
    rt.tasks
        .lock()
        .unwrap()
        .push((Instant::now(), user.clone(), body.clone()));
    if body["bot"] == "pm-ana" && body["requested_by"] != "ana" {
        return refuse(StatusCode::FORBIDDEN, "pm-ana serves ana only");
    }
    let n = rt.tasks.lock().unwrap().len();
    let mut task = body.clone();
    task["id"] = json!(format!("tsk_{n:032x}"));
    task["status"] = json!("queued");
    (StatusCode::CREATED, Json(task)).into_response()
}

async fn rt_notifications(
    State(rt): Rt,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return refuse(StatusCode::UNAUTHORIZED, "bad token");
    }
    let after = q.get("after").cloned();
    rt.polls.lock().unwrap().push(after.clone());
    let from: usize = after.as_deref().and_then(|a| a.parse().ok()).unwrap_or(0);
    let items: Vec<Value> = rt.feed.lock().unwrap().iter().skip(from).cloned().collect();
    if items.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Json(json!({"cursor": (from + items.len()).to_string(), "items": items})).into_response()
}

async fn rt_parse(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    rt.pause().await;
    rt.parses
        .lock()
        .unwrap()
        .push((pmos_user(&headers), body.clone()));
    let text = body["text"].as_str().unwrap_or_default();
    if text.contains("sometime") && !text.contains("9am") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"question": "What time should I run it?"})),
        )
            .into_response();
    }
    let draft = json!({
        "bot": body["bot"], "owner": body["owner"], "enabled": true,
        "schedule": {"cron": "0 8 * * 1-5", "tz": body["tz"]},
        "prompt": text, "output": {"kind": "dm"}, "approval": "approve",
    });
    Json(json!({
        "draft": draft,
        "summary": format!("Every weekday at 8:00 {} time, {} will: {text}", body["tz"].as_str().unwrap_or_default(), body["bot"].as_str().unwrap_or_default()),
    }))
    .into_response()
}

async fn rt_create_routine(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let user = pmos_user(&headers);
    let mut routines = rt.routines.lock().unwrap();
    let mut routine = body["draft"].clone();
    routine["id"] = json!(format!("rtn_{}", routines.len() + 1));
    routine["next_run_at"] = json!("2026-11-16T08:00:00Z");
    routines.push(routine.clone());
    rt.routine_writes
        .lock()
        .unwrap()
        .push(("POST".into(), String::new(), user, body));
    (StatusCode::CREATED, Json(routine)).into_response()
}

async fn rt_routines(
    State(rt): Rt,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let owner = q.get("owner").cloned().unwrap_or_default();
    if pmos_user(&headers).as_deref() != Some(owner.as_str()) {
        return refuse(StatusCode::FORBIDDEN, "own routines only");
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

async fn rt_patch_routine(
    State(rt): Rt,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = pmos_user(&headers);
    rt.routine_writes.lock().unwrap().push((
        "PATCH".into(),
        id.clone(),
        user.clone(),
        body.clone(),
    ));
    let mut routines = rt.routines.lock().unwrap();
    let Some(r) = routines.iter_mut().find(|r| r["id"] == id.as_str()) else {
        return refuse(StatusCode::NOT_FOUND, "no such routine");
    };
    if r["owner"].as_str() != user.as_deref() {
        return refuse(StatusCode::FORBIDDEN, "own routines only");
    }
    if let Some(enabled) = body["enabled"].as_bool() {
        r["enabled"] = json!(enabled);
    }
    Json(r.clone()).into_response()
}

async fn rt_delete_routine(State(rt): Rt, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let user = pmos_user(&headers);
    rt.routine_writes.lock().unwrap().push((
        "DELETE".into(),
        id.clone(),
        user.clone(),
        Value::Null,
    ));
    let mut routines = rt.routines.lock().unwrap();
    let before = routines.len();
    routines.retain(|r| !(r["id"] == id.as_str() && r["owner"].as_str() == user.as_deref()));
    if routines.len() == before {
        return refuse(StatusCode::NOT_FOUND, "no such routine");
    }
    StatusCode::NO_CONTENT.into_response()
}

fn runtime_routes(rt: Arc<MockRuntime>) -> Router {
    Router::new()
        .route("/v1/bots", get(rt_bots))
        .route("/v1/tasks", post(rt_create_task))
        .route("/v1/notifications", get(rt_notifications))
        .route("/v1/routines/parse", post(rt_parse))
        .route("/v1/routines", post(rt_create_routine).get(rt_routines))
        .route(
            "/v1/routines/{id}",
            axum::routing::patch(rt_patch_routine).delete(rt_delete_routine),
        )
        .with_state(rt)
}

// ---- Mock tool server (approvals) ----

#[derive(Default)]
struct MockTools {
    delay: Mutex<Duration>,
    items: Mutex<HashMap<String, Value>>,
    /// `POST /internal/approvals/{id}/decision`: (when, id, body).
    decisions: Mutex<Vec<(Instant, String, Value)>>,
}

type Ts = State<Arc<MockTools>>;

async fn ts_get(State(ts): Ts, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if bearer(&headers) != TOOL_BOT_TOKEN {
        return refuse(StatusCode::UNAUTHORIZED, "bad token");
    }
    match ts.items.lock().unwrap().get(&id) {
        Some(item) => Json(item.clone()).into_response(),
        None => refuse(StatusCode::NOT_FOUND, "no such approval item"),
    }
}

async fn ts_decide(
    State(ts): Ts,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let delay = *ts.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    if bearer(&headers) != TOOL_BOT_TOKEN {
        return refuse(StatusCode::UNAUTHORIZED, "bad token");
    }
    ts.decisions
        .lock()
        .unwrap()
        .push((Instant::now(), id.clone(), body.clone()));
    let mut items = ts.items.lock().unwrap();
    let Some(item) = items.get_mut(&id) else {
        return refuse(StatusCode::NOT_FOUND, "approval item not found");
    };
    if item["user"] != body["decided_by"] {
        return refuse(
            StatusCode::FORBIDDEN,
            "only the item's own user can decide it",
        );
    }
    if item["status"] != "pending" {
        return refuse(StatusCode::CONFLICT, "approval item is already decided");
    }
    // A reason goes only with `reject` (spec §1 of `docs/improvement-loop-spec.md`).
    if body.get("reason").is_some() && body["decision"] != "reject" {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a reason goes only with reject",
        );
    }
    item["decided_by"] = body["decided_by"].clone();
    match body["decision"].as_str() {
        Some("reject") => item["status"] = json!("rejected"),
        Some(d @ ("approve" | "edit")) => {
            if d == "edit" {
                item["args"] = body["args"].clone();
            }
            item["status"] = json!("approved");
            let n = id.trim_start_matches("apr_").to_uppercase();
            item["result"] = json!({"system": "confluence", "id": format!("CONF-{n}"), "url": format!("https://confluence.fake.pmos.internal/CONF-{n}")});
        }
        _ => return refuse(StatusCode::BAD_REQUEST, "bad decision"),
    }
    Json(item.clone()).into_response()
}

fn tools_routes(ts: Arc<MockTools>) -> Router {
    Router::new()
        .route("/internal/approvals/{id}", get(ts_get))
        .route("/internal/approvals/{id}/decision", post(ts_decide))
        .with_state(ts)
}

fn approval(id: &str, user: &str, tool: &str, args: Value) -> Value {
    json!({
        "id": id, "user": user, "skill": "bot:docs", "run_id": "tsk_1", "tool": tool,
        "args": args, "args_hash": "x", "status": "pending", "created_at": "2026-11-14T10:00:00Z",
    })
}

// ---- The stack ----

struct Stack {
    slack: Arc<FakeSlack>,
    live: mpsc::UnboundedSender<Value>,
    runtime: Arc<MockRuntime>,
    tools: Arc<MockTools>,
    bots: Bots,
    state: tempfile::TempDir,
    _team: tempfile::TempDir,
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
        let team_file = team.path().join("team.yaml");
        std::fs::write(&team_file, TEAM).unwrap();
        let http = reqwest::Client::new();
        let bots = Bots::new(BotsConfig {
            slack: SlackApi::new(slack_base, SLACK_BOT_TOKEN, http.clone()),
            runtime: Runtime::new(rt_base, BOTS_TOKEN, http.clone()),
            toolserver: ToolServer::new(ts_base, TOOL_BOT_TOKEN),
            team_file,
            default_tz: "UTC".into(),
            github_org: None,
            response_url_prefix: "https://hooks.slack.com/".into(),
        });
        Self {
            slack,
            live: tx,
            runtime,
            tools,
            bots,
            state: tempfile::tempdir().unwrap(),
            _team: team,
            stop: vec![stop_slack, stop_rt, stop_ts],
            socket: None,
        }
    }

    /// Connect the bot to the fake Slack.
    fn connect(&mut self) {
        let http = reqwest::Client::new();
        let socket = Socket::new(
            self.slack.base.lock().unwrap().clone(),
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

    /// Start the notifications loop; cancel the token to stop it.
    fn notifier(&self) -> (CancellationToken, JoinHandle<()>) {
        let shutdown = CancellationToken::new();
        let mut notifier = Notifier::new(self.bots.clone(), self.state.path());
        notifier.wait = Duration::from_millis(200);
        let handle = tokio::spawn(notifier.run(shutdown.clone()));
        (shutdown, handle)
    }

    /// Send an envelope and wait for its acknowledgement.
    async fn send(&self, envelope: Value) -> Value {
        let id = envelope["envelope_id"].as_str().unwrap().to_string();
        self.live.send(envelope).unwrap();
        until(&format!("the ack for {id}"), || self.slack.ack(&id))
            .await
            .1
    }

    fn feed(&self, items: Vec<Value>) {
        self.runtime.feed.lock().unwrap().extend(items);
    }

    async fn calls(&self, method: &str, n: usize) -> Vec<Call> {
        until(&format!("{n} {method} calls"), || {
            let calls = self.slack.calls(method);
            (calls.len() >= n).then_some(calls)
        })
        .await
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

fn dm(
    envelope_id: &str,
    event_id: &str,
    user: &str,
    ts: &str,
    thread: Option<&str>,
    text: &str,
) -> Value {
    let channel = format!("D{}", user.trim_start_matches('U'));
    let mut event = json!({
        "type": "message", "channel_type": "im", "channel": channel, "user": user,
        "text": text, "ts": ts, "event_ts": ts,
    });
    if let Some(t) = thread {
        event["thread_ts"] = json!(t);
    }
    json!({
        "envelope_id": envelope_id, "type": "events_api", "accepts_response_payload": false,
        "payload": {"type": "event_callback", "event_id": event_id, "event": event},
    })
}

fn click(
    envelope_id: &str,
    user: &str,
    action_id: &str,
    value: &str,
    channel: &str,
    ts: &str,
) -> Value {
    json!({
        "envelope_id": envelope_id, "type": "interactive", "accepts_response_payload": false,
        "payload": {
            "type": "block_actions", "user": {"id": user}, "trigger_id": format!("trigger-{envelope_id}"),
            "container": {"type": "message", "channel_id": channel, "message_ts": ts},
            "channel": {"id": channel},
            "message": {"ts": ts, "thread_ts": "100.2"},
            "actions": [{"type": "button", "action_id": action_id, "block_id": "b", "value": value}],
        },
    })
}

fn text_of(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

/// The `value` of the button with `action_id` in a message's blocks.
fn button_value(body: &Value, action_id: &str) -> String {
    body["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| b["elements"].as_array().cloned().unwrap_or_default())
        .find(|e| e["action_id"] == action_id)
        .and_then(|e| e["value"].as_str().map(String::from))
        .unwrap_or_else(|| panic!("no {action_id} button in {body}"))
}

#[tokio::test]
async fn dms_go_to_the_right_bot_as_the_right_pm() {
    let mut stack = Stack::new().await;
    stack.connect();
    let tasks = |n: usize| {
        let rt = stack.runtime.clone();
        async move {
            until(&format!("{n} tasks"), || {
                let t = rt.tasks.lock().unwrap().clone();
                (t.len() >= n).then_some(t)
            })
            .await
        }
    };

    // 3. No prefix, no thread: Ana's personal bot.
    stack
        .send(dm(
            "e1",
            "Ev1",
            "U0ANA",
            "100.1",
            None,
            "what ports does DSM need?",
        ))
        .await;
    tasks(1).await;
    // 1. A prefix picks the bot.
    stack
        .send(dm(
            "e2",
            "Ev2",
            "U0ANA",
            "100.2",
            None,
            "docs: what is SAML &amp; SSO?",
        ))
        .await;
    tasks(2).await;
    // 2. A reply in that thread stays with its bot.
    stack
        .send(dm(
            "e3",
            "Ev3",
            "U0ANA",
            "100.3",
            Some("100.2"),
            "and for CCM?",
        ))
        .await;
    tasks(3).await;
    // 1 beats 2: a prefix in the thread.
    stack
        .send(dm(
            "e4",
            "Ev4",
            "U0ANA",
            "100.4",
            Some("100.2"),
            "pm-ana: draft a PRD",
        ))
        .await;
    tasks(4).await;
    // 4. Bob has no personal bot: docs.
    stack
        .send(dm("e5", "Ev5", "U0BOB", "100.5", None, "hello"))
        .await;
    tasks(5).await;
    // Not in team.yaml: a private note, no task.
    stack
        .send(dm("e6", "Ev6", "U0STRANGER", "100.6", None, "hi"))
        .await;
    // The bot's own messages, edits and other subtypes, and retries, are ignored.
    let mut own = dm("e7", "Ev7", "U0ANA", "100.7", None, "Working on it");
    own["payload"]["event"]["bot_id"] = json!("B1");
    stack.send(own).await;
    let mut edit = dm("e8", "Ev8", "U0ANA", "100.8", None, "edited");
    edit["payload"]["event"]["subtype"] = json!("message_changed");
    stack.send(edit).await;
    stack
        .send(dm(
            "e9",
            "Ev1",
            "U0ANA",
            "100.1",
            None,
            "what ports does DSM need?",
        ))
        .await;
    // Someone else's personal bot: the runtime refuses, the PM is told why.
    stack
        .send(dm(
            "e10",
            "Ev10",
            "U0BOB",
            "100.10",
            None,
            "pm-ana: show me Ana's notes",
        ))
        .await;
    tasks(6).await;
    let notes = stack.calls("chat.postMessage", 1).await;
    let ephemeral = stack.calls("chat.postEphemeral", 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all_tasks = stack.runtime.tasks.lock().unwrap().clone();
    assert_eq!(
        all_tasks.len(),
        6,
        "no task for the stranger, the bot, the edit or the retry"
    );

    let expect = [
        ("pm-ana", "ana", "100.1", "what ports does DSM need?"),
        ("docs", "ana", "100.2", "what is SAML & SSO?"),
        ("docs", "ana", "100.2", "and for CCM?"),
        ("pm-ana", "ana", "100.2", "draft a PRD"),
        ("docs", "bob", "100.5", "hello"),
        ("pm-ana", "bob", "100.10", "show me Ana's notes"),
    ];
    for (i, ((_, user, body), (bot, handle, thread, text))) in
        all_tasks.iter().zip(expect).enumerate()
    {
        assert_eq!(body["bot"], bot, "task {i}");
        assert_eq!(body["requested_by"], handle, "task {i}");
        assert_eq!(user.as_deref(), Some(handle), "X-Pmos-User, task {i}");
        assert_eq!(body["source"], "slack");
        assert_eq!(body["slack"]["thread_ts"], thread, "task {i}");
        assert_eq!(body["text"], text, "task {i}");
    }
    assert_eq!(all_tasks[0].2["slack"]["channel"], "D0ANA");

    assert_eq!(ephemeral.len(), 1);
    assert_eq!(ephemeral[0].body["user"], "U0STRANGER");
    assert!(text_of(&ephemeral[0]).contains("PM OS is for the Acme product team"));
    assert_eq!(
        notes.len(),
        1,
        "only the refusal is posted; replies come from notifications"
    );
    assert_eq!(notes[0].body["thread_ts"], "100.10");
    assert!(text_of(&notes[0]).contains("PM Ana can't take that: pm-ana serves ana only"));

    // Every envelope was acknowledged at once.
    for id in ["e1", "e2", "e3", "e4", "e5", "e6", "e7", "e8", "e9", "e10"] {
        let (acked, ack) = stack.slack.ack(id).unwrap();
        assert!(
            ack.get("payload").is_none(),
            "events are acked without a payload"
        );
        assert!(acked.duration_since(stack.slack.sent_at(id).unwrap()) < Duration::from_secs(3));
    }
    stack.stop().await;
}

#[tokio::test]
async fn notifications_edit_one_message_per_task_and_survive_a_restart() {
    let stack = Stack::new().await;
    let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    let now = chrono::Utc::now().to_rfc3339();
    let slack = |t: &str| json!({"channel": "D0ANA", "thread_ts": t});
    stack.feed(vec![
        json!({"id": "n0", "task_id": "tsk_0", "bot": "docs", "requested_by": "ana", "slack": slack("90.1"), "kind": "milestone", "text": "Old work", "at": old}),
        json!({"id": "n1", "task_id": "tsk_1", "bot": "docs", "requested_by": "ana", "slack": slack("100.2"), "kind": "milestone", "text": "Working on it", "at": now}),
        json!({"id": "n2", "task_id": "tsk_1", "bot": "docs", "requested_by": "ana", "slack": slack("100.2"), "kind": "milestone", "text": "Searching the support docs", "at": now}),
        json!({"id": "n3", "task_id": "tsk_1", "bot": "docs", "requested_by": "ana", "slack": slack("100.2"), "kind": "done", "text": "DSM needs **port 443** <open>. See [Ports](https://support.acme.com/ports).", "at": now}),
        json!({"id": "n4", "task_id": "tsk_2", "bot": "pm-ana", "requested_by": "ana", "slack": slack("100.1"), "kind": "failed", "text": "the tool server was unreachable", "at": now}),
    ]);
    let (stop, handle) = stack.notifier();
    let posts = stack.calls("chat.postMessage", 3).await;
    let updates = stack.calls("chat.update", 2).await;
    until("the cursor saved", || {
        std::fs::read_to_string(stack.state.path().join(CURSOR_FILE))
            .ok()
            .filter(|c| c == "5")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.cancel();
    handle.await.unwrap();
    assert_eq!(stack.slack.calls("chat.postMessage").len(), 3, "{posts:?}");
    assert_eq!(stack.slack.calls("chat.update").len(), 2);

    // The old item was skipped on the first start.
    assert!(posts.iter().all(|p| !text_of(p).contains("Old work")));
    // One progress message, posted at the first milestone as the bot...
    let progress = &posts[0];
    assert_eq!(progress.body["channel"], "D0ANA");
    assert_eq!(progress.body["thread_ts"], "100.2");
    assert_eq!(progress.body["username"], "Docs");
    assert!(text_of(progress).contains("Working on it"));
    let progress_ts = "1700000000.000001";
    // ...edited in place for the next milestone, then marked done.
    for u in &updates {
        assert_eq!(u.body["ts"], progress_ts);
        assert_eq!(u.body["channel"], "D0ANA");
    }
    assert!(text_of(&updates[0]).contains(":hourglass_flowing_sand: Searching the support docs"));
    assert!(text_of(&updates[1]).contains(":white_check_mark: Searching the support docs"));
    // The answer is a new message in the thread, as mrkdwn, escaped, with its link.
    let answer = &posts[1];
    assert_eq!(answer.body["thread_ts"], "100.2");
    assert_eq!(answer.body["username"], "Docs");
    let blocks = answer.body["blocks"].to_string();
    assert!(blocks.contains("*port 443* &lt;open&gt;"), "{blocks}");
    assert!(blocks.contains("<https://support.acme.com/ports|Ports>"));
    // A failure says why, as the bot that failed.
    let failed = &posts[2];
    assert_eq!(failed.body["username"], "PM Ana");
    assert_eq!(failed.body["thread_ts"], "100.1");
    assert!(text_of(failed).contains("I couldn't finish this: the tool server was unreachable"));
    // The thread now belongs to its bot.
    assert_eq!(
        stack.bots.thread_bot("D0ANA", "100.2").as_deref(),
        Some("docs")
    );

    // A restart picks up from the saved cursor and posts nothing again.
    let polls_before = stack.runtime.polls.lock().unwrap().len();
    let (stop, handle) = stack.notifier();
    until("a poll after the restart", || {
        let polls = stack.runtime.polls.lock().unwrap().clone();
        (polls.len() > polls_before).then(|| polls[polls_before].clone())
    })
    .await
    .map(|after| assert_eq!(after, "5"))
    .expect("the restart polls from the saved cursor");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        stack.slack.calls("chat.postMessage").len(),
        3,
        "nothing reposted"
    );

    // A routine's run has no Slack thread: it goes to the PM's DM, and its first message
    // starts the thread for the rest.
    stack.feed(vec![
        json!({"id": "n5", "task_id": "tsk_3", "bot": "pm-ana", "requested_by": "ana", "slack": null, "kind": "milestone", "text": "Reading #launch"}),
        json!({"id": "n6", "task_id": "tsk_3", "bot": "pm-ana", "requested_by": "ana", "slack": null, "kind": "done", "text": "Overnight in #launch: nothing urgent."}),
    ]);
    let posts = stack.calls("chat.postMessage", 5).await;
    stop.cancel();
    handle.await.unwrap();
    let (root, summary) = (&posts[3], &posts[4]);
    assert_eq!(root.body["channel"], "U0ANA");
    assert!(root.body.get("thread_ts").is_none());
    assert_eq!(summary.body["channel"], "D0ANA");
    assert_eq!(summary.body["thread_ts"], "1700000000.000004");
    assert_eq!(
        stack
            .bots
            .thread_bot("D0ANA", "1700000000.000004")
            .as_deref(),
        Some("pm-ana"),
        "a reply to the routine's thread goes to its bot"
    );
    stack.stop().await;
}

#[tokio::test]
async fn approval_cards_are_decided_by_their_own_pm() {
    let mut stack = Stack::new().await;
    {
        let mut items = stack.tools.items.lock().unwrap();
        items.insert(
            "apr_a1".into(),
            approval(
                "apr_a1",
                "ana",
                "confluence.create_draft",
                json!({"space": "PM", "title": "Launch plan", "body": "Draft body"}),
            ),
        );
        items.insert(
            "apr_a2".into(),
            approval(
                "apr_a2",
                "ana",
                "confluence.create_draft",
                json!({"space": "PM", "title": "Second", "body": "Draft body"}),
            ),
        );
        items.insert(
            "apr_a3".into(),
            approval(
                "apr_a3",
                "ana",
                "jira.create_issue",
                json!({"project": "DSM", "summary": "Fix <login>", "description": "Steps"}),
            ),
        );
    }
    stack.feed(vec![json!({
        "id": "n1", "task_id": "tsk_1", "bot": "docs", "requested_by": "ana",
        "slack": {"channel": "D0ANA", "thread_ts": "100.2"}, "kind": "needs_approval",
        "text": "I drafted three things.", "approvals": ["apr_a1", "apr_a2", "apr_a3"],
    })]);
    stack.connect();
    let (stop, handle) = stack.notifier();
    let posts = stack.calls("chat.postMessage", 4).await;
    assert!(text_of(&posts[0]).contains("I drafted three things."));
    let card = |id: &str| {
        posts
            .iter()
            .find(|p| {
                p.body["blocks"]
                    .to_string()
                    .contains(&format!("approval:{id}"))
            })
            .unwrap_or_else(|| panic!("no card for {id}"))
            .clone()
    };
    let (c1, c2, c3) = (card("apr_a1"), card("apr_a2"), card("apr_a3"));
    for c in [&c1, &c2, &c3] {
        assert_eq!(c.body["username"], "Docs");
        assert_eq!(c.body["thread_ts"], "100.2");
        let s = c.body["blocks"].to_string();
        for a in ["apr_approve", "apr_edit", "apr_reject"] {
            assert!(s.contains(a), "{s}");
        }
    }
    assert!(c3.body["blocks"]
        .to_string()
        .contains("*Summary:* Fix &lt;login&gt;"));
    let ts = |c: &Call| {
        c.body["ts"].as_str().map(String::from).unwrap_or_else(|| {
            // The fake numbers posts in order.
            let i = posts.iter().position(|p| p.body == c.body).unwrap();
            format!("1700000000.{:06}", i + 1)
        })
    };
    let (t1, t2, t3) = (ts(&c1), ts(&c2), ts(&c3));

    // Bob is on the team, but it isn't his to decide: a private note, no decision.
    let ack = stack
        .send(click("c1", "U0BOB", "apr_approve", "apr_a1", "D0ANA", &t1))
        .await;
    assert!(ack.get("payload").is_none());
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0BOB");
    assert!(text_of(&notes[0]).contains("Only Ana can decide this one"));
    // Someone outside the team: the same.
    stack
        .send(click(
            "c2",
            "U0STRANGER",
            "apr_reject",
            "apr_a1",
            "D0ANA",
            &t1,
        ))
        .await;
    stack.calls("chat.postEphemeral", 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stack.tools.decisions.lock().unwrap().is_empty(),
        "no call for anyone else"
    );

    // Ana approves: the card says so, with the link that was written.
    stack
        .send(click("c3", "U0ANA", "apr_approve", "apr_a1", "D0ANA", &t1))
        .await;
    let approved = until("the approved card", || {
        stack
            .slack
            .calls("chat.update")
            .into_iter()
            .find(|u| u.body["ts"] == t1.as_str() && text_of(u).contains("Approved by Ana"))
    })
    .await;
    assert!(text_of(&approved).contains("<https://confluence.fake.pmos.internal/CONF-A1|CONF-A1>"));
    assert!(
        !text_of(&approved).contains("apr_approve"),
        "no buttons once decided"
    );

    // Ana rejects the third: the card asks why, and a reason decides it.
    stack
        .send(click("c4", "U0ANA", "apr_reject", "apr_a3", "D0ANA", &t3))
        .await;
    until("the reasons", || {
        stack
            .slack
            .calls("chat.update")
            .into_iter()
            .find(|u| u.body["ts"] == t3.as_str() && text_of(u).contains("apr_reason_incomplete"))
    })
    .await;
    stack
        .send(click(
            "c4r",
            "U0ANA",
            "apr_reason_incomplete",
            "apr_a3",
            "D0ANA",
            &t3,
        ))
        .await;
    until("the rejected card", || {
        stack.slack.calls("chat.update").into_iter().find(|u| {
            u.body["ts"] == t3.as_str() && text_of(u).contains("Rejected by Ana: Incomplete")
        })
    })
    .await;

    // Ana edits the second: a modal, prefilled with its editable fields.
    stack
        .send(click("c5", "U0ANA", "apr_edit", "apr_a2", "D0ANA", &t2))
        .await;
    let opened = stack.calls("views.open", 1).await;
    assert_eq!(opened[0].body["trigger_id"], "trigger-c5");
    let view = &opened[0].body["view"];
    let inputs: Vec<(String, String)> = view["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "input")
        .map(|b| {
            (
                b["block_id"].as_str().unwrap().to_string(),
                b["element"]["initial_value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        inputs,
        vec![
            ("field:title".into(), "Second".into()),
            ("field:body".into(), "Draft body".into())
        ]
    );
    let submit = |envelope_id: &str, title: &str| {
        json!({
            "envelope_id": envelope_id, "type": "interactive",
            "payload": {
                "type": "view_submission", "user": {"id": "U0ANA"},
                "view": {
                    "callback_id": view["callback_id"], "private_metadata": view["private_metadata"],
                    "state": {"values": {
                        "field:title": {"value": {"type": "plain_text_input", "value": title}},
                        "field:body": {"value": {"type": "plain_text_input", "value": "Better body"}},
                    }},
                },
            },
        })
    };
    // An emptied field keeps the modal open, with an error; nothing is decided.
    let ack = stack.send(submit("v1", " ")).await;
    assert_eq!(ack["payload"]["response_action"], "errors");
    assert!(ack["payload"]["errors"]["field:title"].is_string());
    // The edit is approved with the edited arguments; the rest are kept.
    let ack = stack.send(submit("v2", "Better title")).await;
    assert!(ack.get("payload").is_none(), "the modal closes");
    until("the edited card", || {
        stack.slack.calls("chat.update").into_iter().find(|u| {
            u.body["ts"] == t2.as_str() && text_of(u).contains("Edited and approved by Ana")
        })
    })
    .await;

    // A second tap on a decided card changes nothing.
    stack
        .send(click("c6", "U0ANA", "apr_approve", "apr_a1", "D0ANA", &t1))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let decisions = stack.tools.decisions.lock().unwrap().clone();
    let seen: Vec<(String, Value)> = decisions.into_iter().map(|(_, id, b)| (id, b)).collect();
    assert_eq!(
        seen,
        vec![
            (
                "apr_a1".into(),
                json!({"decided_by": "ana", "decision": "approve"})
            ),
            (
                "apr_a3".into(),
                json!({"decided_by": "ana", "decision": "reject", "reason": "incomplete"})
            ),
            (
                "apr_a2".into(),
                json!({"decided_by": "ana", "decision": "edit", "args": {"space": "PM", "title": "Better title", "body": "Better body"}})
            ),
        ]
    );
    stop.cancel();
    handle.await.unwrap();
    stack.stop().await;
}

/// The blocks of the latest `chat.update` of the message at `ts`, once one matches `want`.
async fn card_update(stack: &Stack, ts: &str, what: &str, want: impl Fn(&str) -> bool) -> Call {
    until(what, || {
        stack
            .slack
            .calls("chat.update")
            .into_iter()
            .rev()
            .find(|u| u.body["ts"] == ts && want(&text_of(u)))
    })
    .await
}

#[tokio::test]
async fn reject_asks_why_and_back_restores_the_card() {
    let mut stack = Stack::new().await;
    stack.tools.items.lock().unwrap().insert(
        "apr_r1".into(),
        approval(
            "apr_r1",
            "ana",
            "jira.create_issue",
            json!({"project": "DSM", "summary": "Fix <login>", "description": "Steps"}),
        ),
    );
    stack.connect();
    let ts = "1700000000.000042";
    let decisions = || stack.tools.decisions.lock().unwrap().len();

    // Reject turns the card into the six reasons and Back; nothing is decided yet.
    let ack = stack
        .send(click("r1", "U0ANA", "apr_reject", "apr_r1", "D0ANA", ts))
        .await;
    assert!(
        ack.get("payload").is_none(),
        "acked at once, the work follows"
    );
    let reasons = card_update(&stack, ts, "the reasons", |t| t.contains("apr_back")).await;
    let buttons: Vec<(String, String, String)> = reasons.body["blocks"]
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
        .collect();
    let labels: Vec<&str> = buttons.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Wrong facts",
            "Wrong place",
            "Tone",
            "Incomplete",
            "Shouldn't have drafted",
            "Other",
            "Back"
        ]
    );
    assert!(buttons.iter().all(|b| b.2 == "apr_r1"));
    assert!(!text_of(&reasons).contains("apr_approve"));
    assert!(
        text_of(&reasons).contains("*Summary:* Fix &lt;login&gt;"),
        "the draft stays on the card"
    );
    assert_eq!(decisions(), 0);

    // Back restores Approve, Edit and Reject.
    stack
        .send(click("r2", "U0ANA", "apr_back", "apr_r1", "D0ANA", ts))
        .await;
    let back = card_update(&stack, ts, "the card back", |t| t.contains("apr_approve")).await;
    let s = text_of(&back);
    assert!(s.contains("apr_edit") && s.contains("apr_reject") && !s.contains("apr_reason_"));
    assert_eq!(decisions(), 0);

    // Someone else's reason click is refused, and nothing is sent.
    stack
        .send(click("r3", "U0ANA", "apr_reject", "apr_r1", "D0ANA", ts))
        .await;
    card_update(&stack, ts, "the reasons again", |t| t.contains("apr_back")).await;
    stack
        .send(click(
            "r4",
            "U0BOB",
            "apr_reason_tone",
            "apr_r1",
            "D0ANA",
            ts,
        ))
        .await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0BOB");
    assert!(text_of(&notes[0]).contains("Only Ana can decide this one"));
    stack
        .send(click("r5", "U0BOB", "apr_back", "apr_r1", "D0ANA", ts))
        .await;
    stack.calls("chat.postEphemeral", 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(decisions(), 0, "no decision for anyone else");

    // Ana's reason decides it, with the reason.
    stack
        .send(click(
            "r6",
            "U0ANA",
            "apr_reason_tone",
            "apr_r1",
            "D0ANA",
            ts,
        ))
        .await;
    card_update(&stack, ts, "the rejected card", |t| {
        t.contains("Rejected by Ana: Tone")
    })
    .await;
    let sent: Vec<(String, Value)> = stack
        .tools
        .decisions
        .lock()
        .unwrap()
        .iter()
        .map(|(_, id, b)| (id.clone(), b.clone()))
        .collect();
    assert_eq!(
        sent,
        vec![(
            "apr_r1".into(),
            json!({"decided_by": "ana", "decision": "reject", "reason": "tone"})
        )]
    );

    // A reason or Back on a card decided meanwhile shows the outcome, and sends nothing.
    stack
        .send(click("r7", "U0ANA", "apr_back", "apr_r1", "D0ANA", ts))
        .await;
    stack
        .send(click(
            "r8",
            "U0ANA",
            "apr_reason_other",
            "apr_r1",
            "D0ANA",
            ts,
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let last = stack.slack.calls("chat.update").last().unwrap().clone();
    assert!(text_of(&last).contains("Rejected by Ana"));
    assert!(!text_of(&last).contains("apr_approve"));
    assert_eq!(decisions(), 1);

    // Every click was acknowledged inside Slack's 3 seconds.
    for id in ["r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8"] {
        let (acked, _) = stack.slack.ack(id).unwrap();
        assert!(acked.duration_since(stack.slack.sent_at(id).unwrap()) < Duration::from_secs(3));
    }
    stack.stop().await;
}

#[tokio::test]
async fn routines_are_made_and_changed_in_slack() {
    let mut stack = Stack::new().await;
    stack.connect();
    let parses = |n: usize| {
        let rt = stack.runtime.clone();
        async move {
            until(&format!("{n} parses"), || {
                let p = rt.parses.lock().unwrap().clone();
                (p.len() >= n).then_some(p)
            })
            .await
        }
    };

    // A schedule is parsed in the PM's time zone, as their personal bot, and read back.
    stack
        .send(dm(
            "e1",
            "Ev1",
            "U0ANA",
            "200.1",
            None,
            "every weekday at 8am summarize #launch",
        ))
        .await;
    let p = parses(1).await;
    assert_eq!(p[0].0.as_deref(), Some("ana"));
    assert_eq!(
        p[0].1,
        json!({"bot": "pm-ana", "owner": "ana", "text": "every weekday at 8am summarize #launch", "tz": "Europe/London"})
    );
    let readback = stack.calls("chat.postMessage", 1).await[0].clone();
    assert_eq!(readback.body["username"], "PM Ana");
    assert_eq!(readback.body["thread_ts"], "200.1");
    assert!(text_of(&readback).contains("Every weekday at 8:00 Europe/London time"));
    let key = button_value(&readback.body, "rtn_create");
    assert_eq!(button_value(&readback.body, "rtn_cancel"), key);
    let readback_ts = "1700000000.000001";

    // Bob can't confirm Ana's draft.
    stack
        .send(click(
            "c0",
            "U0BOB",
            "rtn_create",
            &key,
            "D0ANA",
            readback_ts,
        ))
        .await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert!(text_of(&notes[0]).contains("Only Ana can confirm this routine"));
    // Create.
    stack
        .send(click(
            "c1",
            "U0ANA",
            "rtn_create",
            &key,
            "D0ANA",
            readback_ts,
        ))
        .await;
    let created = until("the created routine", || {
        stack
            .slack
            .calls("chat.update")
            .into_iter()
            .find(|u| text_of(u).contains("Routine created"))
    })
    .await;
    assert_eq!(created.body["ts"], readback_ts);
    {
        let writes = stack.runtime.routine_writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "POST");
        assert_eq!(writes[0].2.as_deref(), Some("ana"));
        assert_eq!(writes[0].3["draft"]["schedule"]["cron"], "0 8 * * 1-5");
    }

    // `routine: …`, from Bob: no personal bot and no Slack time zone, so docs and the default.
    stack
        .send(dm(
            "e2",
            "Ev2",
            "U0BOB",
            "200.2",
            None,
            "routine: monday digest of DSM docs changes",
        ))
        .await;
    let p = parses(2).await;
    assert_eq!(p[1].1["bot"], "docs");
    assert_eq!(p[1].1["tz"], "UTC");
    assert_eq!(p[1].1["text"], "monday digest of DSM docs changes");
    let bob_readback = stack.calls("chat.postMessage", 2).await[1].clone();
    assert_eq!(bob_readback.body["username"], "Docs");
    let bob_key = button_value(&bob_readback.body, "rtn_cancel");
    stack
        .send(click(
            "c2",
            "U0BOB",
            "rtn_cancel",
            &bob_key,
            "D0BOB",
            "1700000000.000002",
        ))
        .await;
    until("the cancelled draft", || {
        stack
            .slack
            .calls("chat.update")
            .into_iter()
            .find(|u| text_of(u).contains("Cancelled; nothing was scheduled"))
    })
    .await;

    // Unclear: the bot asks, and the answer in the thread completes the request.
    stack
        .send(dm(
            "e3",
            "Ev3",
            "U0ANA",
            "200.3",
            None,
            "every day sometime, check the DSM queue",
        ))
        .await;
    let question = stack.calls("chat.postMessage", 3).await[2].clone();
    assert_eq!(question.body["username"], "PM Ana");
    assert_eq!(question.body["thread_ts"], "200.3");
    assert!(text_of(&question).contains("What time should I run it?"));
    stack
        .send(dm("e4", "Ev4", "U0ANA", "200.4", Some("200.3"), "at 9am"))
        .await;
    let p = parses(4).await;
    assert_eq!(
        p[3].1["text"],
        "every day sometime, check the DSM queue\nat 9am"
    );
    let second = stack.calls("chat.postMessage", 4).await[3].clone();
    assert!(text_of(&second).contains("rtn_create"));
    assert_eq!(
        stack.runtime.tasks.lock().unwrap().len(),
        0,
        "routine requests never become tasks"
    );

    // "my routines": the list, with Pause and Delete.
    stack
        .send(dm("e5", "Ev5", "U0ANA", "200.5", None, "my routines"))
        .await;
    let list = stack.calls("chat.postMessage", 5).await[4].clone();
    assert_eq!(list.body["thread_ts"], "200.5");
    assert!(
        list.body.get("username").is_none(),
        "the list is from PM OS, not one bot"
    );
    assert!(text_of(&list).contains("*PM Ana*"));
    assert_eq!(button_value(&list.body, "rtn_pause"), "rtn_1");
    let list_ts = "1700000000.000005";
    let updated_list = |n: usize| {
        let slack = stack.slack.clone();
        async move {
            until(&format!("list update {n}"), || {
                let u: Vec<Call> = slack
                    .calls("chat.update")
                    .into_iter()
                    .filter(|u| u.body["ts"] == list_ts)
                    .collect();
                (u.len() >= n).then(|| u[n - 1].clone())
            })
            .await
        }
    };
    stack
        .send(click("c3", "U0ANA", "rtn_pause", "rtn_1", "D0ANA", list_ts))
        .await;
    let paused = updated_list(1).await;
    assert!(text_of(&paused).contains("_paused_"));
    assert_eq!(button_value(&paused.body, "rtn_resume"), "rtn_1");
    stack
        .send(click(
            "c4",
            "U0ANA",
            "rtn_resume",
            "rtn_1",
            "D0ANA",
            list_ts,
        ))
        .await;
    let resumed = updated_list(2).await;
    assert_eq!(button_value(&resumed.body, "rtn_pause"), "rtn_1");
    stack
        .send(click(
            "c5",
            "U0ANA",
            "rtn_delete",
            "rtn_1",
            "D0ANA",
            list_ts,
        ))
        .await;
    let deleted = updated_list(3).await;
    assert!(text_of(&deleted).contains("You have no routines"));

    let writes: Vec<RoutineWrite> = stack.runtime.routine_writes.lock().unwrap().clone();
    assert_eq!(
        writes[1..].to_vec(),
        vec![
            (
                "PATCH".into(),
                "rtn_1".into(),
                Some("ana".into()),
                json!({"enabled": false})
            ),
            (
                "PATCH".into(),
                "rtn_1".into(),
                Some("ana".into()),
                json!({"enabled": true})
            ),
            (
                "DELETE".into(),
                "rtn_1".into(),
                Some("ana".into()),
                Value::Null
            ),
        ]
    );
    stack.stop().await;
}

#[tokio::test]
async fn envelopes_are_acked_before_slow_work() {
    let mut stack = Stack::new().await;
    *stack.runtime.delay.lock().unwrap() = Duration::from_millis(1500);
    *stack.tools.delay.lock().unwrap() = Duration::from_millis(1500);
    stack.tools.items.lock().unwrap().insert(
        "apr_s1".into(),
        approval(
            "apr_s1",
            "ana",
            "slack.post_reply",
            json!({"channel": "C1", "thread_ts": "1.0", "text": "Shipping Friday"}),
        ),
    );
    stack.connect();

    stack
        .send(dm(
            "e1",
            "Ev1",
            "U0ANA",
            "300.1",
            None,
            "what changed in DSM 5.0?",
        ))
        .await;
    stack
        .send(click(
            "c1",
            "U0ANA",
            "apr_approve",
            "apr_s1",
            "D0ANA",
            "1700000000.000009",
        ))
        .await;
    let task_at = until("the slow task", || {
        stack.runtime.tasks.lock().unwrap().first().map(|t| t.0)
    })
    .await;
    let decided_at = until("the slow decision", || {
        stack.tools.decisions.lock().unwrap().first().map(|d| d.0)
    })
    .await;
    for (id, work_at) in [("e1", task_at), ("c1", decided_at)] {
        let sent = stack.slack.sent_at(id).unwrap();
        let (acked, _) = stack.slack.ack(id).unwrap();
        assert!(
            acked.duration_since(sent) < Duration::from_secs(1),
            "{id} acked well inside Slack's 3 s"
        );
        assert!(acked < work_at, "{id}: the ack comes before the work");
    }
    // The card shows the decision is under way before the slow write returns.
    let first_update = stack.calls("chat.update", 1).await[0].clone();
    assert!(text_of(&first_update).contains("Approving…"));
    assert!(first_update.at < decided_at);
    stack.calls("chat.update", 2).await;
    stack.stop().await;
}
