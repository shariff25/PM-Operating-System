//! The stack for the tagging and App Home tests (`docs/slack-mentions-and-home-spec.md` §6):
//! the fake Slack (`common`), a mock bots runtime (roster, bot details, tasks, notifications)
//! and a mock tool server (onboarding, OAuth starts, the pending-approvals list, decisions and
//! unlinking), each recording what it was asked.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::Commands;
use pmos_slack::notify::Notifier;
use pmos_slack::runtime::Runtime;
use pmos_slack::slackapi::SlackApi;
use pmos_slack::socket::{Ended, Socket};
use pmos_slack::toolserver::ToolServer;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::common::{self, serve, until, Call, FakeSlack, APP_TOKEN, SLACK_BOT_TOKEN};

pub const BOTS_TOKEN: &str = "test-bots-token-slack-0123456789abcdef";
pub const TOOL_BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
/// The app's own bot user, as `authorizations[0].user_id`.
pub const APP_USER: &str = "UPMOSAPP";
/// Ana has a personal bot and Slack linked; Bob has no personal bot; Cat has one but hasn't
/// linked Slack.
const TEAM: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n  - {handle: bob, slack-id: U0BOB, roles: [pm]}\n  - {handle: cat, slack-id: U0CAT, roles: [pm]}\n";

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

fn answer(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

// ---- Mock runtime ----

/// A routine write: (method, routine id, X-Pmos-User, body).
pub type RoutineWrite = (String, String, Option<String>, Value);

#[derive(Default)]
pub struct MockRuntime {
    /// `POST /v1/tasks`: (X-Pmos-User, body).
    pub tasks: Mutex<Vec<(Option<String>, Value)>>,
    /// Refuse every task with a 503.
    pub fail_tasks: Mutex<bool>,
    /// `GET /v1/bots/{name}`: the names asked for.
    pub details: Mutex<Vec<String>>,
    /// Each bot's role's skills.
    pub role_skills: Mutex<HashMap<String, Vec<String>>>,
    pub feed: Mutex<Vec<Value>>,
    /// `member_settings`: each handle's level; unset reads as `senior`.
    pub levels: Mutex<HashMap<String, String>>,
    /// The settings routes answer `503`.
    pub settings_down: Mutex<bool>,
    /// `PUT /v1/members/{handle}/settings`: (X-Pmos-User, handle, body).
    pub settings_writes: Mutex<Vec<(Option<String>, String, Value)>>,
    pub routines: Mutex<Vec<Value>>,
    /// Routine writes: (method, routine id, X-Pmos-User, body).
    pub routine_writes: Mutex<Vec<RoutineWrite>>,
}

type Rt = State<Arc<MockRuntime>>;

fn pmos_user(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-pmos-user")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

/// The runtime's settings checks: the slack client, for the PM in `X-Pmos-User` only.
fn settings_refusal(rt: &MockRuntime, headers: &HeaderMap, handle: &str) -> Option<Response> {
    if bearer(headers) != BOTS_TOKEN {
        return Some(answer(
            StatusCode::UNAUTHORIZED,
            json!({"error": "bad token"}),
        ));
    }
    if pmos_user(headers).as_deref() != Some(handle) {
        return Some(answer(
            StatusCode::FORBIDDEN,
            json!({"error": "own settings only"}),
        ));
    }
    if *rt.settings_down.lock().unwrap() {
        return Some(answer(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "storage unavailable"}),
        ));
    }
    None
}

async fn rt_get_settings(
    State(rt): Rt,
    Path(handle): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(no) = settings_refusal(&rt, &headers, &handle) {
        return no;
    }
    let level = rt
        .levels
        .lock()
        .unwrap()
        .get(&handle)
        .cloned()
        .unwrap_or_else(|| "senior".into());
    Json(json!({"handle": handle, "level": level})).into_response()
}

async fn rt_put_settings(
    State(rt): Rt,
    Path(handle): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    rt.settings_writes
        .lock()
        .unwrap()
        .push((pmos_user(&headers), handle.clone(), body.clone()));
    if let Some(no) = settings_refusal(&rt, &headers, &handle) {
        return no;
    }
    let level = body["level"].as_str().unwrap_or_default().to_string();
    if !["senior", "staff", "principal"].contains(&level.as_str()) {
        return answer(
            StatusCode::BAD_REQUEST,
            json!({"error": "use senior, staff or principal"}),
        );
    }
    rt.levels
        .lock()
        .unwrap()
        .insert(handle.clone(), level.clone());
    Json(json!({"handle": handle, "level": level})).into_response()
}

