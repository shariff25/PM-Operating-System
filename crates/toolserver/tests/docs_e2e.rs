//! The support docs end to end: tool server `/internal` session and call, the docs
//! connector, and a real `pmos-docs` MCP server holding indexed pages.

use std::sync::Arc;

use chrono::Utc;
use pmos_docs::page::DocPage;
use pmos_docs::store::{DocsStore, MemoryStore};
use pmos_docs::sync::{SyncConfig, Syncer};
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::docs::Docs;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const DOCS_TOKEN: &str = "docs-token-for-tests-0123456789abcdef";
const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const BASE: &str = "https://support.acme.com";

fn page(slug: &str, title: &str, body: &str) -> DocPage {
    DocPage {
        url: format!("{BASE}/docs/{slug}.md"),
        slug: slug.into(),
        title: title.into(),
        section: "docs".into(),
        product: "dsm".into(),
        description: None,
        headings: String::new(),
        body: body.into(),
        updated: None,
        etag: None,
        fetched_at: Utc::now(),
    }
}

async fn serve(app: axum::Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, cancel)
}

async fn start_docs() -> (String, CancellationToken) {
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    store
        .upsert(&page(
            "dsm-saml-sso",
            "Configure SAML SSO for Acme DSM",
            "# Configure SAML SSO\n\nDSM supports SAML 2.0 single sign-on with Okta and Azure AD.",
        ))
        .await
        .unwrap();
    store
        .upsert(&page(
            "dsm-backup",
            "Backup and Restore",
            "Back up the cluster nightly.",
        ))
        .await
        .unwrap();
    let runtime = Arc::new(pmos_docs::app::Runtime::new(
        store.clone(),
        Syncer::new(SyncConfig::default(), store),
    ));
    let cancel = CancellationToken::new();
    let app = pmos_docs::app::router(
        runtime,
        pmos_docs::app::AppConfig {
            token: DOCS_TOKEN.into(),
            base: BASE.into(),
            allowed_hosts: vec!["127.0.0.1".into()],
        },
        cancel.clone(),
    );
    let (base, stop) = serve(app).await;
    (format!("{base}/mcp"), stop)
}

async fn start_toolserver(
    docs: Docs,
) -> (
    String,
    Arc<pmos_toolserver::audit::MemoryAudit>,
    CancellationToken,
) {
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let catalog = Catalog::load(&content, Utc::now().date_naive()).unwrap();
    let (stores, audit) = Stores::in_memory(&content);
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(docs),
    ];
    let svc = Arc::new(ToolService::new(catalog, stores, connectors).unwrap());
    let cancel = CancellationToken::new();
    let app = router(
        svc,
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            content_root: content,
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: None,
        },
        cancel.clone(),
    );
    let (base, stop) = serve(app).await;
    (base, audit, stop)
}

