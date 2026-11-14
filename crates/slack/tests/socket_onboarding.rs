//! Onboarding in Slack end to end (`docs/onboarding-spec.md` §4): the fake Slack (`common`),
//! a mock bots runtime for the roster, and a mock tool server with the members, onboarding,
//! OAuth-start and memory-repo endpoints of §3.
//!
//! - A stranger taps **Request access**: the request is filed and every admin gets a card.
//! - An admin approves: every card shows it, and the new member gets a welcome and the
//!   checklist. A non-admin's tap is refused; a second admin's tap finds it decided.
//! - `github_not_linked` gets a **Connect GitHub** button and leaves the card alone.
//! - `setup` renders each state; the memory-repo modal shows refusals as field errors.
//! - `remove` works with its confirmation; `requests` lists the pending cards.
//! - Every envelope is acknowledged within 3 seconds.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{serve, until, Call, FakeSlack, APP_TOKEN, SLACK_BOT_TOKEN};
use pmos_contract::model::{Role, TeamFile, TeamMember};
use pmos_contract::onboarding::{propose_handle, render_team_yaml, valid_new_handle};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::Commands;
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
const TEAM: &str = "\
# Who may use PM OS.
members:
  - {handle: ana, slack-id: U0ANA, email: ana@fortanix.com, roles: [pm, admin]}
  - {handle: cal, slack-id: U0CAL, email: cal@fortanix.com, roles: [pm, admin]}
  - {handle: bob, slack-id: U0BOB, email: bob@fortanix.com, roles: [pm]}
";

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

// ---- Mock tool server (spec §3) ----

#[derive(Default)]
struct MockTools {
    team_file: Mutex<PathBuf>,
    requests: Mutex<Vec<Value>>,
    /// Every write: (path, body).
    writes: Mutex<Vec<(String, Value)>>,
    /// Admins' GitHub: `push`, or `no_push`; missing is not linked.
    github: Mutex<HashMap<String, &'static str>>,
    /// The providers each member has linked.
    linked: Mutex<HashMap<String, HashSet<String>>>,
    memory: Mutex<HashMap<String, String>>,
    memory_delay: Mutex<Duration>,
    commits: Mutex<u32>,
}

impl MockTools {
    fn team(&self) -> (String, TeamFile) {
        let text = std::fs::read_to_string(&*self.team_file.lock().unwrap()).unwrap();
        let team = TeamFile::from_yaml(&text).unwrap();
        (text, team)
    }

    fn write_team(&self, original: &str, members: &[TeamMember]) -> String {
        std::fs::write(
            &*self.team_file.lock().unwrap(),
            render_team_yaml(original, members),
        )
        .unwrap();
        let mut n = self.commits.lock().unwrap();
        *n += 1;
        format!("abc{:037x}", *n)
    }

    fn wrote(&self, path: &str) -> Vec<Value> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with(path))
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn link(&self, handle: &str, provider: &str) {
        self.linked
            .lock()
            .unwrap()
            .entry(handle.into())
            .or_default()
            .insert(provider.into());
    }
}

type Ts = State<Arc<MockTools>>;

macro_rules! authed {
    ($headers:expr) => {
        if bearer(&$headers) != TOOL_BOT_TOKEN {
            return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
        }
    };
}

async fn ts_new_request(State(ts): Ts, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    authed!(headers);
    ts.writes
        .lock()
        .unwrap()
        .push(("/internal/members/requests".into(), body.clone()));
    let slack_id = body["slack_id"].as_str().unwrap_or_default();
    let email = body["email"].as_str().unwrap_or_default();
    if !slack_id.starts_with('U') || !email.contains('@') {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "bad request", "code": "bad_request"}),
        );
    }
    let (_, team) = ts.team();
    if team.by_slack_id(slack_id).is_some() {
        return answer(
            StatusCode::CONFLICT,
            json!({"error": "already a member", "code": "already_member"}),
        );
    }
    let mut requests = ts.requests.lock().unwrap();
    if let Some(r) = requests
        .iter()
        .find(|r| r["slack_id"] == slack_id && r["status"] == "pending")
    {
        return answer(StatusCode::OK, json!({"request": r}));
    }
    let taken = team
        .members
        .iter()
        .map(|m| m.handle.clone())
        .chain(
            requests
                .iter()
                .filter(|r| r["status"] == "pending")
                .map(|r| r["proposed_handle"].as_str().unwrap().to_string()),
        )
        .collect();
    let request = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "slack_id": slack_id, "email": email, "name": body["name"],
        "proposed_handle": propose_handle(email, body["name"].as_str().unwrap_or_default(), &taken),
        "status": "pending", "requested_at": "2026-11-14T10:00:00Z",
    });
    requests.push(request.clone());
    answer(StatusCode::CREATED, json!({"request": request}))
}

async fn ts_requests(
    State(ts): Ts,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    assert_eq!(q.get("status").map(String::as_str), Some("pending"));
    let pending: Vec<Value> = ts
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "pending")
        .cloned()
        .collect();
    answer(StatusCode::OK, json!({"requests": pending}))
}

