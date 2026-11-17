#![allow(dead_code)]

//! Shared test pieces: a fixed catalog of bots, a mock tool server and gateway, the runtime
//! served on a local port, and a Postgres store in a fresh schema.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use pmos_bots::assertion::Assertions;
use pmos_bots::catalog::Catalog;
use pmos_bots::clients::{Gateway, ToolServer};
use pmos_bots::config::{parse_clients, Client};
use pmos_bots::pg::PgStore;
use pmos_bots::store::{BotsStore, MemoryStore};
use pmos_bots::{router, router_with, Runtime};
use pmos_contract::bots::{ActionSetting, Bot, BotScope, Reads, Role};
use pmos_contract::model::{ModelRole, Sensitivity, Skill, ToolDef, ToolKind, ToolRegistry};
use pmos_directory::snapshot::{
    user, DirectoryUser, FakeClock, SnapshotClient, StaticFetcher, UserStatus,
};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::Connection;
use tokio_util::sync::CancellationToken;

pub mod signing;

pub const SLACK: &str = "slack-token-0123456789abcdef0123456789";
pub const WEB: &str = "web-token-0123456789abcdef0123456789abc";
pub const WORKER: &str = "worker-token-0123456789abcdef012345678";
/// The Docs bot's own worker token (`worker-docs`): it serves only `docs`.
pub const DOCS_WORKER: &str = "docs-worker-token-0123456789abcdef0123";
pub const CONSOLE: &str = "console-token-0123456789abcdef01234567";
pub const EVALS: &str = "evals-token-0123456789abcdef0123456789";
pub const SERVICE: &str = "service-token-0123456789abcdef0123456";
pub const GATEWAY: &str = "gateway-token-0123456789abcdef0123456";

pub fn clients() -> Vec<Client> {
    parse_clients(&format!(
        "slack={SLACK},web={WEB},worker={WORKER},worker-docs={DOCS_WORKER},console={CONSOLE},evals={EVALS}"
    ))
    .unwrap()
}

/// `PMOS_EVALS_AS` for the runtime under test: ben is the benchmark member here (a team member
/// in the fixed catalog), so eval tasks run as him and as nobody else.
pub fn evals_as() -> Vec<String> {
    vec!["ben".into()]
}

/// A whole-second time, so both stores keep it exactly.
pub fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

// ----- catalog ----------------------------------------------------------------------------

fn skill(name: &str, tools: &[&str]) -> Skill {
    Skill {
        name: name.into(),
        description: format!("{name} skill"),
        owner: "pm".into(),
        version: "0.1.0".into(),
        model: ModelRole::Default,
        data_class: Sensitivity::Public,
        tools: tools.iter().map(|t| t.to_string()).collect(),
        surfaces: vec!["slack".into()],
        ships_in: None,
        track: None,
    }
}

fn tool(name: &str, kind: ToolKind) -> ToolDef {
    ToolDef {
        name: name.into(),
        system: name.split('.').next().unwrap().into(),
        kind,
        description: None,
    }
}

/// The user directory every test runtime starts with (the tool server's
/// `/internal/directory`): `ana` and `ben` hold `pm`, `cy` holds `pm` and `admin`. `eve` is
/// nobody.
pub fn directory() -> Vec<DirectoryUser> {
    vec![
        user("ana", &["pm"]),
        user("ben", &["pm"]),
        user("cy", &["pm", "admin"]),
    ]
}

/// `handle`, deactivated, with the roles they held kept (they grant nothing).
pub fn deactivated(handle: &str, roles: &[&str]) -> DirectoryUser {
    DirectoryUser {
        status: UserStatus::Deactivated,
        ..user(handle, roles)
    }
}

