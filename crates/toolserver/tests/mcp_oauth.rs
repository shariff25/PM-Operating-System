//! Slice M4: per-PM OAuth to an MCP server (`docs/mcp-servers-spec.md` §2.7; threats 4, 6, 7,
//! 12; MCP-10 to MCP-12), against a fake authorization server and the fake MCP server, both on
//! loopback. The record has the production shape (`https://` hosts, `system_vendor`,
//! `oauth_per_user`); only the test build's routes send its two hosts to the local fakes.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use chrono::{Duration, Utc};
use common::{member, memory_users};
use pmos_contract::model::Role;
use pmos_toolserver::connectors::{Caller, Connector, ConnectorError, TeamBot};
use pmos_toolserver::mcp_client::fake::{self, FakeMcp};
use pmos_toolserver::mcp_client::oauth::{
    McpCredentials, McpOAuth, McpOAuthError, MemoryMcpCredentials, PgMcpCredentials, SealedMcp,
};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth, UserTokens};
use pmos_toolserver::users::access::People;
use pmos_toolserver::vault::KeyRing;
use pmos_toolserver::Stores;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MCP_HOST: &str = "mcp.example.test";
const AUTH_HOST: &str = "auth.example.test";
const RESOURCE: &str = "https://mcp.example.test/mcp";

// ----- every log line of this test binary, to check that no token is in one -----------------

static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let b = buf.clone();
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || Capture(b.clone()))
            .try_init();
        buf
    })
    .clone()
}

// ----- a fake authorization server -----------------------------------------------------------

#[derive(Default)]
struct AuthServer {
    /// code → (PKCE challenge, the account's email)
    codes: Mutex<HashMap<String, (String, String)>>,
    /// refresh token → email
    refresh: Mutex<HashMap<String, String>>,
    revoked: AtomicBool,
    registered: AtomicBool,
    /// Every token request's grant type and whether its resource was the server's.
    requests: Mutex<Vec<(String, bool)>>,
    /// Every token issued, to look for in logs and events.
    issued: Mutex<Vec<String>>,
    scope: Mutex<String>,
    expires_in: AtomicU64,
    metadata: Mutex<Value>,
    n: AtomicU64,
    mcp: Mutex<Option<FakeMcp>>,
}

fn metadata() -> Value {
    json!({
        "issuer": "https://auth.example.test",
        "authorization_endpoint": "https://auth.example.test/authorize",
        "token_endpoint": "https://auth.example.test/token",
        "registration_endpoint": "https://auth.example.test/register",
        "code_challenge_methods_supported": ["S256"],
    })
}

async fn as_metadata(State(a): State<Arc<AuthServer>>) -> Json<Value> {
    Json(a.metadata.lock().unwrap().clone())
}

async fn as_register(State(a): State<Arc<AuthServer>>) -> &'static str {
    a.registered.store(true, Ordering::SeqCst);
    "{}"
}

async fn as_token(
    State(a): State<Arc<AuthServer>>,
    Form(form): Form<HashMap<String, String>>,
) -> (axum::http::StatusCode, Json<Value>) {
    let grant = form.get("grant_type").cloned().unwrap_or_default();
    a.requests.lock().unwrap().push((
        grant.clone(),
        form.get("resource").map(String::as_str) == Some(RESOURCE),
    ));
    let bad = |e: &str| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": e})),
        )
    };
    let email = match grant.as_str() {
        "authorization_code" => {
            let code = form.get("code").cloned().unwrap_or_default();
            let Some((challenge, email)) = a.codes.lock().unwrap().remove(&code) else {
                return bad("invalid_grant");
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let got = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(verifier.as_bytes()));
            if got != challenge
                || form.get("client_id").map(String::as_str) != Some("pmos-test-client")
            {
                return bad("invalid_grant");
            }
            email
        }
        "refresh_token" => {
            if a.revoked.load(Ordering::SeqCst) {
                return bad("invalid_grant");
            }
            let rt = form.get("refresh_token").cloned().unwrap_or_default();
            match a.refresh.lock().unwrap().get(&rt) {
                Some(e) => e.clone(),
                None => return bad("invalid_grant"),
            }
        }
        _ => return bad("unsupported_grant_type"),
    };
    let n = a.n.fetch_add(1, Ordering::SeqCst);
    let access = format!("access-token-{n}-0123456789abcdef");
    let refresh = format!("refresh-token-{n}-0123456789abcdef");
    a.refresh
        .lock()
        .unwrap()
        .insert(refresh.clone(), email.clone());
    a.issued
        .lock()
        .unwrap()
        .extend([access.clone(), refresh.clone()]);
    if let Some(mcp) = a.mcp.lock().unwrap().as_ref() {
        mcp.add_account(&access, &email);
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": a.expires_in.load(Ordering::SeqCst),
            "refresh_token": refresh,
            "scope": a.scope.lock().unwrap().clone(),
        })),
    )
}