async fn session(http: &reqwest::Client, ts: &str, skill: &str) -> String {
    let res: Value = http
        .post(format!("{ts}/internal/sessions"))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"user": "ana", "skill": skill, "run_id": "slack-docs-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn docs_search_and_read_through_the_tool_server() {
    let (docs_url, stop_docs) = start_docs().await;
    let (ts, audit, stop_ts) = start_toolserver(Docs::new(docs_url, DOCS_TOKEN)).await;
    let http = reqwest::Client::new();
    let token = session(&http, &ts, "docs").await;

    let call = |tool: &'static str, args: Value| {
        let http = http.clone();
        let url = format!("{ts}/internal/call");
        let token = token.clone();
        async move {
            http.post(url)
                .bearer_auth(SERVICE_TOKEN)
                .header("x-pmos-session", token)
                .json(&json!({"tool": tool, "args": args}))
                .send()
                .await
                .unwrap()
        }
    };

    let res = call("docs_search", json!({"query": "SAML SSO", "limit": 3})).await;
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let first = &body["result"]["results"][0];
    assert_eq!(
        first["untrusted"], true,
        "wrapped like any external content"
    );
    assert_eq!(first["title"], "Configure SAML SSO for Acme DSM");
    assert_eq!(first["source"]["url"], format!("{BASE}/docs/dsm-saml-sso"));
    assert!(first["content"]["snippet"]
        .as_str()
        .unwrap()
        .contains("SAML"));

    let res = call(
        "docs_read",
        json!({"url": format!("{BASE}/docs/dsm-saml-sso")}),
    )
    .await;
    let body: Value = res.json().await.unwrap();
    assert!(body["result"]["results"][0]["content"]["markdown"]
        .as_str()
        .unwrap()
        .contains("Okta"));

    // A page outside the index is a readable not-found, not a fetch.
    let res = call("docs_read", json!({"url": format!("{BASE}/docs/nope")})).await;
    assert_eq!(res.status(), 404);

    // The docs skill can't reach anything it didn't declare.
    let res = call(
        "jira_create_issue",
        json!({"project": "PM", "issue_type": "Task", "summary": "x"}),
    )
    .await;
    assert_eq!(res.status(), 403);
    let res = call("jira_search", json!({"jql": "x"})).await;
    assert_eq!(res.status(), 403);

    // Internal calls need the service token, and a real session.
    let res = http
        .post(format!("{ts}/internal/call"))
        .header("x-pmos-session", &token)
        .json(&json!({"tool": "docs_search", "args": {"query": "x"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let res = http
        .post(format!("{ts}/internal/call"))
        .bearer_auth(SERVICE_TOKEN)
        .header("x-pmos-session", "pmos_s_nope")
        .json(&json!({"tool": "docs_search", "args": {"query": "x"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let events = audit.events();
    let tools: Vec<_> = events
        .iter()
        .map(|e| (e.tool.as_str(), e.outcome))
        .collect();
    use pmos_toolserver::audit::Outcome::*;
    assert_eq!(
        tools,
        vec![
            ("docs.search", Ok),
            ("docs.read", Ok),
            ("docs.read", Error),
            ("jira.create_issue", Denied),
            ("jira.search", Denied),
        ]
    );
    // The audit names the page read (its indexed `.md` URL), not its content.
    assert_eq!(
        events[0].sources,
        vec![format!("docs:{BASE}/docs/dsm-saml-sso.md")]
    );

    stop_ts.cancel();
    stop_docs.cancel();
}

#[tokio::test]
async fn a_wrong_docs_token_is_reported_not_hidden() {
    let (docs_url, stop_docs) = start_docs().await;
    let (ts, _audit, stop_ts) = start_toolserver(Docs::new(docs_url, "wrong-token")).await;
    let http = reqwest::Client::new();
    let token = session(&http, &ts, "docs").await;
    let res = http
        .post(format!("{ts}/internal/call"))
        .bearer_auth(SERVICE_TOKEN)
        .header("x-pmos-session", &token)
        .json(&json!({"tool": "docs_search", "args": {"query": "SAML"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("support docs"));
    stop_ts.cancel();
    stop_docs.cancel();
}

#[tokio::test]
async fn the_bot_token_is_limited_to_its_own_skills() {
    let (docs_url, stop_docs) = start_docs().await;
    let (ts, audit, stop_ts) = start_toolserver(Docs::new(docs_url, DOCS_TOKEN)).await;
    let http = reqwest::Client::new();
    let new_session = |token: &'static str, skill: &'static str| {
        http.post(format!("{ts}/internal/sessions"))
            .bearer_auth(token)
            .json(&json!({"user": "ana", "skill": skill, "run_id": "r1"}))
            .send()
    };

    // The bot may start `docs` sessions and search in them; the call is audited as the bot.
    let res = new_session(BOT_TOKEN, "docs").await.unwrap();
    assert_eq!(res.status(), 201);
    let docs_token = res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let res = http
        .post(format!("{ts}/internal/call"))
        .bearer_auth(BOT_TOKEN)
        .header("x-pmos-session", &docs_token)
        .json(&json!({"tool": "docs_search", "args": {"query": "SAML"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        audit.events().last().unwrap().client,
        pmos_toolserver::audit::Client::Bot
    );

    // But not sessions for other skills, ...
    let res = new_session(BOT_TOKEN, "prd-draft").await.unwrap();
    assert_eq!(res.status(), 403);
    // ... nor a session someone else started for another skill, ...
    let res = new_session(SERVICE_TOKEN, "ask").await.unwrap();
    let ask_token = res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let res = http
        .post(format!("{ts}/internal/call"))
        .bearer_auth(BOT_TOKEN)
        .header("x-pmos-session", &ask_token)
        .json(&json!({"tool": "context_search", "args": {"query": "x"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    // ... nor approvals, grants, linking or reindexing.
    for (method, path) in [
        ("GET", "/internal/approvals?user=ana"),
        ("POST", "/internal/approvals/apr_x/decision"),
        ("POST", "/internal/grants"),
        ("POST", "/internal/reindex"),
        ("GET", "/internal/credentials?user=ana"),
    ] {
        let res = http
            .request(method.parse().unwrap(), format!("{ts}{path}"))
            .bearer_auth(BOT_TOKEN)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 403, "{method} {path}");
    }
    // The session token is never accepted in place of a service token.
    let res = http
        .get(format!("{ts}/internal/approvals?user=ana"))
        .bearer_auth(&docs_token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    stop_ts.cancel();
    stop_docs.cancel();
}