async fn ts_decide(
    State(ts): Ts,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    authed!(headers);
    ts.writes.lock().unwrap().push((
        format!("/internal/members/requests/{id}/decision"),
        body.clone(),
    ));
    let (original, team) = ts.team();
    let admin = body["decided_by"].as_str().unwrap_or_default();
    if !team.by_handle(admin).is_some_and(|m| m.is_admin()) {
        return answer(StatusCode::FORBIDDEN, json!({"error": "admins only"}));
    }
    let mut requests = ts.requests.lock().unwrap();
    let Some(request) = requests.iter_mut().find(|r| r["id"] == id.as_str()) else {
        return answer(StatusCode::NOT_FOUND, json!({"error": "no such request"}));
    };
    if request["status"] != "pending" {
        return answer(
            StatusCode::CONFLICT,
            json!({"error": "already decided", "code": "already_decided", "request": request}),
        );
    }
    if body["decision"] == "reject" {
        request["status"] = json!("rejected");
        request["decided_by"] = json!(admin);
        return answer(StatusCode::OK, json!({"request": request}));
    }
    let handle = body["handle"]
        .as_str()
        .or(request["proposed_handle"].as_str())
        .unwrap()
        .to_string();
    if !valid_new_handle(&handle) || team.by_handle(&handle).is_some() {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "that handle is taken", "code": "bad_handle"}),
        );
    }
    match ts.github.lock().unwrap().get(admin) {
        None => {
            return answer(
                StatusCode::CONFLICT,
                json!({"error": "link GitHub first", "code": "github_not_linked"}),
            )
        }
        Some(&"no_push") => {
            return answer(
                StatusCode::CONFLICT,
                json!({"error": "no push access", "code": "no_push"}),
            )
        }
        Some(_) => {}
    }
    let mut members = team.members.clone();
    members.push(TeamMember {
        handle: handle.clone(),
        slack_id: request["slack_id"].as_str().unwrap().into(),
        email: request["email"].as_str().map(String::from),
        github: None,
        roles: vec![Role::Pm],
    });
    let commit = ts.write_team(&original, &members);
    request["status"] = json!("approved");
    request["decided_by"] = json!(admin);
    request["handle"] = json!(handle);
    request["commit"] = json!(commit);
    answer(StatusCode::OK, json!({"request": request}))
}

async fn ts_remove(
    State(ts): Ts,
    Path(handle): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    authed!(headers);
    ts.writes
        .lock()
        .unwrap()
        .push((format!("/internal/members/{handle}/remove"), body.clone()));
    let (original, team) = ts.team();
    let admin = body["decided_by"].as_str().unwrap_or_default();
    if !team.by_handle(admin).is_some_and(|m| m.is_admin()) || admin == handle {
        return answer(StatusCode::FORBIDDEN, json!({"error": "no"}));
    }
    if team.by_handle(&handle).is_none() {
        return answer(StatusCode::NOT_FOUND, json!({"error": "not a member"}));
    }
    let members: Vec<TeamMember> = team
        .members
        .into_iter()
        .filter(|m| m.handle != handle)
        .collect();
    let commit = ts.write_team(&original, &members);
    answer(StatusCode::OK, json!({"handle": handle, "commit": commit}))
}

async fn ts_onboarding(
    State(ts): Ts,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    let (_, team) = ts.team();
    let Some(member) = team.by_handle(&user) else {
        return answer(StatusCode::NOT_FOUND, json!({"error": "not a member"}));
    };
    let linked = ts
        .linked
        .lock()
        .unwrap()
        .get(&user)
        .cloned()
        .unwrap_or_default();
    answer(
        StatusCode::OK,
        json!({
            "member": true, "handle": user, "admin": member.is_admin(),
            "linked": {
                "atlassian": linked.contains("atlassian"),
                "slack": linked.contains("slack"),
                "github": linked.contains("github"),
            },
            "memory_repo": ts.memory.lock().unwrap().get(&user),
            "personal_bot": null,
            "pending_request": null,
        }),
    )
}

async fn ts_oauth_start(
    State(ts): Ts,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    if ts.team().1.by_handle(&user).is_none() {
        return answer(StatusCode::FORBIDDEN, json!({"error": "members only"}));
    }
    answer(
        StatusCode::OK,
        json!({"url": format!("https://pmos.example.com/oauth/{provider}/start?state=s-{user}")}),
    )
}

