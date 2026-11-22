//! A mocked bot runtime (axum) that plays scripted task outcomes, and helpers to write
//! categories into a temporary evals directory.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

pub const TOKEN: &str = "evals-test-token-0123456789abcdef0123";
pub const CONSOLE_TOKEN: &str = "console-test-token-0123456789abcdef01";

/// How a task asked with a given text plays out.
#[derive(Clone, Debug)]
pub struct Script {
    pub queue_ms: i64,
    pub work_ms: i64,
    /// The final status: done, failed, needs_approval or cancelled.
    pub status: &'static str,
    pub result: Option<String>,
    pub error: Option<String>,
    pub approvals: usize,
    pub attempts: u32,
    /// Whether the worker sends events once it has the task.
    pub events: bool,
    /// Never claimed: stays queued.
    pub never_claimed: bool,
    /// Stays working forever (with or without events).
    pub hangs: bool,
    pub trace: Vec<Value>,
    /// The task's drafts, as `(tool, args)`; their ids are the approval ids.
    pub drafts: Vec<(&'static str, Value)>,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            queue_ms: 20,
            work_ms: 30,
            status: "done",
            result: Some("An answer.".into()),
            error: None,
            approvals: 0,
            attempts: 1,
            events: true,
            never_claimed: false,
            hangs: false,
            trace: Vec::new(),
            drafts: Vec::new(),
        }
    }
}

impl Script {
    /// A task that ends `needs_approval` with these drafts.
    pub fn drafting(drafts: Vec<(&'static str, Value)>) -> Self {
        Script {
            status: "needs_approval",
            result: Some("Drafted; waiting for your approval.".into()),
            approvals: drafts.len(),
            drafts,
            ..Script::default()
        }
    }
}

pub(crate) struct Stored {
    created: DateTime<Utc>,
    start: DateTime<Utc>,
    finish: DateTime<Utc>,
    script: Script,
    body: Value,
}

#[derive(Default)]
pub struct Mock {
    /// Per ask, the scripts its creations play in turn (the nth creation plays the nth,
    /// wrapping around).
    pub scripts: Mutex<HashMap<String, Vec<Script>>>,
    pub(crate) plays: Mutex<HashMap<String, usize>>,
    /// Tasks run one at a time, like a bot with one sandbox.
    pub serial: bool,
    /// `GET /v1/tasks/{id}/drafts` answers 404, as a runtime without it would.
    pub no_drafts: bool,
    /// `POST /v1/tasks` with a `conversation` answers 403, as the runtime does for client
    /// `evals` until it may name one.
    pub reject_conversation: bool,
    pub(crate) tasks: Mutex<HashMap<String, Stored>>,
    pub(crate) order: Mutex<Vec<String>>,
    pub(crate) last_finish: Mutex<Option<DateTime<Utc>>>,
    pub created: Mutex<Vec<Value>>,
    pub drafts_read: Mutex<Vec<String>>,
    /// Plays the tool server's one-pending-draft rule (`already_drafted`): once an ask has
    /// drafted, later creations of it draft nothing (done, no approvals) until [`Mock::fresh`]
    /// replaces the "tool server".
    pub one_draft_until_fresh: bool,
    pub(crate) drafted: Mutex<std::collections::HashSet<String>>,
    /// How many times [`Mock::fresh`] ran.
    pub fresh_calls: Mutex<usize>,
}

impl Mock {
    /// A fresh "tool server": forgets every draft.
    pub fn fresh(&self) {
        self.drafted.lock().unwrap().clear();
        *self.fresh_calls.lock().unwrap() += 1;
    }

    /// Each created task's `(created, finish)`, in creation order.
    pub fn spans(&self) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
        let order = self.order.lock().unwrap();
        let tasks = self.tasks.lock().unwrap();
        order
            .iter()
            .map(|id| (tasks[id].created, tasks[id].finish))
            .collect()
    }

    pub fn script(&self, ask: &str, s: Script) {
        self.script_seq(ask, vec![s]);
    }

    /// Scripts for an ask's successive creations, such as its repeats.
    pub fn script_seq(&self, ask: &str, s: Vec<Script>) {
        self.scripts.lock().unwrap().insert(ask.to_string(), s);
        self.plays.lock().unwrap().remove(ask);
    }

