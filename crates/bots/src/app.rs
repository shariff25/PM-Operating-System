//! The HTTP API (spec §3). Every `/v1` call needs a client token from `PMOS_BOTS_TOKENS`;
//! the client's name decides what it may do:
//!
//! | Client | May |
//! | --- | --- |
//! | `slack` | create tasks as `X-Pmos-User`; read that PM's tasks and routines; read and set that PM's settings; read the shared notification feed and the bot list (no PM needed) |
//! | `worker` | claim, heartbeat, events, trace, complete, fail |
//! | `console` | read everything, settings included; pause, resume and delete routines; cancel tasks |
//! | `evals` | create tasks with `source: "eval"` (and a task's own `level`) as a handle in `PMOS_EVALS_AS` only, with no Slack target; read them and their drafts |
//!
//! A task's drafts (`GET /v1/tasks/{id}/drafts`) are for the console and evals only: the
//! runtime reads them from the tool server with its own service token, so the evals client
//! never holds one.
//!
//! The `web` and `console` clients may also prove who they act for with a signed
//! `X-Pmos-Assertion` (`crate::assertion`, `PMOS_ASSERTION_MODE`): with `verify` a bad one is
//! refused, and with `require` a call that names a person without a valid one is refused, both
//! with `401 {"code":"bad_assertion"}`.
//!
//! A task or routine another PM owns answers `404` to the Slack client, the same as one that
//! doesn't exist. Request bodies that don't parse are `400`; `422` is kept for "the routine
//! request is unclear", with the question to ask.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, OriginalUri, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use pmos_contract::model::valid_handle;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::assertion::{Assertions, Mode, Refusal};
use crate::config::{Client, ClientKind};
use crate::model::{
    self, ChecklistItem, Draft, Level, MemberSettings, Schedule, SlackRef, Source, Task,
    TaskStatus, TraceKind,
};
use crate::runtime::{Error, NewTask, RoutineChange, Runtime, WorkerEvent};
use crate::store::TaskFilter;

/// Largest request body: a batch of trace entries with tool results in it.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

pub const USER_HEADER: &str = "x-pmos-user";

#[derive(Clone)]
struct AppState {
    rt: Arc<Runtime>,
    clients: Arc<[Client]>,
    assertions: Arc<Assertions>,
    /// `PMOS_EVALS_AS`: the handles the evals client may request tasks as.
    evals_as: Arc<[String]>,
}

/// Who is calling: the client, and for Slack the PM it acts for.
#[derive(Debug, Clone)]
struct Caller {
    kind: ClientKind,
    user: Option<String>,
    /// A per-bot worker's bot (`worker-<bot>`).
    bot: Option<String>,
}

impl Caller {
    fn user(&self) -> &str {
        self.user.as_deref().unwrap_or_default()
    }
}

/// The API with assertions off (`PMOS_ASSERTION_MODE=off`). `evals_as` is `PMOS_EVALS_AS`
/// ([`crate::config::parse_evals_as`]): the only handles the `evals` client may create tasks
/// for.
pub fn router(rt: Arc<Runtime>, clients: Vec<Client>, evals_as: Vec<String>) -> Router {
    router_with(rt, clients, evals_as, Arc::new(Assertions::off()))
}