async fn serve(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

// ----- the setup -------------------------------------------------------------------------------

fn record() -> pmos_contract::mcp::ServerRecord {
    let d = FakeMcp::digest;
    let yaml = format!(
        r##"id: acme-jira
operator: system_vendor
transport: streamable_http
url: {RESOURCE}
identity: {{host: {MCP_HOST}}}
auth: oauth_per_user
oauth:
  client_id: pmos-test-client
  issuer: https://auth.example.test
  authorization_endpoint: https://auth.example.test/authorize
  token_endpoint: https://auth.example.test/token
  scopes: [read:jira, write:jira]
  resource: {RESOURCE}
identity_check: {{tool: whoami, digest: "{who}", email: email}}
systems: [jira]
approved_by: fixture-admin
pull: "#7"
tools:
  - registry: jira.search
    upstream: jira_search_issues
    digest: "{search}"
    class: read
    approved_by: fixture-admin
    scopes: [read:jira]
    args: {{pass: [jql, limit]}}
    result: {{items: issues, id: key, title: summary, url: url, content: fields}}
  - registry: jira.create_issue
    upstream: jira_create_issue
    digest: "{create}"
    approved_by: fixture-admin
    scopes: [write:jira]
    args: {{pass: [issue_type, summary, description], rename: {{project: project_key, epic: parent_key}}, marker: description}}
    result: {{id: key, url: url}}
"##,
        who = d("whoami"),
        search = d("jira_search_issues"),
        create = d("jira_create_issue"),
    );
    pmos_contract::mcp::parse(&yaml).unwrap()
}

struct Setup {
    oauth: Arc<McpOAuth>,
    store: Arc<MemoryMcpCredentials>,
    auth: Arc<AuthServer>,
    fake: FakeMcp,
    jira: McpConnector,
    stores: Stores,
    audit: Arc<pmos_toolserver::audit::MemoryAudit>,
    _cancel: CancellationToken,
}

async fn setup_with(edit_metadata: impl FnOnce(&mut Value)) -> Setup {
    capture_logs();
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new().with_account("seed-token-not-used-0123456789", "nobody@example.com");
    let prm = Router::new().route(
        "/.well-known/oauth-protected-resource",
        get(|| async {
            Json(json!({"resource": RESOURCE, "authorization_servers": ["https://auth.example.test"]}))
        }),
    );
    let mcp_addr = fake::serve_with(fake.clone(), vec![], None, prm, cancel.clone())
        .await
        .unwrap();
    let auth = Arc::new(AuthServer::default());
    let mut md = metadata();
    edit_metadata(&mut md);
    *auth.metadata.lock().unwrap() = md;
    *auth.scope.lock().unwrap() = "read:jira write:jira".into();
    auth.expires_in.store(3600, Ordering::SeqCst);
    *auth.mcp.lock().unwrap() = Some(fake.clone());
    let as_addr = serve(
        Router::new()
            .route("/.well-known/oauth-authorization-server", get(as_metadata))
            .route("/token", post(as_token))
            .route("/register", post(as_register))
            .with_state(auth.clone()),
    )
    .await;
    let routes = TestRoutes::default()
        .route(MCP_HOST, mcp_addr)
        .route(AUTH_HOST, as_addr);
    let (stores, audit) = Stores::in_memory(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../content"
    )));
    let mut ana = member("ana", "U0ANA", vec![Role::Pm]);
    ana.email = Some("ana@example.com".into());
    let mut ben = member("ben", "U0BEN", vec![Role::Pm]);
    ben.email = Some("ben@example.com".into());
    let users = memory_users(vec![ana, ben], &stores).await;
    let people = Arc::new(People::new(users));
    let store = Arc::new(MemoryMcpCredentials::default());
    let keys = KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap();
    let oauth = Arc::new(
        McpOAuth::new(
            &[record()],
            keys,
            store.clone(),
            people,
            b"state-key-for-tests".to_vec(),
            "https://pmos.example.test",
            |_| None,
            routes.clone(),
            Some(stores.audit.clone()),
        )
        .unwrap(),
    );
    let server = McpServer::new(
        record(),
        ServerAuth::PerUser(oauth.clone()),
        Some(stores.audit.clone()),
        &routes,
    )
    .unwrap();
    let jira = McpConnector::new("jira", Arc::new(server), "mcp").unwrap();
    Setup {
        oauth,
        store,
        auth,
        fake,
        jira,
        stores,
        audit,
        _cancel: cancel,
    }
}