/// Two bots: the team bot `docs` and `pm-ana`, Ana's personal bot, whose role may draft Jira
/// issues (approve) but never send Slack messages. Who may use them is the directory's.
pub fn catalog() -> Catalog {
    let roles = vec![
        Role {
            name: "docs-helper".into(),
            description: "Answers from the docs".into(),
            owner: "pm".into(),
            version: "0.1.0".into(),
            skills: vec!["docs".into()],
            data_class: Sensitivity::Public,
            model: ModelRole::Default,
            actions: BTreeMap::new(),
            instructions: "Cite every page.".into(),
        },
        Role {
            name: "pm-assistant".into(),
            description: "A PM's assistant".into(),
            owner: "pm".into(),
            version: "0.1.0".into(),
            skills: vec!["ask".into()],
            data_class: Sensitivity::Restricted,
            model: ModelRole::Extraction,
            actions: BTreeMap::from([("slack.send_message".into(), ActionSetting::Never)]),
            instructions: "Draft; never send.".into(),
        },
    ];
    let bots = vec![
        Bot {
            name: "docs".into(),
            role: "docs-helper".into(),
            scope: BotScope::Team,
            owner: "ana".into(),
            description: "The Docs bot".into(),
            reads: Reads::default(),
            actions: BTreeMap::new(),
            job: "Answer from the docs.".into(),
        },
        Bot {
            name: "pm-ana".into(),
            role: "pm-assistant".into(),
            scope: BotScope::Personal,
            owner: "ana".into(),
            description: "Ana's assistant".into(),
            reads: Reads::default(),
            actions: BTreeMap::new(),
            job: "Help Ana.".into(),
        },
    ];
    let skills = vec![
        skill("docs", &["docs.search", "docs.read"]),
        skill(
            "ask",
            &["jira.search", "jira.create_issue", "slack.send_message"],
        ),
    ];
    let registry = ToolRegistry {
        tools: vec![
            tool("docs.search", ToolKind::Read),
            tool("docs.read", ToolKind::Read),
            tool("jira.search", ToolKind::Read),
            tool("jira.create_issue", ToolKind::Write),
            tool("slack.send_message", ToolKind::Write),
        ],
    };
    Catalog::from_parts(bots, roles, skills, registry)
}

// ----- mock tool server -------------------------------------------------------------------

/// How the mock tool server answers session requests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionMode {
    Ok,
    /// `403` with a reason.
    Refuse,
    /// `503`.
    Down,
}

pub struct MockToolServer {
    pub mode: Mutex<SessionMode>,
    /// Session requests, in order.
    pub sessions: Mutex<Vec<Value>>,
    /// Approval items by id.
    pub approvals: Mutex<HashMap<String, Value>>,
    pub approval_reads: Mutex<usize>,
    /// Approval reads answer `503`.
    pub approvals_down: Mutex<bool>,
    pub ready: Mutex<bool>,
    /// What `GET /internal/runs/{id}/sensitivity` answers per run: a level, or `None` for
    /// "no session stored". A run not listed answers `public`.
    pub run_levels: Mutex<HashMap<String, Option<String>>>,
    /// Run-level reads answer `503`.
    pub runs_down: Mutex<bool>,
}

impl Default for MockToolServer {
    fn default() -> Self {
        Self {
            mode: Mutex::new(SessionMode::Ok),
            run_levels: Mutex::default(),
            runs_down: Mutex::new(false),
            sessions: Mutex::default(),
            approvals: Mutex::default(),
            approval_reads: Mutex::new(0),
            approvals_down: Mutex::new(false),
            ready: Mutex::new(true),
        }
    }
}

impl MockToolServer {
    pub fn set_approval(&self, id: &str, tool: &str, status: &str, by: Option<&str>) {
        let mut item = json!({"id": id, "user": "ana", "skill": "ask", "run_id": "x",
            "tool": tool, "args": {}, "args_hash": "h", "status": status,
            "created_at": "2026-11-14T10:00:00Z"});
        if let Some(by) = by {
            item["decided_by"] = json!(by);
        }
        self.approvals.lock().unwrap().insert(id.into(), item);
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {SERVICE}"))
}

async fn ts_session(
    State(ts): State<Arc<MockToolServer>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "service token required"})),
        )
            .into_response();
    }
    let n = {
        let mut s = ts.sessions.lock().unwrap();
        s.push(body);
        s.len()
    };
    match *ts.mode.lock().unwrap() {
        SessionMode::Ok => (
            StatusCode::CREATED,
            Json(json!({"token": format!("pmos_s_test{n}"), "session": {}, "tools": []})),
        )
            .into_response(),
        SessionMode::Refuse => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pm-ana is ana's personal bot"})),
        )
            .into_response(),
        SessionMode::Down => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "storage unavailable"})),
        )
            .into_response(),
    }
}