/// [`router`], checking user assertions from the `web` and `console` clients as `assertions`
/// says.
pub fn router_with(
    rt: Arc<Runtime>,
    clients: Vec<Client>,
    evals_as: Vec<String>,
    assertions: Arc<Assertions>,
) -> Router {
    let state = AppState {
        rt,
        clients: clients.into(),
        assertions,
        evals_as: evals_as.into(),
    };
    let v1 = Router::new()
        .route("/bots", get(list_bots))
        .route("/bots/{name}", get(get_bot))
        .route("/tasks", post(create_task).get(list_tasks))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/cancel", post(cancel_task))
        .route("/tasks/{id}/events", get(task_events))
        .route("/tasks/{id}/trace", get(task_trace))
        .route("/tasks/{id}/drafts", get(task_drafts))
        .route("/worker/claim", post(claim))
        .route("/worker/tasks/{id}/heartbeat", post(heartbeat))
        .route("/worker/tasks/{id}/events", post(worker_events))
        .route("/worker/tasks/{id}/trace", post(worker_trace))
        .route("/worker/tasks/{id}/complete", post(complete))
        .route("/worker/tasks/{id}/fail", post(fail))
        .route("/notifications", get(notifications))
        .route(
            "/members/{handle}/settings",
            get(get_member_settings).put(put_member_settings),
        )
        .route("/routines/parse", post(parse_routine))
        .route("/routines", post(create_routine).get(list_routines))
        .route(
            "/routines/{id}",
            axum::routing::patch(update_routine).delete(delete_routine),
        )
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .nest("/v1", v1)
        .with_state(state)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Error::BadRequest(m) => error(StatusCode::BAD_REQUEST, m),
            Error::Forbidden(m) => error(StatusCode::FORBIDDEN, m),
            Error::NotFound(m) => error(StatusCode::NOT_FOUND, m),
            Error::Conflict { message, status } => (
                StatusCode::CONFLICT,
                Json(json!({"error": message, "status": status})),
            )
                .into_response(),
            Error::Unclear(question) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"question": question})),
            )
                .into_response(),
            Error::Unavailable(m) => error(StatusCode::SERVICE_UNAVAILABLE, m),
            Error::Store(e) => {
                tracing::error!(error = %e, "bots storage error");
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PM OS storage is unavailable right now; try again shortly",
                )
            }
        }
    }
}

type Reply = Result<Response, Error>;

fn bad(message: impl Into<String>) -> Error {
    Error::BadRequest(message.into())
}

fn forbidden(message: impl Into<String>) -> Error {
    Error::Forbidden(message.into())
}

/// Parse a JSON body, answering `400` (not axum's `422`) when it doesn't fit.
fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|e| bad(format!("the request body doesn't fit: {e}")))
}

/// A body that may be empty: `{}` then.
fn optional_body<T: DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, Error> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        Ok(T::default())
    } else {
        body(bytes)
    }
}

/// A Slack or web call made for one PM must name them in `X-Pmos-User`. (The shared feed and the
/// bot list don't call this.)
fn pm(caller: &Caller) -> Result<(), Error> {
    if caller.kind.is_pm() && caller.user.is_none() {
        return Err(forbidden(
            "say which PM this is for in X-Pmos-User (a PM OS handle)",
        ));
    }
    Ok(())
}

/// A per-bot worker may only touch its own bot's tasks; anyone else's look like they don't
/// exist (`404`).
async fn worker_owns(state: &AppState, caller: &Caller, id: &str) -> Result<(), Error> {
    let Some(own) = &caller.bot else {
        return Ok(());
    };
    let task = state.rt.task(id).await?;
    if &task.bot == own {
        Ok(())
    } else {
        Err(Error::NotFound(format!("no task {id}")))
    }
}

fn allow(caller: &Caller, kinds: &[ClientKind], what: &str) -> Result<(), Error> {
    if kinds.contains(&caller.kind) {
        Ok(())
    } else {
        Err(forbidden(format!(
            "the {} client may not {what}",
            caller.kind.as_str()
        )))
    }
}

async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(pmos_runtime::bearer_value);
    let Some(client) = presented.and_then(|t| {
        state
            .clients
            .iter()
            .find(|c| pmos_runtime::token_eq(t, &c.token))
    }) else {
        return error(StatusCode::UNAUTHORIZED, "a PM OS bots token is required");
    };
    let user = match client.kind {
        // The Slack client names the PM it acts for on per-PM calls (tasks, routines). The
        // shared notification feed and the bot list need no PM. Without the header, every
        // per-PM check compares against an empty handle and refuses.
        ClientKind::Slack | ClientKind::Web => match req.headers().get(USER_HEADER) {
            None => None,
            Some(v) => {
                let user = v.to_str().map(str::trim).unwrap_or_default();
                if !valid_handle(user) {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "X-Pmos-User must be a PM OS handle",
                    );
                }
                Some(user.to_string())
            }
        },
        _ => None,
    };
    let (kind, bot) = (client.kind, client.bot.clone());
    let mut user = user;
    if matches!(kind, ClientKind::Web | ClientKind::Console) && state.assertions.mode() != Mode::Off
    {
        match check_assertion(&state.assertions, kind, req).await {
            Ok((checked, subject)) => {
                req = checked;
                // The verified subject is the PM; any X-Pmos-User already equals it.
                if kind == ClientKind::Web && subject.is_some() {
                    user = subject;
                }
            }
            Err(refused) => return *refused,
        }
    }
    req.extensions_mut().insert(Caller { kind, user, bot });
    next.run(req).await
}