async fn setup() -> Setup {
    setup_with(|_| {}).await
}

/// The PM's browser: to the authorization server and back. Returns the code and state.
fn sign_in(s: &Setup, url: &str, email: &str) -> (String, String) {
    sign_in_at(&s.auth, url, email)
}

fn sign_in_at(auth: &AuthServer, url: &str, email: &str) -> (String, String) {
    let url = reqwest::Url::parse(url).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let code = format!("code-{}", auth.n.fetch_add(1, Ordering::SeqCst));
    auth.codes.lock().unwrap().insert(
        code.clone(),
        (q["code_challenge"].clone(), email.to_string()),
    );
    (code, q["state"].clone())
}

async fn link(s: &Setup, user: &str, email: &str) -> Result<String, McpOAuthError> {
    let url = s.oauth.authorize_url(user, "acme-jira", Utc::now()).await?;
    let (code, state) = sign_in(s, &url, email);
    s.oauth
        .complete("acme-jira", &code, &state, Utc::now())
        .await
}

// ----- the tests -------------------------------------------------------------------------------

#[tokio::test]
async fn mcp_12_the_request_asks_for_exactly_the_records_scopes_with_pkce_and_resource() {
    let s = setup().await;
    let url = s
        .oauth
        .authorize_url("ana", "acme-jira", Utc::now())
        .await
        .unwrap();
    let u = reqwest::Url::parse(&url).unwrap();
    assert_eq!(u.host_str(), Some(AUTH_HOST));
    assert_eq!(u.path(), "/authorize");
    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "pmos-test-client");
    assert_eq!(q["scope"], "read:jira write:jira");
    assert_eq!(q["resource"], RESOURCE);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(
        q["redirect_uri"],
        "https://pmos.example.test/oauth/mcp/acme-jira/callback"
    );
    assert!(
        !q.contains_key("code_verifier"),
        "the verifier stays in the tool server"
    );
    assert!(
        !s.auth.registered.load(Ordering::SeqCst),
        "no dynamic client registration"
    );
}

#[tokio::test]
async fn mcp_10_11_a_linked_pm_reads_and_writes_with_their_own_token() {
    let s = setup().await;
    assert_eq!(link(&s, "ana", "ana@example.com").await.unwrap(), "ana");
    assert_eq!(
        s.auth.requests.lock().unwrap()[0],
        ("authorization_code".to_string(), true)
    );
    let links = s.oauth.links("ana").await.unwrap();
    assert!(links[0].linked && links[0].missing_scopes.is_empty());
    assert_eq!(links[0].account.as_deref(), Some("ana@example.com"));
    let caller = Caller::new("ana", "r1");
    let hits = s
        .jira
        .read(caller, "search", &json!({"jql": "sso"}))
        .await
        .unwrap();
    assert_eq!(hits[0].source.id, "PM-101");
    s.jira
        .write(
            caller,
            "create_issue",
            &json!({"project": "PM", "issue_type": "Task", "summary": "T"}),
        )
        .await
        .unwrap();
    let callers = s.fake.callers();
    assert!(
        callers
            .iter()
            .all(|(_, who)| who.as_deref() == Some("ana@example.com")),
        "{callers:?}"
    );
    // Sealed at rest; bound to the user, the server and the field.
    let sealed = s.store.get("ana", "acme-jira").await.unwrap().unwrap();
    let issued = s.auth.issued.lock().unwrap().clone();
    for t in &issued {
        assert!(
            !sealed.access_token.contains(t) && !sealed.refresh_token.clone().unwrap().contains(t)
        );
    }
    s.store
        .put(&SealedMcp {
            user: "ben".into(),
            ..sealed
        })
        .await
        .unwrap();
    assert!(
        s.oauth.token("ben", "acme-jira").await.is_err(),
        "a token copied to another person's row doesn't unseal"
    );
}