    pub fn created_bodies(&self) -> Vec<Value> {
        self.created.lock().unwrap().clone()
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn task_json(id: &str, st: &Stored, now: DateTime<Utc>) -> Value {
    let s = &st.script;
    let (status, updated) = if s.never_claimed || now < st.start {
        ("queued", st.created)
    } else if s.hangs || now < st.finish {
        ("working", st.start)
    } else {
        (s.status, st.finish)
    };
    let settled = !matches!(status, "queued" | "working");
    json!({
        "id": id,
        "bot": st.body["bot"],
        "requested_by": st.body["requested_by"],
        "source": st.body["source"],
        "priority": "personal",
        "text": st.body["text"],
        "status": status,
        "checklist": [],
        "result": if settled { json!(s.result) } else { Value::Null },
        "error": if settled { json!(s.error) } else { Value::Null },
        "approvals": if settled { (0..s.approvals).map(|i| format!("apr_{i}")).collect::<Vec<_>>() } else { Vec::new() },
        "slack": null,
        "routine_id": null,
        "attempts": s.attempts,
        "created_at": st.created,
        "updated_at": updated,
    })
}

async fn create(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if bearer(&headers).as_deref() != Some(TOKEN) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "bad token"})),
        )
            .into_response();
    }
    if body["source"] != "eval" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "evals may only create source eval"})),
        )
            .into_response();
    }
    let bot = body["bot"].as_str().unwrap_or_default();
    if bot != "docs" && !bot.starts_with("pm-") {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such bot"}))).into_response();
    }
    if body["requested_by"] == "stranger" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "stranger may not use docs"})),
        )
            .into_response();
    }
    if m.reject_conversation && body.get("conversation").is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "only the web and Slack clients name a task's conversation"})),
        )
            .into_response();
    }
    m.created.lock().unwrap().push(body.clone());
    let ask = body["text"].as_str().unwrap_or_default().to_string();
    let script = {
        let scripts = m.scripts.lock().unwrap();
        let mut plays = m.plays.lock().unwrap();
        let n = plays.entry(ask.clone()).or_default();
        let s = scripts
            .get(&ask)
            .filter(|v| !v.is_empty())
            .map(|v| v[*n % v.len()].clone())
            .unwrap_or_default();
        *n += 1;
        s
    };
    let script = if m.one_draft_until_fresh && !script.drafts.is_empty() {
        if m.drafted.lock().unwrap().insert(ask.clone()) {
            script
        } else {
            Script {
                result: Some("A draft for that message is already waiting.".into()),
                ..Script::default()
            }
        }
    } else {
        script
    };
    let created = Utc::now();
    let mut start = created + Duration::milliseconds(script.queue_ms);
    if m.serial {
        let mut last = m.last_finish.lock().unwrap();
        if let Some(prev) = *last {
            start = start.max(prev);
        }
        *last = Some(start + Duration::milliseconds(script.work_ms));
    }
    let finish = start + Duration::milliseconds(script.work_ms);
    let id = format!("tsk_{:032x}", m.order.lock().unwrap().len() + 1);
    let stored = Stored {
        created,
        start,
        finish,
        script,
        body,
    };
    let out = task_json(&id, &stored, created);
    m.order.lock().unwrap().push(id.clone());
    m.tasks.lock().unwrap().insert(id, stored);
    (StatusCode::CREATED, Json(out)).into_response()
}

fn authed(headers: &HeaderMap) -> bool {
    matches!(
        bearer(headers).as_deref(),
        Some(TOKEN) | Some(CONSOLE_TOKEN)
    )
}