/// The one answer to every assertion failure. The log says why, by kind only: never the
/// assertion, a key, or a handle.
fn refuse_assertion(kind: ClientKind, why: Refusal) -> Box<Response> {
    tracing::warn!(client = kind.as_str(), reason = %why, "refused a user assertion");
    // Boxed: a `Response` is a large error to carry in a `Result`.
    Box::new(
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "code": "bad_assertion",
                "error": "this call needs a valid X-Pmos-Assertion for the person it acts for",
            })),
        )
            .into_response(),
    )
}

/// Every person `req` names: `X-Pmos-User`, the `requested_by` and `owner` filters, and the
/// handle in `/v1/members/{handle}/…`. A query that doesn't parse counts as naming someone no
/// assertion can match.
fn named_people(req: &Request) -> Vec<String> {
    let mut named = Vec::new();
    if let Some(v) = req.headers().get(USER_HEADER) {
        named.push(v.to_str().map(str::trim).unwrap_or_default().to_string());
    }
    let uri = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| &u.0)
        .unwrap_or(req.uri());
    match Query::<Vec<(String, String)>>::try_from_uri(uri) {
        Ok(Query(pairs)) => named.extend(
            pairs
                .into_iter()
                .filter(|(k, v)| (k == "requested_by" || k == "owner") && !v.is_empty())
                .map(|(_, v)| v),
        ),
        Err(_) => named.push(String::new()),
    }
    let segments: Vec<&str> = uri.path().trim_start_matches('/').split('/').collect();
    if let ["v1", "members", handle, ..] = segments.as_slice() {
        named.push(handle.to_string());
    }
    named
}

/// Check a `web` or `console` call's assertion, with the mode not `off`. Returns the request
/// (its body read and put back) and the verified subject, if the call carried an assertion.
async fn check_assertion(
    assertions: &Assertions,
    kind: ClientKind,
    req: Request,
) -> Result<(Request, Option<String>), Box<Response>> {
    let named = named_people(&req);
    let Some(header) = req.headers().get(pmos_assertion::HEADER).cloned() else {
        if assertions.mode() == Mode::Require && !named.is_empty() {
            return Err(refuse_assertion(kind, Refusal::Missing));
        }
        return Ok((req, None));
    };
    let Ok(header) = header.to_str() else {
        return Err(refuse_assertion(kind, Refusal::Header));
    };
    // The binding covers the request as this service reads it: method, the target as it
    // arrived (before `/v1` is stripped for the nested routes), and the raw body.
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
        return Err(refuse_assertion(kind, Refusal::Body));
    };
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map(|u| u.0.clone())
        .unwrap_or_else(|| parts.uri.clone());
    let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let who = assertions
        .verify(
            kind.as_str(),
            header,
            parts.method.as_str(),
            target,
            &bytes,
            pmos_assertion::unix_now(),
        )
        .map_err(|why| refuse_assertion(kind, why))?;
    if named.iter().any(|n| n != who.subject()) {
        return Err(refuse_assertion(kind, Refusal::SubjectMismatch));
    }
    tracing::debug!(
        client = kind.as_str(),
        session = who.session(),
        "user assertion accepted"
    );
    Ok((
        Request::from_parts(parts, Body::from(bytes)),
        Some(who.subject().to_string()),
    ))
}