#[tokio::test]
async fn mcp_11_another_persons_account_is_refused_and_nothing_is_sealed() {
    let s = setup().await;
    let err = link(&s, "ana", "ben@example.com").await.unwrap_err();
    assert_eq!(err, McpOAuthError::WrongAccount("ana".into()));
    assert!(s.store.get("ana", "acme-jira").await.unwrap().is_none());
    let link = s
        .audit
        .events()
        .into_iter()
        .find(|e| e.tool == "mcp.link")
        .unwrap();
    assert_eq!(link.outcome, pmos_toolserver::audit::Outcome::Denied);
    assert_eq!(link.user, "ana");
}

#[tokio::test]
async fn a_state_is_signed_single_use_and_expires() {
    let s = setup().await;
    let url = s
        .oauth
        .authorize_url("ana", "acme-jira", Utc::now())
        .await
        .unwrap();
    let (code, state) = sign_in(&s, &url, "ana@example.com");
    // Tampered.
    let forged = format!("{}x", state);
    assert_eq!(
        s.oauth
            .complete("acme-jira", &code, &forged, Utc::now())
            .await,
        Err(McpOAuthError::BadState)
    );
    // For another server.
    assert!(s
        .oauth
        .complete("other", &code, &state, Utc::now())
        .await
        .is_err());
    // Expired.
    let late = Utc::now() + Duration::minutes(11);
    assert_eq!(
        s.oauth.complete("acme-jira", &code, &state, late).await,
        Err(McpOAuthError::BadState)
    );
    // A fresh one works once.
    let url = s
        .oauth
        .authorize_url("ana", "acme-jira", Utc::now())
        .await
        .unwrap();
    let (code, state) = sign_in(&s, &url, "ana@example.com");
    s.oauth
        .complete("acme-jira", &code, &state, Utc::now())
        .await
        .unwrap();
    let (code2, _) = sign_in(&s, &url, "ana@example.com");
    assert_eq!(
        s.oauth
            .complete("acme-jira", &code2, &state, Utc::now())
            .await,
        Err(McpOAuthError::BadState)
    );
}