async fn ts_approval(
    State(ts): State<Arc<MockToolServer>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    *ts.approval_reads.lock().unwrap() += 1;
    if *ts.approvals_down.lock().unwrap() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "PM OS storage is unavailable"})),
        )
            .into_response();
    }
    match ts.approvals.lock().unwrap().get(&id) {
        Some(item) => Json(item.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such approval item"})),
        )
            .into_response(),
    }
}

async fn ts_run_level(
    State(ts): State<Arc<MockToolServer>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if *ts.runs_down.lock().unwrap() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let level = ts
        .run_levels
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .unwrap_or(Some("public".into()));
    Json(json!({"run_id": id, "sensitivity": level})).into_response()
}

async fn ts_ready(State(ts): State<Arc<MockToolServer>>) -> StatusCode {
    if *ts.ready.lock().unwrap() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

// ----- mock gateway -----------------------------------------------------------------------

pub struct MockGateway {
    /// The model's reply content.
    pub reply: Mutex<String>,
    /// Status to answer with (200 normally).
    pub status: Mutex<u16>,
    /// Requests seen: (headers we care about, body).
    pub seen: Mutex<Vec<(HashMap<String, String>, Value)>>,
}

impl Default for MockGateway {
    fn default() -> Self {
        Self {
            reply: Mutex::new("{}".into()),
            status: Mutex::new(200),
            seen: Mutex::default(),
        }
    }
}

async fn gw_chat(
    State(gw): State<Arc<MockGateway>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {GATEWAY}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let keep: HashMap<String, String> = ["x-pmos-user", "x-pmos-priority", "x-pmos-run"]
        .iter()
        .filter_map(|k| {
            headers
                .get(*k)
                .and_then(|v| v.to_str().ok())
                .map(|v| (k.to_string(), v.to_string()))
        })
        .collect();
    gw.seen.lock().unwrap().push((keep, body));
    let status = *gw.status.lock().unwrap();
    if status != 200 {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error": {"message": "busy"}})),
        )
            .into_response();
    }
    let content = gw.reply.lock().unwrap().clone();
    Json(json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": content}}]}))
        .into_response()
}

async fn gw_models(headers: HeaderMap) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {GATEWAY}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"data": [{"id": "pmos/default"}]})).into_response()
}

async fn serve(app: Router, cancel: &CancellationToken) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    base
}

// ----- the runtime under test ---------------------------------------------------------------

pub struct Env {
    pub base: String,
    pub rt: Arc<Runtime>,
    pub store: Arc<dyn BotsStore>,
    pub ts: Arc<MockToolServer>,
    pub gw: Arc<MockGateway>,
    pub http: reqwest::Client,
    /// The user directory as the tool server answers it: change it, then [`Env::refresh`].
    pub people: Arc<StaticFetcher>,
    /// The directory's clock: frozen unless a test moves it, so the snapshot stays fresh.
    pub clock: Arc<FakeClock>,
    stop: CancellationToken,
    pg: Option<TestPg>,
}

impl Env {
    async fn with_store(store: Arc<dyn BotsStore>, pg: Option<TestPg>) -> Self {
        Self::with_parts(store, pg, catalog(), None).await
    }

    async fn with_parts(
        store: Arc<dyn BotsStore>,
        pg: Option<TestPg>,
        catalog: Catalog,
        assertions: Option<Assertions>,
    ) -> Self {
        let stop = CancellationToken::new();
        let ts = Arc::new(MockToolServer::default());
        let gw = Arc::new(MockGateway::default());
        let ts_base = serve(
            Router::new()
                .route("/internal/sessions", post(ts_session))
                .route("/internal/approvals/{id}", get(ts_approval))
                .route("/internal/runs/{id}/sensitivity", get(ts_run_level))
                .route("/readyz", get(ts_ready))
                .with_state(ts.clone()),
            &stop,
        )
        .await;
        let gw_base = serve(
            Router::new()
                .route("/v1/chat/completions", post(gw_chat))
                .route("/v1/models", get(gw_models))
                .with_state(gw.clone()),
            &stop,
        )
        .await;
        let people = Arc::new(StaticFetcher::new(directory()));
        let clock = Arc::new(FakeClock::default());
        let snapshots = Arc::new(SnapshotClient::new(people.clone(), clock.clone()));
        snapshots
            .refresh()
            .await
            .expect("the test directory answers");
        let rt = Arc::new(Runtime::new(
            store.clone(),
            Arc::new(catalog),
            snapshots,
            ToolServer::new(&ts_base, SERVICE),
            Gateway::new(&format!("{gw_base}/v1"), GATEWAY),
            stop.child_token(),
        ));
        let app = match assertions {
            None => router(rt.clone(), clients(), evals_as()),
            Some(a) => router_with(rt.clone(), clients(), evals_as(), Arc::new(a)),
        };
        let base = serve(app, &stop).await;
        Self {
            base,
            rt,
            store,
            ts,
            gw,
            http: reqwest::Client::new(),
            people,
            clock,
            stop,
            pg,
        }
    }