async fn ts_memory_repo(State(ts): Ts, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    authed!(headers);
    let delay = *ts.memory_delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    ts.writes
        .lock()
        .unwrap()
        .push(("/internal/memory/repo".into(), body.clone()));
    let user = body["user"].as_str().unwrap_or_default().to_string();
    let repo = body["repo"].as_str().unwrap_or_default().to_string();
    let linked = ts
        .linked
        .lock()
        .unwrap()
        .get(&user)
        .is_some_and(|l| l.contains("github"));
    if !linked {
        return answer(
            StatusCode::CONFLICT,
            json!({"error": "link GitHub", "code": "github_not_linked"}),
        );
    }
    let (org, name) = repo.split_once('/').unwrap();
    let code = if org != "fortanix" {
        Some("not_in_org")
    } else if name.contains("public") {
        Some("not_private")
    } else if name.contains("readonly") {
        Some("no_push")
    } else if name.contains("missing") {
        Some("not_found")
    } else {
        None
    };
    if let Some(code) = code {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": format!("refused: {code}"), "code": code}),
        );
    }
    ts.memory.lock().unwrap().insert(user, repo.clone());
    answer(
        StatusCode::OK,
        json!({"repo": repo, "verified_at": "2026-11-14T10:00:00Z"}),
    )
}

fn tools_routes(ts: Arc<MockTools>) -> Router {
    Router::new()
        .route(
            "/internal/members/requests",
            post(ts_new_request).get(ts_requests),
        )
        .route("/internal/members/requests/{id}/decision", post(ts_decide))
        .route("/internal/members/{handle}/remove", post(ts_remove))
        .route("/internal/onboarding", get(ts_onboarding))
        .route("/internal/oauth/{provider}/start", get(ts_oauth_start))
        .route("/internal/memory/repo", post(ts_memory_repo))
        .with_state(ts)
}

// ---- Mock runtime: the roster, and tasks ----

#[derive(Default)]
struct MockRuntime {
    roster: Mutex<Vec<Value>>,
    tasks: Mutex<Vec<Value>>,
}

type Rt = State<Arc<MockRuntime>>;

async fn rt_bots(State(rt): Rt, headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
    }
    answer(StatusCode::OK, json!(rt.roster.lock().unwrap().clone()))
}

async fn rt_create_task(State(rt): Rt, Json(body): Json<Value>) -> Response {
    rt.tasks.lock().unwrap().push(body.clone());
    let mut task = body;
    task["id"] = json!("tsk_00000000000000000000000000000001");
    answer(StatusCode::CREATED, task)
}

fn bot(name: &str) -> Value {
    json!({"name": name, "role": "x", "scope": "team", "owner": null, "description": ""})
}

// ---- The stack ----

async fn hook(State(s): State<Arc<FakeSlack>>, Json(body): Json<Value>) -> Json<Value> {
    s.responses.lock().unwrap().push((Instant::now(), body));
    Json(json!({"ok": true}))
}

struct Stack {
    slack: Arc<FakeSlack>,
    slack_base: String,
    live: mpsc::UnboundedSender<Value>,
    runtime: Arc<MockRuntime>,
    tools: Arc<MockTools>,
    _team: tempfile::TempDir,
    stop: Vec<CancellationToken>,
    socket: Option<(CancellationToken, JoinHandle<Ended>)>,
    next: Mutex<u32>,
}

impl Stack {
    async fn new() -> Self {
        let slack = Arc::new(FakeSlack::default());
        let app = common::routes()
            .route("/hooks/bots", post(hook))
            .with_state(slack.clone());
        let (slack_base, stop_slack) = serve(app).await;
        *slack.base.lock().unwrap() = slack_base.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        *slack.live.lock().unwrap() = Some(rx);
        slack.users.lock().unwrap().extend([
            (
                "U0BEN".to_string(),
                json!({"name": "ben", "real_name": "Ben <Smith>", "profile": {"email": "ben.smith@fortanix.com", "real_name": "Ben <Smith>"}}),
            ),
            (
                "U0DEE".to_string(),
                json!({"name": "dee", "real_name": "Dee", "profile": {"email": "dee@fortanix.com"}}),
            ),
            (
                "U0NOMAIL".to_string(),
                json!({"name": "nomail", "real_name": "No Mail", "profile": {}}),
            ),
        ]);

        let team = tempfile::tempdir().unwrap();
        let team_file = team.path().join("team.yaml");
        std::fs::write(&team_file, TEAM).unwrap();
        let tools = Arc::new(MockTools::default());
        *tools.team_file.lock().unwrap() = team_file.clone();
        tools.github.lock().unwrap().insert("ana".into(), "push");
        let (ts_base, stop_ts) = serve(tools_routes(tools.clone())).await;
        let runtime = Arc::new(MockRuntime::default());
        runtime.roster.lock().unwrap().push(bot("docs"));
        let (rt_base, stop_rt) = serve(
            Router::new()
                .route("/v1/bots", get(rt_bots))
                .route("/v1/tasks", post(rt_create_task))
                .with_state(runtime.clone()),
        )
        .await;

        let http = reqwest::Client::new();
        let bots = Bots::new(BotsConfig {
            slack: SlackApi::new(slack_base.clone(), SLACK_BOT_TOKEN, http.clone()),
            runtime: Runtime::new(rt_base, BOTS_TOKEN, http.clone()),
            toolserver: ToolServer::new(ts_base.clone(), TOOL_BOT_TOKEN),
            team_file: team_file.clone(),
            default_tz: "UTC".into(),
            github_org: Some("fortanix".into()),
            response_url_prefix: format!("{slack_base}/hooks/"),
        });
        let socket = Socket::new(
            slack_base.clone(),
            APP_TOKEN,
            Commands {
                toolserver: ToolServer::new(ts_base, TOOL_BOT_TOKEN),
                team_file,
                http: http.clone(),
                response_url_prefix: format!("{slack_base}/hooks/"),
                request_access: true,
            },
            http,
        )
        .with_bots(bots);
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let handle = tokio::spawn(async move { socket.run(token).await });
        Self {
            slack,
            slack_base,
            live: tx,
            runtime,
            tools,
            _team: team,
            stop: vec![stop_slack, stop_ts, stop_rt],
            socket: Some((shutdown, handle)),
            next: Mutex::new(0),
        }
    }