async fn readyz(State(state): State<AppState>) -> Response {
    let (storage, toolserver, gateway) = state.rt.readiness().await;
    let ok = storage && toolserver && gateway;
    let mark = |b: bool| if b { "ok" } else { "down" };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "ready": ok,
            "checks": {
                "storage": mark(storage),
                "toolserver": mark(toolserver),
                "gateway": mark(gateway),
            },
        })),
    )
        .into_response()
}

// ----- bots -------------------------------------------------------------------------------

async fn list_bots(State(state): State<AppState>) -> Response {
    Json(state.rt.catalog().summaries()).into_response()
}

async fn get_bot(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.rt.catalog().detail(&name) {
        Some(d) => Json(d).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            format!("there's no bot called `{name}`"),
        ),
    }
}

// ----- tasks ------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTask {
    bot: String,
    requested_by: String,
    text: String,
    source: Source,
    #[serde(default)]
    slack: Option<SlackRef>,
    /// The task's own level: the evals client only.
    #[serde(default)]
    level: Option<Level>,
    /// The conversation this task continues: the web and Slack clients only.
    #[serde(default)]
    conversation: Option<String>,
}

async fn create_task(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    bytes: Bytes,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Evals],
        "create tasks",
    )?;
    let req: CreateTask = body(&bytes)?;
    if req.level.is_some() && caller.kind != ClientKind::Evals {
        return Err(forbidden(
            "only the evals client sets a task's level; a PM's own level comes from their settings",
        ));
    }
    if req.conversation.is_some()
        && !matches!(
            caller.kind,
            ClientKind::Slack | ClientKind::Web | ClientKind::Evals
        )
    {
        return Err(forbidden(
            "only the web, Slack and evals clients name a task's conversation",
        ));
    }
    match caller.kind {
        ClientKind::Slack | ClientKind::Web => {
            if req.requested_by != caller.user() {
                return Err(forbidden(
                    "the client creates tasks only for the PM in X-Pmos-User",
                ));
            }
            let allowed = match caller.kind {
                ClientKind::Web => req.source == Source::Web,
                _ => matches!(req.source, Source::Slack | Source::Cli),
            };
            if !allowed {
                return Err(forbidden(if caller.kind == ClientKind::Web {
                    "the web client creates `web` tasks only"
                } else {
                    "the Slack client creates `slack` or `cli` tasks only"
                }));
            }
        }
        _ => {
            if req.source != Source::Eval {
                return Err(forbidden("the evals client creates `eval` tasks only"));
            }
            // A benchmark task runs as the benchmark member, never as a PM: the evals token
            // must not be able to run a PM's personal bot with their accounts.
            if !state.evals_as.contains(&req.requested_by) {
                return Err(forbidden(format!(
                    "the evals client creates tasks only as {} (PMOS_EVALS_AS)",
                    state.evals_as.join(", ")
                )));
            }
        }
    }
    // An eval task never reaches Slack: it may not name a Slack target, and the runtime sends
    // no notifications for it.
    if req.source == Source::Eval && req.slack.is_some() {
        return Err(Error::BadRequest(
            "an eval task has no Slack target (`slack`)".into(),
        ));
    }
    let task = state
        .rt
        .create_task(
            NewTask {
                bot: req.bot,
                requested_by: req.requested_by,
                text: req.text,
                source: req.source,
                slack: req.slack,
                level: req.level,
                conversation: req.conversation,
            },
            model::now(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(task)).into_response())
}

/// Can `caller` see `task`? Slack sees its PM's own, evals sees eval tasks, the console all.
fn visible(caller: &Caller, task: &Task) -> bool {
    match caller.kind {
        ClientKind::Console => true,
        ClientKind::Slack | ClientKind::Web => task.requested_by == caller.user(),
        ClientKind::Evals => task.source == Source::Eval,
        ClientKind::Worker => false,
    }
}

async fn visible_task(state: &AppState, caller: &Caller, id: &str) -> Result<Task, Error> {
    match state.rt.task(id).await {
        Ok(task) if visible(caller, &task) => Ok(task),
        Ok(_) => Err(Error::NotFound(format!("no task {id}"))),
        Err(e) => Err(e),
    }
}