    /// Poll the directory now, as the runtime's 10-second loop would.
    pub async fn refresh(&self) {
        self.rt
            .people()
            .refresh()
            .await
            .expect("the test directory answers");
    }

    /// Change one user in the directory and poll it.
    pub async fn set_user(&self, u: DirectoryUser) {
        self.people.upsert(u);
        self.refresh().await;
    }

    pub async fn memory() -> Self {
        Self::with_store(Arc::new(MemoryStore::default()), None).await
    }

    /// In memory, with the bots of `catalog` (such as one loaded from a content tree).
    pub async fn memory_with(catalog: Catalog) -> Self {
        Self::with_parts(Arc::new(MemoryStore::default()), None, catalog, None).await
    }

    /// In memory, checking user assertions from `web` and `console` as `assertions` says.
    pub async fn memory_with_assertions(assertions: Assertions) -> Self {
        Self::with_parts(
            Arc::new(MemoryStore::default()),
            None,
            catalog(),
            Some(assertions),
        )
        .await
    }

    /// In memory, with `catalog`, checking assertions as `assertions` says.
    pub async fn memory_with_catalog_and_assertions(
        catalog: Catalog,
        assertions: Assertions,
    ) -> Self {
        Self::with_parts(
            Arc::new(MemoryStore::default()),
            None,
            catalog,
            Some(assertions),
        )
        .await
    }

    pub async fn postgres() -> Option<Self> {
        let pg = TestPg::new().await?;
        let store: Arc<dyn BotsStore> = Arc::new(pg.store.clone());
        Some(Self::with_store(store, Some(pg)).await)
    }

    /// A second runtime on this one's Postgres database, as after a restart. `None` in
    /// memory. Finish it before this one.
    pub async fn restarted(&self) -> Option<Self> {
        let pg = self.pg.as_ref()?;
        let store: Arc<dyn BotsStore> = Arc::new(pg.reconnect().await);
        Some(Self::with_store(store, None).await)
    }

    pub async fn put(
        &self,
        path: &str,
        client: &str,
        user: Option<&str>,
        body: Value,
    ) -> (u16, Value) {
        let res = self
            .req(reqwest::Method::PUT, path, client, user)
            .json(&body)
            .send()
            .await
            .unwrap();
        read(res).await
    }

    pub async fn finish(self) {
        self.stop.cancel();
        if let Some(pg) = self.pg {
            pg.drop_schema().await;
        }
    }