async fn rt_create_routine(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let user = pmos_user(&headers);
    rt.routine_writes
        .lock()
        .unwrap()
        .push(("POST".into(), String::new(), user, body.clone()));
    let mut routines = rt.routines.lock().unwrap();
    let mut routine = body["draft"].clone();
    routine["id"] = json!(format!("rtn_{}", routines.len() + 1));
    routine["enabled"] = json!(true);
    routine["next_run_at"] = json!("2026-12-01T09:00:00Z");
    routines.push(routine.clone());
    answer(StatusCode::CREATED, routine)
}

async fn rt_routines(
    State(rt): Rt,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let owner = q.get("owner").cloned().unwrap_or_default();
    if pmos_user(&headers).as_deref() != Some(owner.as_str()) {
        return answer(StatusCode::FORBIDDEN, json!({"error": "own routines only"}));
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
    match routines
        .iter_mut()
        .find(|r| r["id"] == id.as_str() && r["owner"].as_str() == user.as_deref())
    {
        Some(r) => {
            if let Some(e) = body.get("enabled") {
                r["enabled"] = e.clone();
            }
            Json(r.clone()).into_response()
        }
        None => answer(StatusCode::NOT_FOUND, json!({"error": "no such routine"})),
    }
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
        return answer(StatusCode::NOT_FOUND, json!({"error": "no such routine"}));
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn rt_bots(headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
    }
    Json(json!([
        {"name": "docs", "role": "docs-helper", "scope": "team", "description": "Docs"},
        {"name": "pm-ana", "role": "pm-assistant", "scope": "personal", "owner": "ana"},
        {"name": "pm-cat", "role": "pm-assistant", "scope": "personal", "owner": "cat"},
    ]))
    .into_response()
}

async fn rt_bot(State(rt): Rt, Path(name): Path<String>, headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
    }
    rt.details.lock().unwrap().push(name.clone());
    let skills = rt.role_skills.lock().unwrap().get(&name).cloned();
    match skills {
        Some(skills) => Json(json!({
            "bot": {"name": name, "role": "r"},
            "role": {"name": "r", "skills": skills},
            "tools": [], "actions": {},
        }))
        .into_response(),
        None => answer(StatusCode::NOT_FOUND, json!({"error": "no such bot"})),
    }
}

async fn rt_create_task(State(rt): Rt, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
    }
    let user = headers
        .get("x-pmos-user")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    rt.tasks.lock().unwrap().push((user, body.clone()));
    if *rt.fail_tasks.lock().unwrap() {
        return answer(StatusCode::SERVICE_UNAVAILABLE, json!({"error": "down"}));
    }
    let n = rt.tasks.lock().unwrap().len();
    let mut task = body;
    task["id"] = json!(format!("tsk_{n:032x}"));
    answer(StatusCode::CREATED, task)
}

async fn rt_notifications(State(rt): Rt, Query(q): Query<HashMap<String, String>>) -> Response {
    let from: usize = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    // A cursor past the feed (as after a runtime's database was reset): `409 cursor_ahead`,
    // with the feed's present.
    let len = rt.feed.lock().unwrap().len();
    if from > len {
        return answer(
            StatusCode::CONFLICT,
            json!({"code": "cursor_ahead", "error": "cursor is ahead of the feed",
                   "cursor": len.to_string()}),
        );
    }
    let items: Vec<Value> = rt.feed.lock().unwrap().iter().skip(from).cloned().collect();
    if items.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Json(json!({"cursor": (from + items.len()).to_string(), "items": items})).into_response()
}

// ---- Mock tool server ----

#[derive(Default)]
pub struct MockTools {
    /// Linked providers, by handle.
    pub linked: Mutex<HashMap<String, Vec<String>>>,
    /// Include `github` in the onboarding answer's `linked`.
    pub github_in_answer: Mutex<bool>,
    /// The `user` of each onboarding read.
    pub onboarding_reads: Mutex<Vec<String>>,
    /// Approval items, oldest first.
    pub items: Mutex<Vec<Value>>,
    /// A refusal to answer a decision on this item with: (status, code).
    pub refusals: Mutex<HashMap<String, (u16, String)>>,
    /// `GET /internal/approvals?user=`: the `user` of each.
    pub lists: Mutex<Vec<String>>,
    /// `POST /internal/approvals/{id}/decision`: (id, body).
    pub decisions: Mutex<Vec<(String, Value)>>,
    /// `DELETE /internal/credentials/{user}/{provider}`.
    pub unlinks: Mutex<Vec<(String, String)>>,
}

type Ts = State<Arc<MockTools>>;