#[tokio::test]
async fn threat_7_each_call_uses_the_callers_own_token() {
    let s = setup().await;
    link(&s, "ben", "ben@example.com").await.unwrap();
    // Ana has linked nothing: Ben's token is never used for her.
    let err = s
        .jira
        .read(Caller::new("ana", "r1"), "search", &json!({"jql": "sso"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectorError::NotConnected(_)), "{err:?}");
    assert!(
        s.fake.callers().iter().all(|(t, _)| t == "whoami"),
        "only Ben's identity check reached the server"
    );
    // A team bot's write acts as the PM, with the PM's token.
    let reads = pmos_contract::bots::Reads {
        jira_projects: vec!["PM".into()],
        ..Default::default()
    };
    let caller = Caller::new("ben", "r1").for_team_bot(TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    s.jira
        .write(
            caller,
            "create_issue",
            &json!({"project": "PM", "issue_type": "Task", "summary": "T"}),
        )
        .await
        .unwrap();
    assert_eq!(
        s.fake.callers().last().unwrap().1.as_deref(),
        Some("ben@example.com")
    );
    // Its reads use its own account, which isn't linked.
    assert!(matches!(
        s.jira.read(caller, "search", &json!({"jql": "sso"})).await,
        Err(ConnectorError::NotConnected(_))
    ));
    // People not in the user database get nothing.
    assert!(s
        .oauth
        .authorize_url("eve", "acme-jira", Utc::now())
        .await
        .is_err());
}

#[tokio::test]
async fn threat_4_a_grant_missing_a_scope_makes_the_write_answer_reconnect() {
    let s = setup().await;
    *s.auth.scope.lock().unwrap() = "read:jira".into();
    link(&s, "ana", "ana@example.com").await.unwrap();
    assert_eq!(
        s.oauth.links("ana").await.unwrap()[0].missing_scopes,
        vec!["write:jira"]
    );
    let caller = Caller::new("ana", "r1");
    s.jira
        .read(caller, "search", &json!({"jql": "sso"}))
        .await
        .unwrap();
    let err = s
        .jira
        .write(
            caller,
            "create_issue",
            &json!({"project": "PM", "issue_type": "Task", "summary": "T"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ConnectorError::Reconnect(m) if m.contains("write:jira")),
        "{err:?}"
    );
    assert!(
        !s.fake
            .callers()
            .iter()
            .any(|(t, _)| t == "jira_create_issue"),
        "nothing sent"
    );
}

#[tokio::test]
async fn tokens_refresh_under_the_lock_and_only_a_refused_grant_unlinks() {
    let s = setup().await;
    s.auth.expires_in.store(0, Ordering::SeqCst);
    link(&s, "ana", "ana@example.com").await.unwrap();
    let caller = Caller::new("ana", "r1");
    // Expired at once: two calls at the same time refresh once.
    let (sso, scim) = (json!({"jql": "sso"}), json!({"jql": "scim"}));
    let (a, b) = tokio::join!(
        s.jira.read(caller, "search", &sso),
        s.jira.read(caller, "search", &scim)
    );
    a.unwrap();
    b.unwrap();
    let grants: Vec<String> = s
        .auth
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.0.clone())
        .collect();
    assert!(
        grants.iter().filter(|g| *g == "refresh_token").count() >= 1,
        "{grants:?}"
    );
    assert!(
        s.auth.requests.lock().unwrap().iter().all(|r| r.1),
        "resource on every request"
    );
    // The authorization server refuses the grant: unlinked, and the PM is asked to reconnect.
    s.auth.revoked.store(true, Ordering::SeqCst);
    let mut expired = s.store.get("ana", "acme-jira").await.unwrap().unwrap();
    let was = expired.updated_at;
    expired.expires_at = Some(Utc::now() - Duration::minutes(1));
    expired.updated_at = Utc::now();
    assert!(s.store.replace(&expired, was).await.unwrap());
    let err = s.oauth.token("ana", "acme-jira").await.unwrap_err();
    assert!(matches!(err, ConnectorError::Reconnect(_)), "{err:?}");
    assert!(s.store.get("ana", "acme-jira").await.unwrap().is_none());
}

#[tokio::test]
async fn threat_12_metadata_naming_another_host_refuses_the_link() {
    for (field, value) in [
        ("token_endpoint", "https://169.254.169.254/token"),
        (
            "authorization_endpoint",
            "https://evil.example.com/authorize",
        ),
        ("issuer", "https://other.example.test"),
    ] {
        let s = setup_with(|m| m[field] = json!(value)).await;
        let err = s
            .oauth
            .authorize_url("ana", "acme-jira", Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, McpOAuthError::Metadata(_)),
            "{field}: {err:?}"
        );
        assert!(
            s.auth.requests.lock().unwrap().is_empty(),
            "no token request"
        );
    }
    let s = setup_with(|m| m["code_challenge_methods_supported"] = json!(["plain"])).await;
    assert!(matches!(
        s.oauth.authorize_url("ana", "acme-jira", Utc::now()).await,
        Err(McpOAuthError::Metadata(_))
    ));
}

#[tokio::test]
async fn mcp_10_no_token_reaches_a_log_or_an_audit_event() {
    let s = setup().await;
    link(&s, "ana", "ana@example.com").await.unwrap();
    let caller = Caller::new("ana", "r1");
    s.jira
        .read(caller, "search", &json!({"jql": "sso"}))
        .await
        .unwrap();
    let _ = link(&s, "ben", "ana@example.com").await;
    tracing::info!("a line, so the capture is known to work");
    let logs = String::from_utf8(capture_logs().lock().unwrap().clone()).unwrap();
    assert!(logs.contains("a line, so the capture is known to work"));
    let events = serde_json::to_string(&s.audit.events()).unwrap();
    for t in s.auth.issued.lock().unwrap().iter() {
        assert!(!logs.contains(t), "a token in the logs");
        assert!(!events.contains(t), "a token in an audit event");
    }
    // What left PM OS: only the mapped arguments, no token.
    for (_, args) in s.fake.received() {
        for t in s.auth.issued.lock().unwrap().iter() {
            assert!(!args.to_string().contains(t));
        }
    }
}

#[tokio::test]
async fn an_admin_approves_the_record_and_each_tool() {
    let s = setup().await;
    let dir = s.oauth_dir().await;
    assert!(pmos_toolserver::mcp_client::check_approvals(&[record()], &dir).is_ok());
    let mut by_pm = record();
    by_pm.tools[1].approved_by = "ana".into();
    let errors = pmos_toolserver::mcp_client::check_approvals(&[by_pm], &dir).unwrap_err();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("jira.create_issue") && errors[0].contains("not an active Admin"));
    // And a third_party record is refused at start; any record but pmos-docs with fake data.
    let mut tp = record();
    tp.operator = pmos_contract::mcp::Operator::ThirdParty;
    assert_eq!(
        pmos_toolserver::mcp_client::startup_refusals(&[tp]).len(),
        1
    );
    assert_eq!(
        pmos_toolserver::mcp_client::fake_data_refusals(&[record()]).len(),
        1
    );
}