    /// A request as `client` (`slack`, `web`, `worker`, `console`, `evals`, or anything else for
    /// no token), with `X-Pmos-User` for Slack.
    pub fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        client: &str,
        user: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        let token = match client {
            "slack" => Some(SLACK),
            "web" => Some(WEB),
            "worker" => Some(WORKER),
            "worker-docs" => Some(DOCS_WORKER),
            "console" => Some(CONSOLE),
            "evals" => Some(EVALS),
            _ => None,
        };
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(u) = user {
            r = r.header("x-pmos-user", u);
        }
        r
    }

    pub async fn get(&self, path: &str, client: &str, user: Option<&str>) -> (u16, Value) {
        let res = self
            .req(reqwest::Method::GET, path, client, user)
            .send()
            .await
            .unwrap();
        read(res).await
    }

    pub async fn post(
        &self,
        path: &str,
        client: &str,
        user: Option<&str>,
        body: Value,
    ) -> (u16, Value) {
        let res = self
            .req(reqwest::Method::POST, path, client, user)
            .json(&body)
            .send()
            .await
            .unwrap();
        read(res).await
    }

    pub async fn patch(
        &self,
        path: &str,
        client: &str,
        user: Option<&str>,
        body: Value,
    ) -> (u16, Value) {
        let res = self
            .req(reqwest::Method::PATCH, path, client, user)
            .json(&body)
            .send()
            .await
            .unwrap();
        read(res).await
    }

    pub async fn delete(&self, path: &str, client: &str, user: Option<&str>) -> (u16, Value) {
        let res = self
            .req(reqwest::Method::DELETE, path, client, user)
            .send()
            .await
            .unwrap();
        read(res).await
    }

    /// Ana asks the Docs bot something from Slack.
    pub async fn ask(&self, text: &str) -> Value {
        let (status, task) = self
            .post(
                "/v1/tasks",
                "slack",
                Some("ana"),
                json!({"bot": "docs", "requested_by": "ana", "text": text, "source": "slack",
                       "slack": {"channel": "D0ANA", "thread_ts": "1731.0001"}}),
            )
            .await;
        assert_eq!(status, 201, "{task}");
        task
    }

    /// The worker `w1` claims without waiting.
    pub async fn claim(&self) -> (u16, Value) {
        self.post("/v1/worker/claim", "worker", None, json!({"worker": "w1"}))
            .await
    }

    /// Every notification so far.
    pub async fn feed(&self) -> Vec<Value> {
        let (status, body) = self.get("/v1/notifications", "slack", Some("ana")).await;
        assert_eq!(status, 200, "{body}");
        body["items"].as_array().unwrap().clone()
    }
}

pub async fn read(res: reqwest::Response) -> (u16, Value) {
    let status = res.status().as_u16();
    let text = res.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, body)
}

// ----- Postgres ---------------------------------------------------------------------------

/// A Postgres store in a fresh schema. Needs `PMOS_TEST_DATABASE_URL`; skipped locally
/// without it and fails in CI (`CI` set).
pub struct TestPg {
    pub store: PgStore,
    base: PgConnectOptions,
    schema: String,
}

impl TestPg {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("PMOS_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "PMOS_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: PMOS_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("PMOS_TEST_DATABASE_URL parses");
        let schema = format!("b_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect_with(&base)
            .await
            .expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(base.clone().options([("search_path", schema.as_str())]))
            .await
            .expect("connect to test schema");
        let store = PgStore::from_pool(pool);
        store.migrate().await.expect("migrations apply");
        Some(Self {
            store,
            base,
            schema,
        })
    }

    /// A new pool on the same schema, as a restarted process would open.
    pub async fn reconnect(&self) -> PgStore {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(
                self.base
                    .clone()
                    .options([("search_path", self.schema.as_str())]),
            )
            .await
            .expect("reconnect to test schema");
        let store = PgStore::from_pool(pool);
        store
            .migrate()
            .await
            .expect("migrations are already applied");
        store
    }

    pub async fn drop_schema(self) {
        self.store.pool().close().await;
        if let Ok(mut admin) = sqlx::PgConnection::connect_with(&self.base).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&mut admin)
            .await;
        }
    }
}

/// Run each named case against the memory store and, when Postgres is configured, against
/// Postgres. Each case is an `async fn(Arc<dyn BotsStore>)`.
#[allow(unused_macros)]
macro_rules! both_stores {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                let store: std::sync::Arc<dyn pmos_bots::store::BotsStore> =
                    std::sync::Arc::new(pmos_bots::store::MemoryStore::default());
                super::$case(store).await;
            } )*
        }
        mod postgres {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                if let Some(pg) = super::common::TestPg::new().await {
                    let store: std::sync::Arc<dyn pmos_bots::store::BotsStore> =
                        std::sync::Arc::new(pg.store.clone());
                    super::$case(store).await;
                    pg.drop_schema().await;
                }
            } )*
        }
    };
}

/// Run each case against a full `Env` on both stores.
#[allow(unused_macros)]
macro_rules! both_envs {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                let env = super::common::Env::memory().await;
                super::$case(&env).await;
                env.finish().await;
            } )*
        }
        mod postgres {
            $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $case() {
                if let Some(env) = super::common::Env::postgres().await {
                    super::$case(&env).await;
                    env.finish().await;
                }
            } )*
        }
    };
}