macro_rules! authed {
    ($headers:expr) => {
        if bearer(&$headers) != TOOL_BOT_TOKEN {
            return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
        }
    };
}

async fn ts_onboarding(
    State(ts): Ts,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    ts.onboarding_reads.lock().unwrap().push(user.clone());
    let linked = ts
        .linked
        .lock()
        .unwrap()
        .get(&user)
        .cloned()
        .unwrap_or_default();
    let mut map = json!({
        "atlassian": linked.iter().any(|p| p == "atlassian"),
        "slack": linked.iter().any(|p| p == "slack"),
    });
    if *ts.github_in_answer.lock().unwrap() {
        map["github"] = json!(linked.iter().any(|p| p == "github"));
    }
    answer(
        StatusCode::OK,
        json!({"member": true, "handle": user, "linked": map, "memory_repo": "example-org/pm-os-memory-ana"}),
    )
}

async fn ts_oauth_start(
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    answer(
        StatusCode::OK,
        json!({"url": format!("https://pmos.example.com/oauth/{provider}/start?state=s-{user}")}),
    )
}

async fn ts_list(
    State(ts): Ts,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    ts.lists.lock().unwrap().push(user.clone());
    let mine: Vec<Value> = ts
        .items
        .lock()
        .unwrap()
        .iter()
        .filter(|i| i["user"] == user.as_str() && i["status"] == "pending")
        .cloned()
        .collect();
    let total = mine.len();
    answer(
        StatusCode::OK,
        json!({"approvals": mine.into_iter().take(50).collect::<Vec<_>>(), "total": total}),
    )
}

/// `GET /internal/approvals/{id}?user=`: `404` unless the item is `user`'s (S2).
async fn ts_get(
    State(ts): Ts,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    match ts
        .items
        .lock()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.as_str() && i["user"] == user.as_str())
    {
        Some(item) => answer(StatusCode::OK, item.clone()),
        None => answer(
            StatusCode::NOT_FOUND,
            json!({"error": "no such approval item"}),
        ),
    }
}

async fn ts_decide(
    State(ts): Ts,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    authed!(headers);
    ts.decisions
        .lock()
        .unwrap()
        .push((id.clone(), body.clone()));
    if let Some((status, code)) = ts.refusals.lock().unwrap().get(&id).cloned() {
        return answer(
            StatusCode::from_u16(status).unwrap(),
            json!({"error": format!("refused: {code}"), "code": code}),
        );
    }
    let mut items = ts.items.lock().unwrap();
    let Some(item) = items.iter_mut().find(|i| i["id"] == id.as_str()) else {
        return answer(
            StatusCode::NOT_FOUND,
            json!({"error": "approval item not found"}),
        );
    };
    if item["user"] != body["decided_by"] {
        return answer(StatusCode::FORBIDDEN, json!({"error": "not yours"}));
    }
    if item["status"] != "pending" {
        return answer(StatusCode::CONFLICT, json!({"error": "already decided"}));
    }
    // As the tool server (R4): an approve names what the PM was shown.
    match body["expected_args_hash"].as_str() {
        None if body["decision"] == "approve" => {
            return answer(
                StatusCode::BAD_REQUEST,
                json!({"code": "expected_args_hash_required", "error": "required"}),
            )
        }
        Some(h) if !item["args_hash"].as_str().unwrap_or("").starts_with(h) => {
            return answer(
                StatusCode::CONFLICT,
                json!({"code": "stale", "error": "changed"}),
            )
        }
        _ => {}
    }
    item["decided_by"] = body["decided_by"].clone();
    item["status"] = match body["decision"].as_str() {
        Some("approve") => json!("approved"),
        _ => json!("rejected"),
    };
    answer(StatusCode::OK, item.clone())
}