impl Setup {
    async fn oauth_dir(&self) -> pmos_toolserver::users::Directory {
        let mut ana = member("ana", "U0ANA", vec![Role::Pm]);
        ana.email = Some("ana@example.com".into());
        let users = memory_users(vec![ana], &self.stores).await;
        users.snapshot(Utc::now()).await.unwrap()
    }
}

#[tokio::test]
async fn the_postgres_store_keeps_sealed_tokens() {
    let Some(pg) = common::TestPg::new().await else {
        eprintln!("PMOS_TEST_DATABASE_URL not set: skipping the Postgres half");
        return;
    };
    let store = PgMcpCredentials::new(pg.store.pool().clone());
    let at = Utc::now().with_nanosecond_zero();
    let c = SealedMcp {
        user: "ana".into(),
        server: "acme-jira".into(),
        key_id: "k1".into(),
        access_token: "v1.k1.sealed".into(),
        refresh_token: Some("v1.k1.sealed2".into()),
        expires_at: Some(at),
        scopes: "read:jira".into(),
        account: Some("ana@example.com".into()),
        updated_at: at,
    };
    store.put(&c).await.unwrap();
    assert_eq!(
        store.get("ana", "acme-jira").await.unwrap(),
        Some(c.clone())
    );
    let next = SealedMcp {
        scopes: "read:jira write:jira".into(),
        updated_at: at + Duration::seconds(1),
        ..c.clone()
    };
    assert!(store.replace(&next, at).await.unwrap());
    assert!(!store.replace(&next, at).await.unwrap(), "stale");
    assert_eq!(store.list("ana").await.unwrap().len(), 1);
    assert!(store.delete("ana", "acme-jira").await.unwrap());
    assert!(store.get("ana", "acme-jira").await.unwrap().is_none());
    pg.drop_schema().await;
}

trait NanosZero {
    fn with_nanosecond_zero(self) -> Self;
}

impl NanosZero for chrono::DateTime<Utc> {
    fn with_nanosecond_zero(self) -> Self {
        use chrono::Timelike;
        self.with_nanosecond(0).unwrap()
    }
}

#[tokio::test]
async fn the_link_routes_start_finish_and_report_a_link() {
    const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
    let s = setup().await;
    let auth = s.auth.clone();
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../content"));
    let catalog = pmos_toolserver::catalog::Catalog::load(root, Utc::now().date_naive()).unwrap();
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(s.jira),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::confluence()),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::slack()),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::docs()),
    ];
    let svc = Arc::new(
        pmos_toolserver::ToolService::new(catalog, s.stores.clone(), connectors)
            .unwrap()
            .with_open_directory()
            .with_mcp_oauth(s.oauth.clone()),
    );
    let app = pmos_toolserver::router(
        svc,
        pmos_toolserver::AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            console_token: None,
            content_root: root.to_path_buf(),
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: None,
        },
        CancellationToken::new(),
    );
    let base = format!("http://{}", serve(app).await);
    let http = reqwest::Client::new();
    let start: Value = http
        .get(format!(
            "{base}/internal/mcp/oauth/acme-jira/start?user=ana"
        ))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = start["url"].as_str().unwrap().to_string();
    let anonymous = http
        .get(format!(
            "{base}/internal/mcp/oauth/acme-jira/start?user=ana"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let (code, state) = sign_in_at(&auth, &url, "ana@example.com");
    let back = http
        .get(format!("{base}/oauth/mcp/acme-jira/callback"))
        .query(&[("code", code.as_str()), ("state", state.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(back.status(), 200);
    let links: Value = http
        .get(format!("{base}/internal/mcp/links?user=ana"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(links["links"][0]["linked"], true);
    assert!(
        !links.to_string().contains("access-token"),
        "no token in the answer"
    );
}