    fn envelope_id(&self) -> String {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        format!("e{n}")
    }

    /// Send a payload in an envelope, wait for its ack, and return the ack.
    async fn send(&self, kind: &str, payload: Value) -> Value {
        let id = self.envelope_id();
        self.live
            .send(json!({"envelope_id": id, "type": kind, "payload": payload}))
            .unwrap();
        let (acked, ack) = until(&format!("the ack for {id}"), || self.slack.ack(&id)).await;
        let sent = self.slack.sent_at(&id).unwrap();
        assert!(
            acked.duration_since(sent) < Duration::from_secs(3),
            "{id} acked after {:?}",
            acked.duration_since(sent)
        );
        ack
    }

    async fn dm(&self, user: &str, text: &str) -> Value {
        let id = self.envelope_id();
        let ts = format!("200.{}", id.trim_start_matches('e'));
        self.send(
            "events_api",
            json!({"type": "event_callback", "event_id": format!("Ev{id}"), "event": {
                "type": "message", "channel_type": "im", "channel": dm_channel(user),
                "user": user, "text": text, "ts": ts,
            }}),
        )
        .await
    }

    /// A tap on a button in a message the bot posted.
    async fn click(&self, user: &str, action_id: &str, value: &str, at: (&str, &str)) -> Value {
        self.send(
            "interactive",
            json!({
                "type": "block_actions", "user": {"id": user}, "trigger_id": "trigger-1",
                "container": {"type": "message", "channel_id": at.0, "message_ts": at.1},
                "channel": {"id": at.0},
                "actions": [{"type": "button", "action_id": action_id, "value": value}],
            }),
        )
        .await
    }

    /// A tap on a button in an ephemeral note, which carries a `response_url`.
    async fn click_note(&self, user: &str, action_id: &str) -> Value {
        self.send(
            "interactive",
            json!({
                "type": "block_actions", "user": {"id": user}, "trigger_id": "trigger-2",
                "container": {"type": "message", "channel_id": dm_channel(user), "message_ts": "1.0", "is_ephemeral": true},
                "channel": {"id": dm_channel(user)},
                "response_url": format!("{}/hooks/bots", self.slack_base),
                "actions": [{"type": "button", "action_id": action_id, "value": "request"}],
            }),
        )
        .await
    }

    /// Submit the modal opened by `views.open` call `opened`, with `block` set to `value`.
    async fn submit(&self, user: &str, opened: &Call, block: &str, value: &str) -> Value {
        let view = &opened.body["view"];
        self.send(
            "interactive",
            json!({
                "type": "view_submission", "user": {"id": user},
                "view": {
                    "id": "V1", "callback_id": view["callback_id"],
                    "private_metadata": view["private_metadata"],
                    "state": {"values": {block: {"value": {"type": "plain_text_input", "value": value}}}},
                },
            }),
        )
        .await
    }

    async fn calls(&self, method: &str, n: usize) -> Vec<Call> {
        until(&format!("{n} {method} calls"), || {
            let calls = self.slack.calls(method);
            (calls.len() >= n).then_some(calls)
        })
        .await
    }

    async fn responses(&self, n: usize) -> Vec<Value> {
        until(&format!("{n} responses"), || {
            let r: Vec<Value> = self
                .slack
                .responses
                .lock()
                .unwrap()
                .iter()
                .map(|(_, b)| b.clone())
                .collect();
            (r.len() >= n).then_some(r)
        })
        .await
    }