const READERS: &[ClientKind] = &[
    ClientKind::Slack,
    ClientKind::Web,
    ClientKind::Console,
    ClientKind::Evals,
];

async fn get_task(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(&caller, READERS, "read tasks")?;
    Ok(Json(visible_task(&state, &caller, &id).await?).into_response())
}

#[derive(Deserialize)]
struct TaskQuery {
    bot: Option<String>,
    requested_by: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
}

async fn list_tasks(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Query(q): Query<TaskQuery>,
) -> Reply {
    allow(&caller, READERS, "read tasks")?;
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => {
            Some(TaskStatus::parse(s).ok_or_else(|| bad(format!("`{s}` isn't a task status")))?)
        }
        None => None,
    };
    let mut filter = TaskFilter {
        bot: q.bot.filter(|b| !b.is_empty()),
        requested_by: q.requested_by.filter(|r| !r.is_empty()),
        status,
        source: None,
        limit: q.limit.unwrap_or(50).clamp(1, 200),
    };
    match caller.kind {
        ClientKind::Slack | ClientKind::Web => {
            pm(&caller)?;
            if filter
                .requested_by
                .as_deref()
                .is_some_and(|r| r != caller.user())
            {
                return Err(forbidden("the Slack client reads only its PM's own tasks"));
            }
            filter.requested_by = Some(caller.user().to_string());
        }
        ClientKind::Evals => filter.source = Some(Source::Eval),
        _ => {}
    }
    let tasks = state.rt.store.tasks(&filter).await?;
    Ok(Json(tasks).into_response())
}

async fn cancel_task(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Console],
        "cancel tasks",
    )?;
    visible_task(&state, &caller, &id).await?;
    let by = match caller.kind {
        ClientKind::Slack | ClientKind::Web => caller.user().to_string(),
        _ => "the console".into(),
    };
    let task = state.rt.cancel(&id, &by, model::now()).await?;
    Ok(Json(task).into_response())
}

async fn task_events(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(&caller, READERS, "read task events")?;
    visible_task(&state, &caller, &id).await?;
    let events = state.rt.store.events(&id).await?;
    Ok(Json(events).into_response())
}

async fn task_trace(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(&caller, &[ClientKind::Console], "read traces")?;
    visible_task(&state, &caller, &id).await?;
    let trace = state.rt.store.trace(&id).await?;
    Ok(Json(trace).into_response())
}

/// `{drafts: [{id, tool, args, status}]}`: the task's approval items as the tool server has
/// them now.
async fn task_drafts(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Console, ClientKind::Evals],
        "read drafts",
    )?;
    let task = visible_task(&state, &caller, &id).await?;
    let drafts = state.rt.drafts(&task).await?;
    Ok(Json(json!({ "drafts": drafts })).into_response())
}

// ----- workers ----------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBody {
    worker: String,
    #[serde(default)]
    bots: Option<Vec<String>>,
    #[serde(default)]
    wait_ms: Option<u64>,
}

fn valid_worker(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 100 && !name.chars().any(char::is_control)
}

