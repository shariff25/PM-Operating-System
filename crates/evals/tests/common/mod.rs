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
    pub scripts: Mutex<HashMap<String, Script>>,
    /// Tasks run one at a time, like a bot with one sandbox.
    pub serial: bool,
    pub(crate) tasks: Mutex<HashMap<String, Stored>>,
    pub(crate) order: Mutex<Vec<String>>,
    pub(crate) last_finish: Mutex<Option<DateTime<Utc>>>,
    pub created: Mutex<Vec<Value>>,
}

impl Mock {
    pub fn script(&self, ask: &str, s: Script) {
        self.scripts.lock().unwrap().insert(ask.to_string(), s);
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
    if body["bot"] != "docs" {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such bot"}))).into_response();
    }
    if body["requested_by"] == "stranger" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "stranger may not use docs"})),
        )
            .into_response();
    }
    m.created.lock().unwrap().push(body.clone());
    let ask = body["text"].as_str().unwrap_or_default().to_string();
    let script = m
        .scripts
        .lock()
        .unwrap()
        .get(&ask)
        .cloned()
        .unwrap_or_default();
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

/// Start the mock on a free port. Returns its base URL.
pub async fn spawn(mock: Arc<Mock>) -> String {
    let app = Router::new()
        .route("/v1/tasks", post(create))
        .route("/v1/tasks/{id}", get(get_task))
        .route("/v1/tasks/{id}/events", get(events))
        .route("/v1/tasks/{id}/trace", get(trace))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
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
";

/// Write `config.yaml` and a category with the given task files (`(id, yaml)`) under `root`.
pub fn write_category(root: &Path, name: &str, bot: &str, bar: f64, tasks: &[(&str, String)]) {
    std::fs::write(root.join("config.yaml"), CONFIG).unwrap();
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