    /// Ben asks for access; returns the request id and the admins' cards `(channel, ts)`.
    async fn ben_requests(&self) -> (String, Vec<(String, String)>) {
        let before = self.slack.calls("chat.postMessage").len();
        self.click_note("U0BEN", "pmos_request_access").await;
        let posts = self.calls("chat.postMessage", before + 2).await;
        let cards: Vec<(String, String)> = posts[before..]
            .iter()
            .map(|c| {
                (
                    c.body["channel"].as_str().unwrap().to_string(),
                    posted_ts(&self.slack, c),
                )
            })
            .collect();
        let id = button_value(&posts[before].body, "pmos_member_approve");
        (id, cards)
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

fn dm_channel(user: &str) -> String {
    format!("D{}", user.trim_start_matches('U'))
}

/// The ts the fake Slack gave a posted message: posts are numbered in order.
fn posted_ts(slack: &FakeSlack, call: &Call) -> String {
    let n = slack
        .calls("chat.postMessage")
        .iter()
        .position(|c| c.at == call.at && c.body == call.body)
        .unwrap()
        + 1;
    format!("1700000000.{n:06}")
}

fn text_of(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

/// Every element (buttons and accessories) in a message's blocks.
fn elements(body: &Value) -> Vec<Value> {
    body["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| {
            let mut els = b["elements"].as_array().cloned().unwrap_or_default();
            if b["accessory"].is_object() {
                els.push(b["accessory"].clone());
            }
            els
        })
        .collect()
}

fn button_value(body: &Value, action_id: &str) -> String {
    elements(body)
        .iter()
        .find(|e| e["action_id"] == action_id)
        .and_then(|e| e["value"].as_str().map(String::from))
        .unwrap_or_else(|| panic!("no {action_id} button in {body}"))
}

fn link_urls(body: &Value) -> Vec<String> {
    elements(body)
        .iter()
        .filter_map(|e| e["url"].as_str().map(String::from))
        .collect()
}

fn has_action(body: &Value, action_id: &str) -> bool {
    elements(body).iter().any(|e| e["action_id"] == action_id)
}

#[tokio::test]
async fn a_stranger_requests_access_and_an_admin_approves() {
    let stack = Stack::new().await;

    // A DM from someone not in team.yaml: the note, with Request access.
    stack.dm("U0BEN", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0BEN");
    assert!(text_of(&notes[0]).contains("PM OS is for the Fortanix product team."));
    assert!(has_action(&notes[0].body, "pmos_request_access"));
    assert!(stack.runtime.tasks.lock().unwrap().is_empty());

    // So does /docs, in its ack.
    let ack = stack
        .send(
            "slash_commands",
            json!({"command": "/docs", "text": "saml", "user_id": "U0ZED", "channel_id": "C1",
                   "response_url": format!("{}/hooks/bots", stack.slack_base)}),
        )
        .await;
    assert_eq!(ack["payload"]["response_type"], "ephemeral");
    assert!(ack["payload"]["blocks"]
        .to_string()
        .contains("pmos_request_access"));

    // The tap: users.info, the request, a card for each admin, and the reply.
    let (id, cards) = stack.ben_requests().await;
    let filed = stack.tools.wrote("/internal/members/requests");
    assert_eq!(
        filed,
        vec![
            json!({"slack_id": "U0BEN", "email": "ben.smith@fortanix.com", "name": "Ben <Smith>"})
        ]
    );
    assert_eq!(stack.slack.calls("users.info").len(), 1);
    let opened: Vec<Value> = stack
        .slack
        .calls("conversations.open")
        .iter()
        .map(|c| c.body["users"].clone())
        .collect();
    assert_eq!(opened, vec![json!("U0ANA"), json!("U0CAL")]);
    let posts = stack.slack.calls("chat.postMessage");
    assert_eq!(
        cards.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["D0ANA", "D0CAL"]
    );
    for card in &posts {
        let s = text_of(card);
        assert!(s.contains("Ben &lt;Smith&gt;"), "{s}");
        assert!(s.contains("ben.smith@fortanix.com"));
        assert!(s.contains("`ben-smith`"));
        for action in [
            "pmos_member_approve",
            "pmos_member_edit",
            "pmos_member_reject",
        ] {
            assert_eq!(button_value(&card.body, action), id);
        }
    }
    let replies = stack.responses(1).await;
    assert_eq!(replies[0]["replace_original"], true);
    assert!(replies[0]["text"]
        .as_str()
        .unwrap()
        .contains("Sent to the platform owners. I'll DM you when it's decided."));

    // A second tap finds the request already pending: no new cards.
    stack.click_note("U0BEN", "pmos_request_access").await;
    let replies = stack.responses(2).await;
    assert!(replies[1]["text"]
        .as_str()
        .unwrap()
        .contains("already with the platform owners"));
    // No email in Slack: said so, and nothing is filed.
    stack.click_note("U0NOMAIL", "pmos_request_access").await;
    let replies = stack.responses(3).await;
    assert!(replies[2]["text"]
        .as_str()
        .unwrap()
        .contains("Slack has no email address for you"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(stack.slack.calls("chat.postMessage").len(), 2);
    assert_eq!(stack.tools.wrote("/internal/members/requests").len(), 2);

    // A member who isn't an admin can't decide.
    stack
        .click(
            "U0BOB",
            "pmos_member_approve",
            &id,
            ("D0BOB", "1700000000.000001"),
        )
        .await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert_eq!(notes[1].body["user"], "U0BOB");
    assert!(text_of(&notes[1]).contains("Only platform owners can decide this."));
    assert!(stack.tools.wrote("/decision").is_empty());

    // Ana approves: both cards show it, and Ben gets a welcome and his checklist.
    let (ana_ch, ana_ts) = &cards[0];
    stack
        .click("U0ANA", "pmos_member_approve", &id, (ana_ch, ana_ts))
        .await;
    let updates = stack.calls("chat.update", 2).await;
    assert_eq!(
        stack.tools.wrote("/decision"),
        vec![json!({"decision": "approve", "decided_by": "ana"})]
    );
    let mut updated: Vec<(String, String)> = updates
        .iter()
        .map(|u| {
            (
                u.body["channel"].as_str().unwrap().to_string(),
                u.body["ts"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    updated.sort();
    assert_eq!(updated, cards, "every admin's card");
    for u in &updates {
        let s = text_of(u);
        assert!(
            s.contains(":white_check_mark: Approved by Ana: `ben-smith` added (commit `abc0000`)"),
            "{s}"
        );
        assert!(
            !has_action(&u.body, "pmos_member_approve"),
            "no buttons left"
        );
    }
    let posts = stack.calls("chat.postMessage", 4).await;
    assert!(stack
        .slack
        .calls("conversations.open")
        .iter()
        .any(|c| c.body["users"] == "U0BEN"));
    let (welcome, checklist) = (&posts[2], &posts[3]);
    assert_eq!(welcome.body["channel"], "D0BEN");
    assert!(text_of(welcome).contains("Welcome to PM OS, Ben!"));
    assert!(text_of(welcome).contains("Ana approved you as `ben-smith`"));
    assert_eq!(checklist.body["channel"], "D0BEN");
    assert_eq!(
        link_urls(&checklist.body),
        ["atlassian", "slack", "github"]
            .map(|p| format!("https://pmos.example.com/oauth/{p}/start?state=s-ben-smith"))
    );
    assert!(has_action(&checklist.body, "pmos_setup_memory"));
    assert!(text_of(checklist).contains("`pm-ben-smith`: being set up (a minute or two)"));

    // Cal taps his card later: already decided, so his card shows the outcome; no second DM.
    let (cal_ch, cal_ts) = &cards[1];
    stack
        .click("U0CAL", "pmos_member_approve", &id, (cal_ch, cal_ts))
        .await;
    let updates = stack.calls("chat.update", 3).await;
    assert_eq!(updates[2].body["channel"], "D0CAL");
    assert!(text_of(&updates[2]).contains("Approved by Ana: `ben-smith` added"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(stack.slack.calls("chat.postMessage").len(), 4);

    // Ben is a member now (the tool server committed team.yaml): his DMs go to the bots.
    stack.dm("U0BEN", "what ports does DSM need?").await;
    until("Ben's task", || {
        let tasks = stack.runtime.tasks.lock().unwrap().clone();
        (!tasks.is_empty()).then_some(tasks)
    })
    .await;
    stack.stop().await;
}

#[tokio::test]
async fn github_not_linked_reject_and_another_handle() {
    let stack = Stack::new().await;
    let (id, cards) = stack.ben_requests().await;

    // Cal hasn't linked GitHub: a private note with Connect GitHub; the card stays.
    stack
        .click(
            "U0CAL",
            "pmos_member_approve",
            &id,
            (&cards[1].0, &cards[1].1),
        )
        .await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0CAL");
    assert!(text_of(&notes[0]).contains("your GitHub account has to be linked first"));
    assert_eq!(
        link_urls(&notes[0].body),
        ["https://pmos.example.com/oauth/github/start?state=s-cal"]
    );
    // Linked, but no push to the content repo.
    stack
        .tools
        .github
        .lock()
        .unwrap()
        .insert("cal".into(), "no_push");
    stack
        .click(
            "U0CAL",
            "pmos_member_approve",
            &id,
            (&cards[1].0, &cards[1].1),
        )
        .await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(text_of(&notes[1]).contains("can't push to the content repo"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stack.slack.calls("chat.update").is_empty(),
        "cards unchanged"
    );

    // Ana approves with another handle: the modal, prefilled, checked before the ack.
    stack
        .click("U0ANA", "pmos_member_edit", &id, (&cards[0].0, &cards[0].1))
        .await;
    let opened = stack.calls("views.open", 1).await;
    let view = &opened[0].body["view"];
    assert_eq!(view["callback_id"], "pmos_member_handle");
    assert_eq!(view["blocks"][1]["element"]["initial_value"], "ben-smith");
    let ack = stack
        .submit("U0ANA", &opened[0], "handle", "Ben Smith!")
        .await;
    assert_eq!(ack["payload"]["response_action"], "errors");
    assert!(ack["payload"]["errors"]["handle"]
        .as_str()
        .unwrap()
        .contains("lowercase"));
    let ack = stack.submit("U0ANA", &opened[0], "handle", "bob").await;
    assert_eq!(
        ack["payload"]["errors"]["handle"],
        "Someone already has that handle."
    );
    // Only the first two Cal taps reached the tool server.
    assert_eq!(stack.tools.wrote("/decision").len(), 2);
    let ack = stack.submit("U0ANA", &opened[0], "handle", "benny").await;
    assert!(ack.get("payload").is_none(), "closes the modal");
    let updates = stack.calls("chat.update", 2).await;
    assert_eq!(
        stack.tools.wrote("/decision")[2],
        json!({"decision": "approve", "decided_by": "ana", "handle": "benny"})
    );
    assert!(text_of(&updates[0]).contains("Approved by Ana: `benny` added"));
    let posts = stack.calls("chat.postMessage", 4).await;
    assert!(text_of(&posts[2]).contains("as `benny`"));

    // Dee asks, and Ana rejects: the cards say so, and Dee is told.
    stack.click_note("U0DEE", "pmos_request_access").await;
    let posts = stack.calls("chat.postMessage", 6).await;
    let dee = button_value(&posts[4].body, "pmos_member_reject");
    let dee_card = (
        posts[4].body["channel"].as_str().unwrap().to_string(),
        posted_ts(&stack.slack, &posts[4]),
    );
    stack
        .click(
            "U0ANA",
            "pmos_member_reject",
            &dee,
            (&dee_card.0, &dee_card.1),
        )
        .await;
    let updates = stack.calls("chat.update", 4).await;
    assert!(updates[2..]
        .iter()
        .all(|u| text_of(u).contains(":no_entry_sign: Rejected by Ana")));
    let posts = stack.calls("chat.postMessage", 7).await;
    assert_eq!(posts[6].body["channel"], "D0DEE");
    assert!(text_of(&posts[6]).contains("turned down your request"));
    stack.stop().await;
}

#[tokio::test]
async fn setup_renders_each_state_and_the_memory_modal_shows_field_errors() {
    let stack = Stack::new().await;

    // Nothing set up: three Connect links, Set memory repo, the bot being set up.
    stack.dm("U0BOB", "setup").await;
    let posts = stack.calls("chat.postMessage", 1).await;
    let checklist = &posts[0];
    assert_eq!(checklist.body["channel"], "D0BOB");
    assert!(checklist.body.get("thread_ts").is_none(), "a new message");
    assert_eq!(link_urls(&checklist.body).len(), 3);
    assert!(has_action(&checklist.body, "pmos_setup_memory"));
    assert!(has_action(&checklist.body, "pmos_setup_refresh"));
    assert!(text_of(checklist).contains("`pm-bob`: being set up"));
    assert!(text_of(checklist).contains("Tap *Refresh* after each step."));
    let at = ("D0BOB", "1700000000.000001");

    // `docs: setup` is a question for the docs bot.
    stack.dm("U0BOB", "docs: setup").await;
    let tasks = until("the docs task", || {
        let t = stack.runtime.tasks.lock().unwrap().clone();
        (!t.is_empty()).then_some(t)
    })
    .await;
    assert_eq!(tasks[0]["bot"], "docs");
    assert_eq!(tasks[0]["text"], "setup");

    // The memory-repo modal, prefilled from PMOS_GITHUB_ORG.
    stack
        .click("U0BOB", "pmos_setup_memory", "memory", at)
        .await;
    let opened = stack.calls("views.open", 1).await;
    let view = &opened[0].body["view"];
    assert_eq!(view["callback_id"], "pmos_memory_repo");
    assert_eq!(
        view["blocks"][1]["element"]["initial_value"],
        "fortanix/pm-os-memory-bob"
    );
    assert!(view.to_string().contains("as a *private* repo first"));

    // Each refusal is a sentence under the field, in the ack.
    let error_for = |ack: &Value| {
        ack["payload"]["errors"]["repo"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let ack = stack
        .submit("U0BOB", &opened[0], "repo", "fortanix/pm-os-memory-bob")
        .await;
    assert_eq!(ack["payload"]["response_action"], "errors");
    assert!(error_for(&ack).contains("Connect GitHub first"));
    stack.tools.link("bob", "github");
    for (repo, says) in [
        ("acme/pm-os-memory-bob", "isn't in the fortanix GitHub org"),
        ("fortanix/public-notes", "That repo is public"),
        ("fortanix/readonly", "can't push to that repo"),
        ("fortanix/missing", "GitHub can't find that repo"),
        (
            "not a repo",
            "Use org/name, like fortanix/pm-os-memory-bob.",
        ),
    ] {
        let ack = stack.submit("U0BOB", &opened[0], "repo", repo).await;
        assert!(error_for(&ack).contains(says), "{repo}: {ack}");
    }
    // A tool server too slow for the ack: still acked in time, with a field error.
    *stack.tools.memory_delay.lock().unwrap() = Duration::from_secs(4);
    let ack = stack
        .submit("U0BOB", &opened[0], "repo", "fortanix/pm-os-memory-bob")
        .await;
    assert!(error_for(&ack).contains("couldn't check that repo in time"));
    *stack.tools.memory_delay.lock().unwrap() = Duration::ZERO;
    // Accepted: the modal closes and the checklist updates in place.
    let ack = stack
        .submit("U0BOB", &opened[0], "repo", "fortanix/pm-os-memory-bob")
        .await;
    assert!(ack.get("payload").is_none(), "{ack}");
    let updates = stack.calls("chat.update", 1).await;
    assert_eq!(updates[0].body["channel"], at.0);
    assert_eq!(updates[0].body["ts"], at.1);
    assert!(text_of(&updates[0])
        .contains("4. :white_check_mark: *Memory repo:* `fortanix/pm-os-memory-bob`"));
    assert!(text_of(&updates[0]).contains("3. :white_check_mark: *GitHub*"));
    assert_eq!(link_urls(&updates[0].body).len(), 2);
    assert_eq!(
        stack
            .tools
            .memory
            .lock()
            .unwrap()
            .get("bob")
            .map(String::as_str),
        Some("fortanix/pm-os-memory-bob")
    );

    // Everything done: Refresh shows it all ticked.
    stack.tools.link("bob", "atlassian");
    stack.tools.link("bob", "slack");
    stack.runtime.roster.lock().unwrap().push(bot("pm-bob"));
    stack
        .click("U0BOB", "pmos_setup_refresh", "refresh", at)
        .await;
    let updates = stack.calls("chat.update", 2).await;
    let done = text_of(&updates[1]);
    assert!(done.contains("1. :white_check_mark: *Jira and Confluence*"));
    assert!(done.contains("2. :white_check_mark: *Slack*"));
    assert!(done.contains("5. :white_check_mark: *Your bot* `pm-bob`"));
    assert!(done.contains("All set"));
    assert!(link_urls(&updates[1].body).is_empty());

    // `status` is the same checklist.
    stack.dm("U0BOB", "Status").await;
    let posts = stack.calls("chat.postMessage", 2).await;
    assert!(text_of(&posts[1]).contains("All set"));
    stack.stop().await;
}

#[tokio::test]
async fn remove_works_with_its_confirmation_and_requests_lists_cards() {
    let stack = Stack::new().await;

    stack.dm("U0ANA", "requests").await;
    let posts = stack.calls("chat.postMessage", 1).await;
    assert!(text_of(&posts[0]).contains("No access requests are waiting."));
    let (id, _) = stack.ben_requests().await;
    stack.dm("U0ANA", "requests").await;
    let posts = stack.calls("chat.postMessage", 4).await;
    assert_eq!(posts[3].body["channel"], "D0ANA");
    assert_eq!(button_value(&posts[3].body, "pmos_member_approve"), id);

    // For anyone but an admin, `remove ana` is just a message to their bot.
    stack.dm("U0BOB", "remove ana").await;
    until("Bob's task", || {
        let t = stack.runtime.tasks.lock().unwrap().clone();
        (!t.is_empty()).then_some(t)
    })
    .await;
    // An admin can't remove themselves, or someone who isn't a member.
    stack.dm("U0ANA", "remove ana").await;
    stack.calls("chat.postMessage", 5).await;
    stack.dm("U0ANA", "remove nobody").await;
    let posts = stack.calls("chat.postMessage", 6).await;
    assert!(text_of(&posts[4]).contains("You can't remove yourself"));
    assert!(text_of(&posts[5]).contains("`nobody` isn't a PM OS member."));

    // The confirmation, then Cancel.
    stack.dm("U0ANA", "remove bob").await;
    let posts = stack.calls("chat.postMessage", 7).await;
    let confirm = &posts[6];
    assert!(text_of(confirm).contains("Remove `bob` (<@U0BOB>) from PM OS?"));
    assert_eq!(
        button_value(&confirm.body, "pmos_member_remove_confirm"),
        "bob"
    );
    assert_eq!(
        button_value(&confirm.body, "pmos_member_remove_cancel"),
        "bob"
    );
    let card = ("D0ANA", "1700000000.000007");
    stack
        .click("U0ANA", "pmos_member_remove_cancel", "bob", card)
        .await;
    let updates = stack.calls("chat.update", 1).await;
    assert!(text_of(&updates[0]).contains("Cancelled; nothing was changed."));
    // A non-admin's tap on Remove is refused.
    stack
        .click("U0BOB", "pmos_member_remove_confirm", "bob", card)
        .await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert!(text_of(&notes[0]).contains("Only platform owners can remove members."));
    assert!(stack.tools.wrote("/remove").is_empty());

    // Remove: the tool server commits, and the card says so.
    stack.dm("U0ANA", "remove bob").await;
    stack.calls("chat.postMessage", 8).await;
    let card = ("D0ANA", "1700000000.000008");
    stack
        .click("U0ANA", "pmos_member_remove_confirm", "bob", card)
        .await;
    let updates = stack.calls("chat.update", 3).await;
    assert_eq!(
        stack.tools.wrote("/internal/members/bob/remove"),
        vec![json!({"decided_by": "ana"})]
    );
    assert!(text_of(&updates[1]).contains("Removing…"));
    assert_eq!(updates[2].body["ts"], card.1);
    assert!(
        text_of(&updates[2]).contains(":white_check_mark: Removed `bob` by Ana (commit `abc0000`)"),
        "{}",
        text_of(&updates[2])
    );

    // Bob is no longer a member: his next DM gets the Request access note.
    stack.dm("U0BOB", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(has_action(&notes[1].body, "pmos_request_access"));
    stack.stop().await;
}