async fn ts_unlink(
    State(ts): Ts,
    Path((user, provider)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    authed!(headers);
    ts.unlinks
        .lock()
        .unwrap()
        .push((user.clone(), provider.clone()));
    if let Some(list) = ts.linked.lock().unwrap().get_mut(&user) {
        list.retain(|p| *p != provider);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// A pending approval item for `user`.
pub fn approval(id: &str, user: &str, tool: &str, args: Value, created_at: &str) -> Value {
    json!({
        "id": id, "user": user, "skill": null, "bot": format!("pm-{user}"), "run_id": "tsk_1",
        "tool": tool, "args": args, "args_hash": ARGS_HASH, "status": "pending",
        "created_at": created_at,
    })
}

/// Every fixture item's args hash; a card or App Home row carries its first 16 characters.
pub const ARGS_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const SHOWN: &str = "0123456789abcdef";

// ---- The stack ----

pub struct Stack {
    pub slack: Arc<FakeSlack>,
    live: mpsc::UnboundedSender<Value>,
    pub runtime: Arc<MockRuntime>,
    pub tools: Arc<MockTools>,
    pub bots: Bots,
    pub content: tempfile::TempDir,
    pub state: tempfile::TempDir,
    stop: Vec<CancellationToken>,
    socket: Option<(CancellationToken, JoinHandle<Ended>)>,
}

impl Stack {
    pub async fn new() -> Self {
        let slack = Arc::new(FakeSlack::default());
        let (slack_base, stop_slack) = serve(common::routes().with_state(slack.clone())).await;
        *slack.base.lock().unwrap() = slack_base.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        *slack.live.lock().unwrap() = Some(rx);

        let runtime = Arc::new(MockRuntime::default());
        let (rt_base, stop_rt) = serve(
            Router::new()
                .route("/v1/bots", get(rt_bots))
                .route("/v1/bots/{name}", get(rt_bot))
                .route("/v1/tasks", post(rt_create_task))
                .route("/v1/notifications", get(rt_notifications))
                .route(
                    "/v1/members/{handle}/settings",
                    get(rt_get_settings).put(rt_put_settings),
                )
                .route("/v1/routines", post(rt_create_routine).get(rt_routines))
                .route(
                    "/v1/routines/{id}",
                    axum::routing::patch(rt_patch_routine).delete(rt_delete_routine),
                )
                .with_state(runtime.clone()),
        )
        .await;
        let tools = Arc::new(MockTools::default());
        tools.linked.lock().unwrap().extend([
            ("ana".to_string(), vec!["slack".to_string()]),
            ("bob".to_string(), vec!["slack".to_string()]),
        ]);
        let (ts_base, stop_ts) = serve(
            Router::new()
                .route("/internal/onboarding", get(ts_onboarding))
                .route("/internal/oauth/{provider}/start", get(ts_oauth_start))
                .route("/internal/approvals", get(ts_list))
                .route("/internal/approvals/{id}", get(ts_get))
                .route("/internal/approvals/{id}/decision", post(ts_decide))
                .route("/internal/credentials/{user}/{provider}", delete(ts_unlink))
                .with_state(tools.clone()),
        )
        .await;

        let content = tempfile::tempdir().unwrap();
        let team_file = content.path().join("team.yaml");
        std::fs::write(&team_file, TEAM).unwrap();
        let state = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let bots = Bots::new(BotsConfig {
            slack: SlackApi::new(slack_base, SLACK_BOT_TOKEN, http.clone()),
            runtime: Runtime::new(rt_base, BOTS_TOKEN, http.clone()),
            toolserver: ToolServer::new(ts_base, TOOL_BOT_TOKEN),
            team_file,
            default_tz: "UTC".into(),
            github_org: None,
            response_url_prefix: "https://hooks.slack.com/".into(),
            state_dir: state.path().to_path_buf(),
            web_url: None,
        });
        let mut stack = Self {
            slack,
            live: tx,
            runtime,
            tools,
            bots,
            content,
            state,
            stop: vec![stop_slack, stop_rt, stop_ts],
            socket: None,
        };
        stack.connect();
        stack
    }

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

    /// Write a skill into the content repo.
    pub fn skill(&self, name: &str, description: &str, surfaces: &str, track: Option<&str>) {
        let dir = self.content.path().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let track = track
            .map(|t| format!("  track: \"{t}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\nmetadata:\n  owner: \"ana\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"docs.search\"\n  surfaces: \"{surfaces}\"\n{track}---\n\n# {name}\n\nDo the thing.\n"
            ),
        )
        .unwrap();
    }

    /// Start the notifications loop.
    pub fn notifier(&self) -> (CancellationToken, JoinHandle<()>) {
        let shutdown = CancellationToken::new();
        let mut notifier = Notifier::new(self.bots.clone(), self.state.path());
        notifier.wait = Duration::from_millis(200);
        let handle = tokio::spawn(notifier.run(shutdown.clone()));
        (shutdown, handle)
    }

    /// Send an envelope and wait for its acknowledgement.
    pub async fn send(&self, envelope: Value) -> Value {
        let id = envelope["envelope_id"].as_str().unwrap().to_string();
        self.live.send(envelope).unwrap();
        until(&format!("the ack for {id}"), || self.slack.ack(&id))
            .await
            .1
    }

    /// Wait for at least `n` Web API calls of `method`.
    pub async fn calls(&self, method: &str, n: usize) -> Vec<Call> {
        until(&format!("{n} {method} calls"), || {
            let calls = self.slack.calls(method);
            (calls.len() >= n).then_some(calls)
        })
        .await
    }

    /// The `n`th `views.publish` (1-based), once it has arrived.
    pub async fn published(&self, n: usize) -> Value {
        self.calls("views.publish", n).await[n - 1].body.clone()
    }

    pub async fn stop(mut self) {
        if let Some((shutdown, handle)) = self.socket.take() {
            shutdown.cancel();
            assert_eq!(handle.await.unwrap(), Ended::Shutdown);
        }
        for s in &self.stop {
            s.cancel();
        }
    }
}

/// A DM envelope from `user`, in their DM channel (`D` + their id without the `U`).
pub fn dm(envelope_id: &str, user: &str, ts: &str, text: &str) -> Value {
    let channel = format!("D{}", user.trim_start_matches('U'));
    json!({
        "envelope_id": envelope_id, "type": "events_api",
        "payload": {"type": "event_callback", "event_id": format!("Ev-{envelope_id}"), "event": {
            "type": "message", "channel_type": "im", "channel": channel, "user": user,
            "text": text, "ts": ts, "event_ts": ts,
        }},
    })
}

/// The `text` and blocks of a Web API call, as one string.
pub fn call_text(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

/// An `app_mention` envelope.
pub fn mention(
    envelope_id: &str,
    event_id: &str,
    user: &str,
    channel: &str,
    ts: &str,
    thread_ts: Option<&str>,
    text: &str,
) -> Value {
    let mut event = json!({
        "type": "app_mention", "user": user, "text": text, "ts": ts, "channel": channel,
        "event_ts": ts,
    });
    if let Some(t) = thread_ts {
        event["thread_ts"] = json!(t);
    }
    json!({
        "envelope_id": envelope_id, "type": "events_api", "accepts_response_payload": false,
        "payload": {
            "type": "event_callback", "event_id": event_id, "event": event,
            "authorizations": [{"user_id": APP_USER, "is_bot": true}],
        },
    })
}

/// An `app_home_opened` envelope; `tab` is the section in the view's metadata, if any.
pub fn home_opened(envelope_id: &str, event_id: &str, user: &str, tab: Option<&str>) -> Value {
    let mut event = json!({"type": "app_home_opened", "user": user, "channel": "D0X", "tab": "home", "event_ts": "1.1"});
    if let Some(tab) = tab {
        event["view"] =
            json!({"type": "home", "private_metadata": json!({"tab": tab}).to_string()});
    }
    json!({
        "envelope_id": envelope_id, "type": "events_api", "accepts_response_payload": false,
        "payload": {"type": "event_callback", "event_id": event_id, "event": event},
    })
}

/// A click in the Home view `published` (a `views.publish` body), with these tick boxes
/// ticked.
pub fn home_click(
    envelope_id: &str,
    user: &str,
    action_id: &str,
    published: &Value,
    ticked: &[&str],
) -> Value {
    let mut values = serde_json::Map::new();
    // Each ticked row's block id as the view showed it (`pick_<id>:<hash>`), else `pick_<id>`.
    let shown = |id: &str| -> String {
        let plain = format!("pick_{id}");
        published["view"]["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["block_id"].as_str())
            .find(|b| *b == plain || b.starts_with(&format!("{plain}:")))
            .map(String::from)
            .unwrap_or(plain)
    };
    for id in ticked {
        values.insert(
            shown(id),
            json!({"home_pick": {"type": "checkboxes", "selected_options": [{"text": {"type": "plain_text", "text": "Select"}, "value": id}]}}),
        );
    }
    json!({
        "envelope_id": envelope_id, "type": "interactive", "accepts_response_payload": false,
        "payload": {
            "type": "block_actions", "user": {"id": user}, "trigger_id": "t",
            "container": {"type": "view", "view_id": "V1"},
            "view": {
                "id": "V1", "type": "home",
                "private_metadata": published["view"]["private_metadata"],
                "state": {"values": values},
            },
            "actions": [{"type": "button", "action_id": action_id, "block_id": "b", "value": "x"}],
        },
    })
}

/// A view's blocks as one string, for `contains`.
pub fn view_text(published: &Value) -> String {
    published["view"]["blocks"].to_string()
}

/// The section a published view is on.
pub fn tab_of(published: &Value) -> String {
    let meta: Value =
        serde_json::from_str(published["view"]["private_metadata"].as_str().unwrap()).unwrap();
    meta["tab"].as_str().unwrap().to_string()
}