async fn claim(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "claim tasks")?;
    let req: ClaimBody = body(&bytes)?;
    if !valid_worker(&req.worker) {
        return Err(bad("`worker` must name the worker"));
    }
    let wait = Duration::from_millis(req.wait_ms.unwrap_or(0));
    let mut bots = req.bots.filter(|b| !b.is_empty());
    // A per-bot worker claims only its own bot's tasks.
    if let Some(own) = &caller.bot {
        if bots
            .as_ref()
            .is_some_and(|b| b.iter().any(|name| name != own))
        {
            return Err(forbidden(format!(
                "this worker serves only the `{own}` bot"
            )));
        }
        bots = Some(vec![own.clone()]);
    }
    match state
        .rt
        .claim(req.worker.trim(), bots.as_deref(), wait)
        .await?
    {
        Some(claim) => Ok(Json(claim).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

/// Every worker call may name the worker; if it does, it must hold the claim.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WorkerRef {
    #[serde(default)]
    worker: Option<String>,
}

async fn heartbeat(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "heartbeat tasks")?;
    worker_owns(&state, &caller, &id).await?;
    let req: WorkerRef = optional_body(&bytes)?;
    let until = state
        .rt
        .heartbeat(&id, req.worker.as_deref(), model::now())
        .await?;
    Ok(Json(json!({"lease_until": until})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventBody {
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    checklist: Option<Vec<ChecklistItem>>,
    #[serde(default)]
    worker: Option<String>,
}

async fn worker_events(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "report task events")?;
    worker_owns(&state, &caller, &id).await?;
    let req: EventBody = body(&bytes)?;
    let text = req
        .text
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if text.as_ref().is_some_and(|t| t.chars().count() > 2_000) {
        return Err(bad("event text is longer than 2000 characters"));
    }
    let need_text = |kind: &str| bad(format!("a `{kind}` event needs `text`"));
    let event = match req.kind.as_str() {
        "status" => WorkerEvent::Status(text.ok_or_else(|| need_text("status"))?),
        "milestone" => WorkerEvent::Milestone(text.ok_or_else(|| need_text("milestone"))?),
        "checklist" => WorkerEvent::Checklist(
            req.checklist
                .ok_or_else(|| bad("a `checklist` event needs `checklist`"))?,
            text,
        ),
        other => {
            return Err(bad(format!(
                "`{other}` isn't an event kind: use status, checklist or milestone"
            )))
        }
    };
    let task = state
        .rt
        .worker_event(&id, req.worker.as_deref(), event, model::now())
        .await?;
    Ok(Json(json!({"status": task.status})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceItem {
    kind: TraceKind,
    body: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceBody {
    entries: Vec<TraceItem>,
    /// Accepted like on every worker call; traces may arrive after the task finished, so
    /// the claim isn't checked.
    #[serde(default, rename = "worker")]
    _worker: Option<String>,
}

async fn worker_trace(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "write traces")?;
    worker_owns(&state, &caller, &id).await?;
    let req: TraceBody = body(&bytes)?;
    let entries = req.entries.into_iter().map(|e| (e.kind, e.body)).collect();
    let n = state.rt.add_trace(&id, entries, model::now()).await?;
    Ok(Json(json!({"recorded": n})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteBody {
    result: String,
    #[serde(default)]
    approvals: Vec<String>,
    #[serde(default)]
    worker: Option<String>,
}

async fn complete(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "complete tasks")?;
    worker_owns(&state, &caller, &id).await?;
    let req: CompleteBody = body(&bytes)?;
    let task = state
        .rt
        .complete(
            &id,
            req.worker.as_deref(),
            &req.result,
            req.approvals,
            model::now(),
        )
        .await?;
    Ok(Json(task).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailBody {
    reason: String,
    #[serde(default)]
    worker: Option<String>,
}

async fn fail(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(&caller, &[ClientKind::Worker], "fail tasks")?;
    worker_owns(&state, &caller, &id).await?;
    let req: FailBody = body(&bytes)?;
    let task = state
        .rt
        .fail(&id, req.worker.as_deref(), &req.reason, model::now())
        .await?;
    Ok(Json(task).into_response())
}

// ----- notifications ----------------------------------------------------------------------

#[derive(Deserialize)]
struct FeedQuery {
    after: Option<String>,
    wait_ms: Option<u64>,
}

async fn notifications(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Query(q): Query<FeedQuery>,
) -> Reply {
    allow(&caller, &[ClientKind::Slack], "read notifications")?;
    let after = match q.after.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        None => 0,
        Some(a) => a
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| bad("`after` must be a cursor from this feed"))?,
    };
    let (cursor, items) = state
        .rt
        .notifications(after, Duration::from_millis(q.wait_ms.unwrap_or(0)))
        .await?;
    Ok(Json(json!({"cursor": cursor.to_string(), "items": items})).into_response())
}

// ----- member settings --------------------------------------------------------------------

/// `{"handle": "...", "level": "senior"}`.
fn settings_reply(s: &MemberSettings) -> Response {
    Json(json!({"handle": s.handle, "level": s.level})).into_response()
}

/// The Slack client reads and sets only the settings of the PM in `X-Pmos-User`.
fn own_settings(caller: &Caller, handle: &str) -> Result<(), Error> {
    pm(caller)?;
    if caller.kind.is_pm() && handle != caller.user() {
        return Err(forbidden(
            "the Slack client reads and sets only the settings of the PM in X-Pmos-User",
        ));
    }
    Ok(())
}

async fn get_member_settings(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(handle): Path<String>,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Console],
        "read member settings",
    )?;
    own_settings(&caller, &handle)?;
    Ok(settings_reply(&state.rt.member_settings(&handle).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutSettings {
    level: String,
}

async fn put_member_settings(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(handle): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web],
        "change member settings",
    )?;
    own_settings(&caller, &handle)?;
    let req: PutSettings = body(&bytes)?;
    let level = Level::parse_or_explain(&req.level).map_err(bad)?;
    let settings = state
        .rt
        .set_member_level(&handle, level, model::now())
        .await?;
    Ok(settings_reply(&settings))
}

// ----- routines ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParseBody {
    bot: String,
    owner: String,
    text: String,
    tz: String,
}

fn own(caller: &Caller, owner: &str) -> Result<(), Error> {
    pm(caller)?;
    if caller.kind.is_pm() && owner != caller.user() {
        return Err(forbidden(
            "the Slack client manages only the routines of the PM in X-Pmos-User",
        ));
    }
    Ok(())
}

async fn parse_routine(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    bytes: Bytes,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web],
        "parse routines",
    )?;
    let req: ParseBody = body(&bytes)?;
    own(&caller, &req.owner)?;
    let (draft, summary) = state
        .rt
        .parse_routine(&req.bot, &req.owner, &req.text, &req.tz, model::now())
        .await?;
    Ok(Json(json!({"draft": draft, "summary": summary})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRoutine {
    draft: Draft,
}

async fn create_routine(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    bytes: Bytes,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web],
        "create routines",
    )?;
    let req: CreateRoutine = body(&bytes)?;
    own(&caller, &req.draft.owner)?;
    let routine = state.rt.create_routine(req.draft, model::now()).await?;
    Ok((StatusCode::CREATED, Json(routine)).into_response())
}

#[derive(Deserialize)]
struct RoutineQuery {
    owner: Option<String>,
}

async fn list_routines(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Query(q): Query<RoutineQuery>,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Console],
        "read routines",
    )?;
    let mut owner = q.owner.filter(|o| !o.is_empty());
    if caller.kind.is_pm() {
        own(&caller, owner.as_deref().unwrap_or(caller.user()))?;
        owner = Some(caller.user().to_string());
    }
    let routines = state.rt.store.routines(owner.as_deref()).await?;
    Ok(Json(routines).into_response())
}

/// The routine, if `caller` may manage it (else `404`, as if it didn't exist).
async fn managed_routine(state: &AppState, caller: &Caller, id: &str) -> Result<(), Error> {
    let routine = state.rt.routine(id).await?;
    if caller.kind.is_pm() && routine.owner != caller.user() {
        return Err(Error::NotFound(format!("no routine {id}")));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRoutine {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    prompt: Option<String>,
}

async fn update_routine(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Console],
        "change routines",
    )?;
    let req: PatchRoutine = body(&bytes)?;
    if caller.kind == ClientKind::Console && (req.schedule.is_some() || req.prompt.is_some()) {
        return Err(forbidden("the console may pause and resume routines, not change them; the owner does that in Slack",
        ));
    }
    managed_routine(&state, &caller, &id).await?;
    let routine = state
        .rt
        .update_routine(
            &id,
            RoutineChange {
                enabled: req.enabled,
                schedule: req.schedule,
                prompt: req.prompt,
            },
            model::now(),
        )
        .await?;
    Ok(Json(routine).into_response())
}

async fn delete_routine(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Reply {
    allow(
        &caller,
        &[ClientKind::Slack, ClientKind::Web, ClientKind::Console],
        "delete routines",
    )?;
    managed_routine(&state, &caller, &id).await?;
    state.rt.delete_routine(&id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