async fn get_task(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let tasks = m.tasks.lock().unwrap();
    match tasks.get(&id) {
        Some(st) => Json(task_json(&id, st, Utc::now())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn events(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let tasks = m.tasks.lock().unwrap();
    let Some(st) = tasks.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let now = Utc::now();
    let s = &st.script;
    let mut out = Vec::new();
    if s.events && !s.never_claimed && now >= st.start {
        out.push(json!({"at": st.start, "kind": "status", "text": "Working on it"}));
        out.push(json!({"at": st.start + Duration::milliseconds(1), "kind": "milestone", "text": "Searching the support docs"}));
    }
    Json(out).into_response()
}

/// Only the console may read traces (§3), as in the runtime.
async fn trace(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if bearer(&headers).as_deref() != Some(CONSOLE_TOKEN) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "console only"})),
        )
            .into_response();
    }
    let tasks = m.tasks.lock().unwrap();
    match tasks.get(&id) {
        Some(st) => Json(
            st.script
                .trace
                .iter()
                .map(|e| json!({"at": st.start, "kind": e["kind"], "body": e["body"]}))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /v1/tasks/{id}/drafts` as the runtime serves it to clients `console` and `evals`:
/// `{drafts: [{id, tool, args, status}]}` once the task has settled.
async fn drafts(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if m.no_drafts {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }
    m.drafts_read.lock().unwrap().push(id.clone());
    let tasks = m.tasks.lock().unwrap();
    let Some(st) = tasks.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let settled = !st.script.never_claimed && !st.script.hangs && Utc::now() >= st.finish;
    let out: Vec<Value> = if settled {
        st.script
            .drafts
            .iter()
            .enumerate()
            .map(|(i, (tool, args))| {
                json!({"id": format!("apr_{i}"), "tool": tool, "args": args, "status": "pending"})
            })
            .collect()
    } else {
        Vec::new()
    };
    Json(json!({"drafts": out})).into_response()
}

/// Start the mock on a free port. Returns its base URL.
pub async fn spawn(mock: Arc<Mock>) -> String {
    let app = Router::new()
        .route("/v1/tasks", post(create))
        .route("/v1/tasks/{id}", get(get_task))
        .route("/v1/tasks/{id}/events", get(events))
        .route("/v1/tasks/{id}/trace", get(trace))
        .route("/v1/tasks/{id}/drafts", get(drafts))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

pub const GATEWAY_TOKEN: &str = "evals-gateway-token-0123456789abcdef01";

/// A mocked gateway: `POST /v1/chat/completions` for client `evals`, answering each call
/// with `reply(user message)`.
pub struct Gateway {
    pub reply: Box<dyn Fn(&str) -> String + Send + Sync>,
    pub calls: Mutex<Vec<Value>>,
}

async fn completions(
    State(g): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if bearer(&headers).as_deref() != Some(GATEWAY_TOKEN) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "bad token"}})),
        )
            .into_response();
    }
    if !body["model"]
        .as_str()
        .unwrap_or_default()
        .starts_with("pmos/")
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "model must be pmos/<role>"}})),
        )
            .into_response();
    }
    let user = body["messages"][1]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    g.calls.lock().unwrap().push(body);
    let content = (g.reply)(&user);
    Json(json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": content}}]}))
        .into_response()
}

/// The gateway's `GET /v1/pmos/roles`: two local roles.
async fn roles(headers: HeaderMap) -> Response {
    if bearer(&headers).as_deref() != Some(GATEWAY_TOKEN) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"stub": false, "cloud_test": false, "workarounds": true, "roles": [
        {"role": "default", "alias": "pmos/default", "model": "qwen-prod", "upstream": "local"},
        {"role": "extraction", "alias": "pmos/extraction", "model": "nemo-prod", "upstream": "local"},
    ]}))
    .into_response()
}

/// Start the mock gateway. Returns its base URL, without `/v1`.
pub async fn spawn_gateway(g: Arc<Gateway>) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .route("/v1/pmos/roles", get(roles))
        .with_state(g);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// The checked-in taxonomy, which `check` needs in an evals root.
pub fn write_taxonomy(root: &Path) {
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/taxonomy.yaml");
    std::fs::copy(real, root.join("taxonomy.yaml")).unwrap();
}

/// A trace entry as the worker posts it.
pub fn entry(kind: &str, body: Value) -> Value {
    json!({"kind": kind, "body": body})
}

pub const CONFIG: &str = "\
runner:
  bots_url: http://127.0.0.1:1
  requested_by: ana
  poll_ms: 10
  task_timeout_seconds: 5
  concurrency: 4
latency_targets:
  queue_p50: {seconds: 30, source: test default}
  queue_p90: {seconds: 120, source: test plan}
  total_p50: {seconds: 60, source: test default}
  total_p90: {seconds: 180, source: test default}
decline_phrases: [\"don't answer\", \"couldn't find\"]
benchmark:
  production_gateway: prod-gateway.yaml
";

/// The production gateway config [`write_category`] writes beside `config.yaml`: the models
/// the mock gateway reports.
pub const PROD_GATEWAY: &str = "\
upstreams: {q: 'http://vllm:8000/v1'}
roles:
  default: {upstream: q, model: qwen-prod}
  extraction: {upstream: q, model: nemo-prod}
";

/// Write `config.yaml` and a category with the given task files (`(id, yaml)`) under `root`.
pub fn write_category(root: &Path, name: &str, bot: &str, bar: f64, tasks: &[(&str, String)]) {
    std::fs::write(root.join("config.yaml"), CONFIG).unwrap();
    std::fs::write(root.join("prod-gateway.yaml"), PROD_GATEWAY).unwrap();
    let dir = root.join("categories").join(name);
    std::fs::create_dir_all(dir.join("tasks")).unwrap();
    std::fs::write(
        dir.join("category.yaml"),
        format!("name: {name}\nbot: {bot}\npass_bar: {bar}\ndescription: A test category.\n"),
    )
    .unwrap();
    for (id, yaml) in tasks {
        std::fs::write(dir.join("tasks").join(format!("{id}.yaml")), yaml).unwrap();
    }
}
