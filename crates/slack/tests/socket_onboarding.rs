//! Onboarding in Slack end to end (`docs/onboarding-spec.md` §4): the fake Slack (`common`),
//! a mock bots runtime for the roster, and a mock tool server with the members, onboarding,
//! OAuth-start and memory-repo endpoints of §3, and the user database's
//! `GET /internal/directory` (slice U4b), which the Slack service reads over HTTP with the bot
//! token on a clock the test moves.
//!
//! - A stranger taps **Request access**: the request is filed and every admin gets a card.
//! - An admin approves: every card shows it, and the new member gets a welcome and the
//!   checklist. A non-admin's tap is refused; a second admin's tap finds it decided.
//! - `github_not_linked` gets a **Connect GitHub** button and leaves the card alone.
//! - `setup` renders each state; the memory-repo modal shows refusals as field errors.
//! - `remove` deactivates with its confirmation, and each refusal of slice U3 (an Admin or
//!   Auditor from Slack, the hourly cap, self, already deactivated) is its own sentence;
//!   `requests` lists the pending cards.
//! - Every envelope is acknowledged within 3 seconds.

mod common;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{serve, until, Call, FakeSlack, APP_TOKEN, SLACK_BOT_TOKEN};
use pmos_contract::onboarding::{propose_handle, valid_new_handle};
use pmos_directory::snapshot::{FakeClock, HttpFetcher, SnapshotClient};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::Commands;
use pmos_slack::people::People;
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
/// The mock tool server's user database at the start: two admins and a PM. Aud is an Auditor
/// only, Gil was deactivated, Ivy is a PM whose Slack display name is hostile.
fn users() -> Value {
    json!({"version": 1, "users": [
        {"handle": "ana", "display_name": "Ana", "status": "active", "roles": ["pm", "admin"], "slack_id": "U0ANA"},
        {"handle": "cal", "display_name": "Cal", "status": "active", "roles": ["pm", "admin"], "slack_id": "U0CAL"},
        {"handle": "bob", "display_name": "Bob Jones", "status": "active", "roles": ["pm"], "slack_id": "U0BOB"},
        {"handle": "aud", "display_name": "Aud", "status": "active", "roles": ["auditor"], "slack_id": "U0AUD"},
        {"handle": "gil", "display_name": "Gil", "status": "deactivated", "roles": ["pm"], "slack_id": "U0GIL"},
        {"handle": "ivy", "display_name": "<!channel> *Ivy* <https://evil.example|x>", "status": "active", "roles": ["pm"], "slack_id": "U0IVY"},
    ]})
}

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
    /// The user database, `{version, users}`, kept in a file so it outlives a restart.
    users_file: Mutex<PathBuf>,
    /// Deactivations so far (the hourly cap is 3).
    deactivations: Mutex<u32>,
    /// The directory answers 503 (an outage).
    directory_down: Mutex<bool>,
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
    fn db(&self) -> Value {
        let text = std::fs::read_to_string(&*self.users_file.lock().unwrap()).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn save(&self, mut db: Value) {
        db["version"] = json!(db["version"].as_i64().unwrap() + 1);
        std::fs::write(&*self.users_file.lock().unwrap(), db.to_string()).unwrap();
    }

    fn user(&self, handle: &str) -> Option<Value> {
        self.db()["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["handle"] == handle)
            .cloned()
    }

    fn by_slack_id(&self, slack_id: &str) -> Option<Value> {
        self.db()["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["slack_id"] == slack_id)
            .cloned()
    }

    fn active_admin(&self, handle: &str) -> bool {
        self.user(handle).is_some_and(|u| {
            u["status"] == "active" && u["roles"].as_array().unwrap().contains(&json!("admin"))
        })
    }

    /// Commit the personal bot (as U3 still does): a fake sha.
    fn commit(&self) -> String {
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

/// `GET /internal/directory` for the bot token: every user with their Slack id, an ETag, and
/// `304` for the current one.
async fn ts_directory(State(ts): Ts, headers: HeaderMap) -> Response {
    authed!(headers);
    if *ts.directory_down.lock().unwrap() {
        return answer(StatusCode::SERVICE_UNAVAILABLE, json!({"error": "down"}));
    }
    let db = ts.db();
    let etag = format!("\"{}\"", db["version"]);
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [("etag", etag)],
        Json(json!({"version": db["version"], "users": db["users"]})),
    )
        .into_response()
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
    if let Some(u) = ts.by_slack_id(slack_id) {
        return if u["status"] == "deactivated" {
            answer(
                StatusCode::CONFLICT,
                json!({"error": "your access was turned off", "code": "deactivated"}),
            )
        } else {
            answer(
                StatusCode::CONFLICT,
                json!({"error": "already a member", "code": "already_member"}),
            )
        };
    }
    let mut requests = ts.requests.lock().unwrap();
    if let Some(r) = requests
        .iter()
        .find(|r| r["slack_id"] == slack_id && r["status"] == "pending")
    {
        return answer(StatusCode::OK, json!({"request": r}));
    }
    let taken: BTreeSet<String> = ts.db()["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["handle"].as_str().unwrap().to_string())
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
    let admin = body["decided_by"].as_str().unwrap_or_default();
    if !ts.active_admin(admin) {
        return answer(
            StatusCode::FORBIDDEN,
            json!({"error": "admins only", "code": "not_admin"}),
        );
    }
    // From the bot's token a join is `pm` only (U3).
    if body["roles"]
        .as_array()
        .is_some_and(|r| r.iter().any(|r| r != "pm"))
    {
        return answer(
            StatusCode::FORBIDDEN,
            json!({"error": "pm only from Slack", "code": "role_not_from_slack"}),
        );
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
    if !valid_new_handle(&handle) || ts.user(&handle).is_some() {
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
    // The user first (U3), then the personal bot's commit.
    let mut db = ts.db();
    db["users"].as_array_mut().unwrap().push(json!({
        "handle": handle, "display_name": request["name"], "status": "active",
        "roles": ["pm"], "slack_id": request["slack_id"],
    }));
    ts.save(db);
    let commit = ts.commit();
    request["status"] = json!("approved");
    request["decided_by"] = json!(admin);
    request["handle"] = json!(handle);
    request["commit"] = json!(commit);
    answer(StatusCode::OK, json!({"request": request}))
}

/// `POST /internal/members/{handle}/remove`: a deactivation, with U3's refusals.
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
    let admin = body["decided_by"].as_str().unwrap_or_default();
    let refuse = |status: StatusCode, code: &str| {
        answer(
            status,
            json!({"error": format!("refused: {code}"), "code": code}),
        )
    };
    if !ts.active_admin(admin) {
        return refuse(StatusCode::FORBIDDEN, "not_admin");
    }
    if admin == handle {
        return refuse(StatusCode::FORBIDDEN, "self_removal");
    }
    let Some(user) = ts.user(&handle) else {
        return refuse(StatusCode::NOT_FOUND, "not_a_member");
    };
    let roles = user["roles"].as_array().unwrap();
    if roles.contains(&json!("admin")) || roles.contains(&json!("auditor")) {
        return refuse(StatusCode::FORBIDDEN, "protected_not_from_slack");
    }
    if user["status"] == "deactivated" {
        return refuse(StatusCode::CONFLICT, "already_deactivated");
    }
    {
        let mut n = ts.deactivations.lock().unwrap();
        if *n >= 3 {
            return refuse(StatusCode::TOO_MANY_REQUESTS, "deactivation_limit");
        }
        *n += 1;
    }
    let mut db = ts.db();
    for u in db["users"].as_array_mut().unwrap() {
        if u["handle"] == handle.as_str() {
            u["status"] = json!("deactivated");
        }
    }
    ts.save(db);
    answer(
        StatusCode::OK,
        json!({"handle": handle, "deactivated": true, "commit": null,
               "note": format!("`{handle}` is deactivated")}),
    )
}

async fn ts_onboarding(
    State(ts): Ts,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    authed!(headers);
    let user = q.get("user").cloned().unwrap_or_default();
    let Some(member) = ts.user(&user) else {
        return answer(StatusCode::NOT_FOUND, json!({"error": "not a member"}));
    };
    let admin = member["roles"]
        .as_array()
        .unwrap()
        .contains(&json!("admin"));
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
            "member": true, "handle": user, "admin": admin,
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
    if ts.user(&user).is_none() {
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
    let code = if org != "example-org" {
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
        .route("/internal/directory", get(ts_directory))
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
    /// The notification feed; the cursor is an index into it.
    feed: Mutex<Vec<Value>>,
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

async fn rt_notifications(
    State(rt): Rt,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return answer(StatusCode::UNAUTHORIZED, json!({"error": "bad token"}));
    }
    let from: usize = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let items: Vec<Value> = rt.feed.lock().unwrap().iter().skip(from).cloned().collect();
    if items.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    answer(
        StatusCode::OK,
        json!({"cursor": (from + items.len()).to_string(), "items": items}),
    )
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
    bots: Bots,
    /// The mock tool server's user database and the Slack state, across a restart.
    team: tempfile::TempDir,
    /// The clock the Slack service's directory snapshot ages on.
    clock: Arc<FakeClock>,
    directory: Arc<SnapshotClient>,
    stop: Vec<CancellationToken>,
    socket: Option<(CancellationToken, JoinHandle<Ended>)>,
    next: Mutex<u32>,
}

impl Stack {
    async fn new() -> Self {
        Self::build(false, None).await
    }

    /// The new first-time flow on (`PMOS_ONBOARDING_V2=1`).
    async fn new_v2() -> Self {
        Self::build(true, None).await
    }

    /// Stop everything and keep the directory (the user database and Slack state), to start
    /// again.
    async fn stop_keep(mut self) -> tempfile::TempDir {
        if let Some((shutdown, handle)) = self.socket.take() {
            shutdown.cancel();
            assert_eq!(handle.await.unwrap(), Ended::Shutdown);
        }
        for s in &self.stop {
            s.cancel();
        }
        self.team
    }

    async fn build(v2: bool, dir: Option<tempfile::TempDir>) -> Self {
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
                json!({"name": "ben", "real_name": "Ben <Smith>", "profile": {"email": "ben.smith@example.com", "real_name": "Ben <Smith>"}}),
            ),
            (
                "U0DEE".to_string(),
                json!({"name": "dee", "real_name": "Dee", "profile": {"email": "dee@example.com"}}),
            ),
            (
                "U0NOMAIL".to_string(),
                json!({"name": "nomail", "real_name": "No Mail", "profile": {}}),
            ),
        ]);

        let fresh = dir.is_none();
        let team = dir.unwrap_or_else(|| tempfile::tempdir().unwrap());
        let users_file = team.path().join("users.json");
        if fresh {
            std::fs::write(&users_file, users().to_string()).unwrap();
        }
        let tools = Arc::new(MockTools::default());
        *tools.users_file.lock().unwrap() = users_file;
        tools.github.lock().unwrap().insert("ana".into(), "push");
        let (ts_base, stop_ts) = serve(tools_routes(tools.clone())).await;
        let runtime = Arc::new(MockRuntime::default());
        runtime.roster.lock().unwrap().push(bot("docs"));
        let (rt_base, stop_rt) = serve(
            Router::new()
                .route("/v1/bots", get(rt_bots))
                .route("/v1/tasks", post(rt_create_task))
                .route("/v1/notifications", get(rt_notifications))
                .with_state(runtime.clone()),
        )
        .await;

        // The Slack service reads the mock's directory over HTTP, as it reads the tool
        // server's, with the bot token; its age moves only with `clock`.
        let clock = Arc::new(FakeClock::default());
        let directory = Arc::new(SnapshotClient::new(
            Arc::new(HttpFetcher::new(&ts_base, TOOL_BOT_TOKEN)),
            clock.clone(),
        ));
        directory.refresh().await.unwrap();
        let people = People::new(directory.clone());
        let http = reqwest::Client::new();
        let bots = Bots::with_onboarding_v2(
            BotsConfig {
                slack: SlackApi::new(slack_base.clone(), SLACK_BOT_TOKEN, http.clone()),
                runtime: Runtime::new(rt_base, BOTS_TOKEN, http.clone()),
                toolserver: ToolServer::new(ts_base.clone(), TOOL_BOT_TOKEN),
                people: people.clone(),
                content_root: team.path().join("content"),
                default_tz: "UTC".into(),
                github_org: Some("example-org".into()),
                response_url_prefix: format!("{slack_base}/hooks/"),
                state_dir: team.path().join("state"),
                web_url: None,
            },
            v2,
        );
        let socket = Socket::new(
            slack_base.clone(),
            APP_TOKEN,
            Commands {
                toolserver: ToolServer::new(ts_base, TOOL_BOT_TOKEN),
                people,
                http: http.clone(),
                response_url_prefix: format!("{slack_base}/hooks/"),
                request_access: true,
            },
            http,
        )
        .with_bots(bots.clone());
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let handle = tokio::spawn(async move { socket.run(token).await });
        Self {
            slack,
            slack_base,
            live: tx,
            runtime,
            tools,
            bots,
            team,
            clock,
            directory,
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

/// Every element (buttons and accessories) in a message's blocks (none for plain text).
fn elements(body: &Value) -> Vec<Value> {
    body["blocks"]
        .as_array()
        .into_iter()
        .flatten()
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

    // A DM from someone not in the user directory: the note, with Request access.
    stack.dm("U0BEN", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0BEN");
    assert!(text_of(&notes[0]).contains("PM OS is for the product team."));
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
        vec![json!({"slack_id": "U0BEN", "email": "ben.smith@example.com", "name": "Ben <Smith>"})]
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
        assert!(s.contains("ben.smith@example.com"));
        assert!(s.contains("`ben-smith`"));
        // No role choice (S2, U3): the card says what approving grants instead.
        assert!(s.contains("Approving adds them to PM OS as a PM"), "{s}");
        assert!(!s.contains("team.yaml"), "{s}");
        assert!(!s.to_lowercase().contains("make admin"), "{s}");
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

    // Ben is a user now (the tool server created him, and the Slack service polled the
    // directory after the decision): his DMs go to the bots.
    stack.dm("U0BEN", "what ports does Acme Vault need?").await;
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
        "example-org/pm-os-memory-bob"
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
        .submit("U0BOB", &opened[0], "repo", "example-org/pm-os-memory-bob")
        .await;
    assert_eq!(ack["payload"]["response_action"], "errors");
    assert!(error_for(&ack).contains("Connect GitHub first"));
    stack.tools.link("bob", "github");
    for (repo, says) in [
        (
            "other-org/pm-os-memory-bob",
            "isn't in the example-org GitHub org",
        ),
        ("example-org/public-notes", "That repo is public"),
        ("example-org/readonly", "can't push to that repo"),
        ("example-org/missing", "GitHub can't find that repo"),
        (
            "not a repo",
            "Use org/name, like example-org/pm-os-memory-bob.",
        ),
    ] {
        let ack = stack.submit("U0BOB", &opened[0], "repo", repo).await;
        assert!(error_for(&ack).contains(says), "{repo}: {ack}");
    }
    // A tool server too slow for the ack: still acked in time, with a field error.
    *stack.tools.memory_delay.lock().unwrap() = Duration::from_secs(4);
    let ack = stack
        .submit("U0BOB", &opened[0], "repo", "example-org/pm-os-memory-bob")
        .await;
    assert!(error_for(&ack).contains("couldn't check that repo in time"));
    *stack.tools.memory_delay.lock().unwrap() = Duration::ZERO;
    // Accepted: the modal closes and the checklist updates in place.
    let ack = stack
        .submit("U0BOB", &opened[0], "repo", "example-org/pm-os-memory-bob")
        .await;
    assert!(ack.get("payload").is_none(), "{ack}");
    let updates = stack.calls("chat.update", 1).await;
    assert_eq!(updates[0].body["channel"], at.0);
    assert_eq!(updates[0].body["ts"], at.1);
    assert!(text_of(&updates[0])
        .contains("4. :white_check_mark: *Memory repo:* `example-org/pm-os-memory-bob`"));
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
        Some("example-org/pm-os-memory-bob")
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
    assert!(
        text_of(confirm).contains("Remove Bob Jones `bob` (<@U0BOB>) from PM OS?"),
        "{}",
        text_of(confirm)
    );
    assert!(text_of(confirm).contains("This deactivates them"));
    assert!(!text_of(confirm).contains("team.yaml"));
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

    // Remove: the tool server deactivates, and the card says so; nothing is committed.
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
    let done = text_of(&updates[2]);
    assert!(
        done.contains(":white_check_mark: Removed `bob` by Ana: deactivated."),
        "{done}"
    );
    assert!(!done.contains("commit"), "{done}");

    // Bob is deactivated (the directory was polled after the change): his next DM is
    // refused, with no Request access and no task.
    let tasks = stack.runtime.tasks.lock().unwrap().len();
    stack.dm("U0BOB", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(text_of(&notes[1]).contains("Your PM OS access was turned off"));
    assert!(!has_action(&notes[1].body, "pmos_request_access"));
    // Nor may he ask for access again (`409 deactivated` too, should he try).
    stack.click_note("U0BOB", "pmos_request_access").await;
    let replies = stack.responses(2).await;
    assert!(replies[1]["text"]
        .as_str()
        .unwrap()
        .contains("Your PM OS access was turned off"));
    quiet().await;
    assert_eq!(stack.runtime.tasks.lock().unwrap().len(), tasks);
    stack.stop().await;
}

/// `remove` refusals (slice U3), each its own sentence: an Admin or Auditor can't be
/// deactivated from Slack (checked here first, and by the tool server), nor someone already
/// deactivated; the fourth in an hour hits the cap.
#[tokio::test]
async fn remove_refusals_are_clear() {
    let stack = Stack::new().await;
    let st = &stack;
    let note = move |n: usize| async move {
        let posts = st.calls("chat.postMessage", n).await;
        text_of(&posts[n - 1])
    };

    // An admin, and an Auditor: refused before the tool server is asked.
    stack.dm("U0ANA", "remove cal").await;
    let s = note(1).await;
    assert!(s.contains("`cal` is a platform owner or an auditor"), "{s}");
    assert!(s.contains("won't deactivate them from Slack"), "{s}");
    stack.dm("U0ANA", "remove <@U0AUD>").await;
    let s = note(2).await;
    assert!(s.contains("`aud` is a platform owner or an auditor"), "{s}");
    // Already deactivated.
    stack.dm("U0ANA", "remove gil").await;
    let s = note(3).await;
    assert!(s.contains("`gil` is already deactivated"), "{s}");
    assert!(stack.tools.wrote("/remove").is_empty());

    // The tool server's own refusal, should the directory be behind: Ivy was made an
    // admin a moment ago (on the host), so the confirmation offered Remove; the tool server
    // says no.
    stack.dm("U0ANA", "remove ivy").await;
    note(4).await;
    let mut db = stack.tools.db();
    for u in db["users"].as_array_mut().unwrap() {
        if u["handle"] == "ivy" {
            u["roles"] = json!(["pm", "admin"]);
        }
    }
    stack.tools.save(db);
    let card = ("D0ANA", "1700000000.000004");
    stack
        .click("U0ANA", "pmos_member_remove_confirm", "ivy", card)
        .await;
    let updates = stack.calls("chat.update", 2).await;
    let s = text_of(&updates[1]);
    assert!(s.contains("won't deactivate them from Slack"), "{s}");
    assert!(!has_action(&updates[1].body, "pmos_member_remove_confirm"));
    assert_eq!(stack.tools.wrote("/internal/members/ivy/remove").len(), 1);

    // The hourly cap: three go through, the fourth is refused and says who can continue.
    *stack.tools.deactivations.lock().unwrap() = 3;
    stack.dm("U0ANA", "remove bob").await;
    note(5).await;
    let card = ("D0ANA", "1700000000.000005");
    stack
        .click("U0ANA", "pmos_member_remove_confirm", "bob", card)
        .await;
    let updates = stack.calls("chat.update", 4).await;
    let s = text_of(&updates[3]);
    assert!(
        s.contains("PM OS has stopped deactivations for an hour"),
        "{s}"
    );
    assert!(s.contains("pmos-admin users deactivate"), "{s}");
    // Bob is still active: his DM still reaches the bots.
    stack.dm("U0BOB", "hello").await;
    until("Bob's task", || {
        (!stack.runtime.tasks.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    stack.stop().await;
}

/// Admin actions need a directory snapshot at most 30 seconds old; who someone is holds for
/// 5 minutes; past that, PM OS can't check who anyone is. On the service's real path: the
/// directory over HTTP, an outage, and the clock moving.
#[tokio::test]
async fn stale_directory_fails_closed() {
    let stack = Stack::new().await;
    *stack.tools.directory_down.lock().unwrap() = true;
    assert!(stack.directory.refresh().await.is_err());

    // 31 seconds: an approval, `requests` and `remove` are refused here; nothing is sent.
    stack.clock.advance(Duration::from_secs(31));
    let (id, cards) = {
        // Ben's request is filed, but no admin can be told on a stale directory.
        stack.click_note("U0BEN", "pmos_request_access").await;
        let replies = stack.responses(1).await;
        assert!(
            replies[0]["text"]
                .as_str()
                .unwrap()
                .contains("couldn't reach a platform owner"),
            "{}",
            replies[0]
        );
        assert!(stack.slack.calls("conversations.open").is_empty());
        let id = stack.tools.requests.lock().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        (id, ("D0ANA".to_string(), "1700000000.000001".to_string()))
    };
    stack
        .click("U0ANA", "pmos_member_approve", &id, (&cards.0, &cards.1))
        .await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert!(text_of(&notes[0]).contains("PM OS can't check your access right now"));
    stack.dm("U0ANA", "requests").await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(text_of(&notes[1]).contains("can't check your access"));
    stack.dm("U0ANA", "remove bob").await;
    let notes = stack.calls("chat.postEphemeral", 3).await;
    assert!(text_of(&notes[2]).contains("can't check your access"));
    assert!(stack.tools.wrote("/decision").is_empty());
    assert!(stack.tools.wrote("/remove").is_empty());
    // A PM's ask still goes through on the 5-minute identity: the runtime decides again.
    stack.dm("U0BOB", "hello").await;
    until("Bob's task", || {
        (!stack.runtime.tasks.lock().unwrap().is_empty()).then_some(())
    })
    .await;

    // Exactly 5 minutes: still known. A moment later: PM OS can't check who anyone is.
    stack
        .clock
        .advance(Duration::from_secs(5 * 60) - Duration::from_secs(31));
    stack.dm("U0BOB", "again").await;
    until("Bob's second task", || {
        (stack.runtime.tasks.lock().unwrap().len() == 2).then_some(())
    })
    .await;
    stack.clock.advance(Duration::from_millis(1));
    stack.dm("U0BOB", "and again").await;
    let notes = stack.calls("chat.postEphemeral", 4).await;
    assert!(text_of(&notes[3]).contains("PM OS can't check who you are right now"));
    // A stranger is not offered Request access while PM OS can't check.
    stack.dm("U0ZED", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 5).await;
    assert!(text_of(&notes[4]).contains("can't check who you are"));
    assert!(!has_action(&notes[4].body, "pmos_request_access"));
    quiet().await;
    assert_eq!(stack.runtime.tasks.lock().unwrap().len(), 2);

    // The directory answers again: everything is back.
    *stack.tools.directory_down.lock().unwrap() = false;
    stack.directory.refresh().await.unwrap();
    stack.dm("U0BOB", "back").await;
    until("Bob's third task", || {
        (stack.runtime.tasks.lock().unwrap().len() == 3).then_some(())
    })
    .await;
    stack.stop().await;
}

/// An Auditor-only person isn't a PM in Slack, and a deactivated person is refused: neither
/// is offered Request access, and neither starts a task.
#[tokio::test]
async fn an_auditor_is_not_a_pm_and_a_deactivated_person_is_refused() {
    let stack = Stack::new().await;
    stack.dm("U0AUD", "what ports does Acme Vault need?").await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert!(text_of(&notes[0]).contains("Your PM OS role doesn't include using PM OS in Slack"));
    assert!(!has_action(&notes[0].body, "pmos_request_access"));
    stack.dm("U0GIL", "hello").await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(text_of(&notes[1]).contains("Your PM OS access was turned off"));
    // `/docs` too.
    for (user, says) in [
        ("U0AUD", "doesn't include using PM OS in Slack"),
        ("U0GIL", "access was turned off"),
    ] {
        let ack = stack
            .send(
                "slash_commands",
                json!({"command": "/docs", "text": "saml", "user_id": user, "channel_id": "C1",
                       "response_url": format!("{}/hooks/bots", stack.slack_base)}),
            )
            .await;
        assert!(
            ack["payload"]["text"].as_str().unwrap().contains(says),
            "{ack}"
        );
    }
    // Neither can decide a join, even holding a role row that once said admin.
    let (id, cards) = stack.ben_requests().await;
    stack
        .click(
            "U0AUD",
            "pmos_member_approve",
            &id,
            (&cards[0].0, &cards[0].1),
        )
        .await;
    let notes = stack.calls("chat.postEphemeral", 3).await;
    assert!(text_of(&notes[2]).contains("Only platform owners can decide this."));
    assert!(stack.tools.wrote("/decision").is_empty());
    quiet().await;
    assert!(stack.runtime.tasks.lock().unwrap().is_empty());
    stack.stop().await;
}

/// A display name from Slack renders inert wherever the service shows it: the `remove`
/// confirmation (the directory's name) and an access request's card (Slack's `users.info`).
#[tokio::test]
async fn a_hostile_display_name_renders_inert() {
    let stack = Stack::new().await;
    stack.dm("U0ANA", "remove ivy").await;
    let posts = stack.calls("chat.postMessage", 1).await;
    let s = text_of(&posts[0]);
    assert!(!s.contains("<!channel>"), "{s}");
    assert!(!s.contains("<https://evil"), "{s}");
    assert!(!s.contains("*Ivy*"), "{s}");
    assert!(s.contains("&lt;!channel&gt;"), "{s}");
    assert!(s.contains("`ivy` (<@U0IVY>)"), "{s}");

    stack.slack.users.lock().unwrap().insert(
        "U0EVE".into(),
        json!({"name": "eve", "real_name": "<!here> <@U0ANA> _Eve_ evil.example",
               "profile": {"email": "eve@example.com"}}),
    );
    let before = stack.slack.calls("chat.postMessage").len();
    stack.click_note("U0EVE", "pmos_request_access").await;
    let posts = stack.calls("chat.postMessage", before + 2).await;
    for card in &posts[before..] {
        let s = text_of(card);
        for live in ["<!here>", "<@U0ANA>", "_Eve_", "evil.example"] {
            assert!(!s.contains(live), "{live} in {s}");
        }
        assert!(s.contains("&lt;!here&gt;"), "{s}");
        // The card's own mention of the requester is still one.
        assert!(s.contains("(<@U0EVE>)"), "{s}");
    }
    stack.stop().await;
}

/// A hostile `team.yaml` in the content root, listing `mallory` as an admin with Ana's Slack
/// id and another for Mal's, changes nothing: the directory decides. Then none at all.
#[tokio::test]
async fn a_hostile_or_absent_team_yaml_changes_nothing() {
    let stack = Stack::new().await;
    let content = stack.team.path().join("content");
    std::fs::create_dir_all(&content).unwrap();
    std::fs::write(
        content.join("team.yaml"),
        "members:\n  - {handle: mallory, slack-id: U0ANA, email: ana@example.com, roles: [pm, admin]}\n  - {handle: mal, slack-id: U0MAL, roles: [pm, admin]}\n  - {handle: bob, slack-id: U0BOB, roles: [pm, admin]}\n",
    )
    .unwrap();
    // Ana is still Ana: her task is hers, not mallory's.
    stack.dm("U0ANA", "hello").await;
    let tasks = until("Ana's task", || {
        let t = stack.runtime.tasks.lock().unwrap().clone();
        (!t.is_empty()).then_some(t)
    })
    .await;
    assert_eq!(tasks[0]["requested_by"], "ana");
    // Mal, in the file only, is a stranger; Bob, an admin in the file only, is not an admin.
    stack.dm("U0MAL", "requests").await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert!(has_action(&notes[0].body, "pmos_request_access"));
    stack.dm("U0BOB", "remove cal").await;
    until("Bob's message to his bot", || {
        (stack.runtime.tasks.lock().unwrap().len() == 2).then_some(())
    })
    .await;
    assert!(stack.tools.wrote("/remove").is_empty());
    let (id, cards) = stack.ben_requests().await;
    let opened: Vec<Value> = stack
        .slack
        .calls("conversations.open")
        .iter()
        .map(|c| c.body["users"].clone())
        .collect();
    assert_eq!(
        opened,
        vec![json!("U0ANA"), json!("U0CAL")],
        "admins from the directory"
    );
    stack
        .click(
            "U0BOB",
            "pmos_member_approve",
            &id,
            (&cards[0].0, &cards[0].1),
        )
        .await;
    let notes = stack.calls("chat.postEphemeral", 2).await;
    assert!(text_of(&notes[1]).contains("Only platform owners can decide this."));

    // No team.yaml at all: the same.
    std::fs::remove_file(content.join("team.yaml")).unwrap();
    stack.dm("U0ANA", "requests").await;
    let posts = stack.calls("chat.postMessage", 3).await;
    assert_eq!(posts[2].body["channel"], "D0ANA");
    assert_eq!(button_value(&posts[2].body, "pmos_member_approve"), id);
    stack.stop().await;
}

// ---- The new first-time flow (`PMOS_ONBOARDING_V2=1`) ----

/// Ben asks, Ana approves; returns once the approval is done (the DMs to Ben may follow).
async fn approve_ben(stack: &Stack) {
    let (id, cards) = stack.ben_requests().await;
    let (channel, ts) = &cards[0];
    stack
        .click("U0ANA", "pmos_member_approve", &id, (channel, ts))
        .await;
    stack.calls("chat.update", 2).await;
}

/// What the bot has posted to Ben's DM so far.
fn to_ben(stack: &Stack) -> Vec<Call> {
    stack
        .slack
        .calls("chat.postMessage")
        .into_iter()
        .filter(|c| c.body["channel"] == "D0BEN")
        .collect()
}

async fn ben_posts(stack: &Stack, n: usize) -> Vec<Call> {
    until(&format!("{n} posts to Ben"), || {
        let posts = to_ben(stack);
        (posts.len() >= n).then_some(posts)
    })
    .await
}

/// Nothing the bot posted went to a channel: every post is in someone's DM.
fn assert_only_dms(stack: &Stack) {
    for c in stack.slack.calls("chat.postMessage") {
        let channel = c.body["channel"].as_str().unwrap();
        assert!(channel.starts_with('D'), "posted to {channel}");
    }
}

fn app_home_opened(user: &str, n: u32) -> Value {
    json!({"type": "event_callback", "event_id": format!("EvHome{n}"), "event": {
        "type": "app_home_opened", "tab": "home", "user": user, "event_ts": format!("9{n}.1"),
    }})
}

async fn quiet() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Run the notifications loop on this stack's state directory.
fn run_notifier(stack: &Stack) -> (CancellationToken, JoinHandle<()>) {
    let mut notifier =
        pmos_slack::notify::Notifier::new(stack.bots.clone(), stack.team.path().join("state"));
    notifier.wait = Duration::from_millis(100);
    let stop = CancellationToken::new();
    (stop.clone(), tokio::spawn(notifier.run(stop)))
}

fn done(id: &str, thread: &str) -> Value {
    json!({
        "id": format!("n-{id}"), "task_id": format!("tsk_{id}"), "kind": "done",
        "text": "Here is your answer.", "bot": "docs", "requested_by": "ben-smith",
        "slack": {"channel": "D0BEN", "thread_ts": thread},
    })
}

#[tokio::test]
async fn flag_off_the_welcome_and_checklist_are_unchanged() {
    let stack = Stack::new().await;
    approve_ben(&stack).await;
    let posts = ben_posts(&stack, 2).await;
    assert!(text_of(&posts[0]).contains("Welcome to PM OS, Ben!"));
    assert!(has_action(&posts[1].body, "pmos_setup_memory"));
    assert_eq!(link_urls(&posts[1].body).len(), 3);

    // Linked, then a DM: no offer. A finished task: no closing line.
    stack.tools.link("ben-smith", "atlassian");
    stack.dm("U0BEN", "hi").await;
    stack.runtime.feed.lock().unwrap().push(done("a", "200.9"));
    let (stop, run) = run_notifier(&stack);
    ben_posts(&stack, 3).await;
    quiet().await;
    assert_eq!(to_ben(&stack).len(), 3, "the answer only");
    assert!(!stack.team.path().join("state/onboarding.json").exists());
    stop.cancel();
    run.await.unwrap();
    stack.stop().await;
}

#[tokio::test]
async fn hello_is_two_lines_and_one_button_and_no_checklist() {
    let stack = Stack::new_v2().await;
    approve_ben(&stack).await;
    let posts = ben_posts(&stack, 1).await;
    quiet().await;
    assert_eq!(to_ben(&stack).len(), 1, "no checklist after the welcome");
    let hello = &posts[0].body;
    assert_eq!(
        hello["text"],
        "Hi Ben, I'm PM OS. I read Jira so you don't have to.\nConnect it and I'll prove it."
    );
    let buttons = elements(hello);
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0]["text"]["text"], "Connect Jira and Confluence");
    assert_eq!(
        buttons[0]["url"],
        "https://pmos.example.com/oauth/atlassian/start?state=s-ben-smith"
    );
    assert!(!text_of(&posts[0]).contains("Slack"));
    assert!(!text_of(&posts[0]).contains("memory"));
    assert_only_dms(&stack);
    stack.stop().await;
}

#[tokio::test]
async fn the_offer_comes_after_linking_once_and_each_button_starts_a_task() {
    let stack = Stack::new_v2().await;
    stack
        .runtime
        .roster
        .lock()
        .unwrap()
        .push(bot("pm-ben-smith"));
    approve_ben(&stack).await;
    ben_posts(&stack, 1).await;

    // Not linked yet: a DM and a Home open bring no offer; the DM still becomes a task.
    stack.dm("U0BEN", "what ports does Acme Vault need?").await;
    until("the task", || {
        (!stack.runtime.tasks.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    stack.send("events_api", app_home_opened("U0BEN", 1)).await;
    quiet().await;
    assert_eq!(to_ben(&stack).len(), 1);

    // Linked: the next DM brings the offer, once.
    stack.tools.link("ben-smith", "atlassian");
    stack.dm("U0BEN", "hello again").await;
    let posts = ben_posts(&stack, 2).await;
    let offer = &posts[1];
    assert_eq!(
        offer.body["text"],
        "Found your account. Want the short version of your week, or shall I start on something?"
    );
    let labels: Vec<String> = elements(&offer.body)
        .iter()
        .map(|e| e["text"]["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        labels,
        [
            "Summarize my week",
            "Draft a status update",
            "What's blocked?"
        ]
    );
    stack.dm("U0BEN", "and again").await;
    stack.send("events_api", app_home_opened("U0BEN", 2)).await;
    quiet().await;
    assert_eq!(to_ben(&stack).len(), 2, "the offer is shown once");

    // Each button creates a task for Ben's own bot, in the thread under the offer.
    let ts = posted_ts(&stack.slack, offer);
    let before = stack.runtime.tasks.lock().unwrap().len();
    for (n, (id, expect)) in [
        ("week", "Summarize my week"),
        ("status", "Draft a status update"),
        ("blocked", "What is blocked"),
    ]
    .into_iter()
    .enumerate()
    {
        stack
            .click(
                "U0BEN",
                &format!("pmos_onboard_start_{id}"),
                id,
                ("D0BEN", &ts),
            )
            .await;
        let tasks = until("the starter task", || {
            let t = stack.runtime.tasks.lock().unwrap().clone();
            (t.len() > before + n).then_some(t)
        })
        .await;
        let task = &tasks[before + n];
        assert_eq!(task["bot"], "pm-ben-smith");
        assert_eq!(task["requested_by"], "ben-smith");
        assert_eq!(task["source"], "slack");
        assert_eq!(task["slack"], json!({"channel": "D0BEN", "thread_ts": ts}));
        assert!(task["text"].as_str().unwrap().contains(expect), "{task}");
    }
    let updates = stack.calls("chat.update", 3).await;
    let last = updates.last().unwrap();
    assert!(text_of(last).contains("On it:"));
    assert!(elements(&last.body).is_empty());

    // Someone outside the user directory tapping it gets the usual refusal and no task.
    let tasks = stack.runtime.tasks.lock().unwrap().len();
    stack
        .click("U0ZED", "pmos_onboard_start_week", "week", ("D0ZED", "1.0"))
        .await;
    stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(stack.runtime.tasks.lock().unwrap().len(), tasks);
    assert_only_dms(&stack);
    stack.stop().await;
}

#[tokio::test]
async fn each_step_is_shown_once_across_restarts() {
    let stack = Stack::new_v2().await;
    approve_ben(&stack).await;
    ben_posts(&stack, 1).await;
    stack.tools.link("ben-smith", "atlassian");
    // The offer on a Home open this time.
    stack.send("events_api", app_home_opened("U0BEN", 1)).await;
    ben_posts(&stack, 2).await;
    let dir = stack.stop_keep().await;
    let saved = std::fs::read_to_string(dir.path().join("state/onboarding.json")).unwrap();
    assert!(saved.contains("\"hello\": true") && saved.contains("\"offer\": true"));

    let stack = Stack::build(true, Some(dir)).await;
    stack.tools.link("ben-smith", "atlassian");
    stack.dm("U0BEN", "hi").await;
    stack.send("events_api", app_home_opened("U0BEN", 2)).await;
    // The closing line comes with the first finished task, and only once.
    stack.runtime.feed.lock().unwrap().push(done("a", "200.0"));
    stack.runtime.feed.lock().unwrap().push(done("b", "200.1"));
    let (stop, run) = run_notifier(&stack);
    ben_posts(&stack, 3).await;
    quiet().await;
    let posts = to_ben(&stack);
    assert_eq!(posts.len(), 3, "two answers and one closing line");
    let closing: Vec<&Call> = posts
        .iter()
        .filter(|c| text_of(c).contains("`nudges off` stops my occasional nudges"))
        .collect();
    assert_eq!(closing.len(), 1);
    assert_eq!(closing[0].body["thread_ts"], "200.0");
    assert!(text_of(closing[0]).contains("Skills section of my Home tab"));
    stop.cancel();
    run.await.unwrap();
    let dir = stack.stop_keep().await;

    // And after another restart: still nothing new.
    let stack = Stack::build(true, Some(dir)).await;
    stack.tools.link("ben-smith", "atlassian");
    stack.dm("U0BEN", "hi").await;
    stack.send("events_api", app_home_opened("U0BEN", 3)).await;
    quiet().await;
    assert!(to_ben(&stack).is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn setup_still_works_and_a_stranger_still_gets_the_access_note() {
    let stack = Stack::new_v2().await;
    approve_ben(&stack).await;
    ben_posts(&stack, 1).await;
    stack.dm("U0BEN", "setup").await;
    let posts = ben_posts(&stack, 2).await;
    assert_eq!(
        link_urls(&posts[1].body),
        ["atlassian", "slack", "github"]
            .map(|p| format!("https://pmos.example.com/oauth/{p}/start?state=s-ben-smith"))
    );
    assert!(has_action(&posts[1].body, "pmos_setup_memory"));
    assert!(has_action(&posts[1].body, "pmos_setup_refresh"));

    stack.dm("U0ZED", "hi").await;
    let notes = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(notes[0].body["user"], "U0ZED");
    assert!(text_of(&notes[0]).contains("PM OS is for the product team."));
    assert!(has_action(&notes[0].body, "pmos_request_access"));
    assert_only_dms(&stack);
    stack.stop().await;
}
