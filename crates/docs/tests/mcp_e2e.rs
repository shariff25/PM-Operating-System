//! `pmos-docs` over HTTP with a real MCP client, after a sync from the mock portal.

mod common;

use std::sync::Arc;

use common::{products, seed, site, start_portal, sync_config};
use pmos_docs::app::{router, AppConfig, Runtime};
use pmos_docs::page::{Products, Site};
use pmos_docs::store::{DocsStore, MemoryStore};
use pmos_docs::sync::Syncer;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "docs-test-token-0123456789abcdef0123";

/// A running `pmos-docs`: its address and a cancel token that stops it.
async fn serve(
    runtime: Arc<Runtime>,
    site: Option<Site>,
    products: Products,
) -> (String, CancellationToken) {
    let cancel = CancellationToken::new();
    let app = router(
        runtime,
        AppConfig {
            token: TOKEN.into(),
            name: "the Acme docs".into(),
            site,
            products,
            allowed_hosts: vec!["127.0.0.1".into()],
            stale_after: Some(std::time::Duration::from_secs(3600)),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, cancel)
}

async fn connect(base: &str) -> RunningService<RoleClient, ClientConfig> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")).auth_header(TOKEN),
    );
    ClientConfig::default().serve(transport).await.unwrap()
}

async fn call(
    client: &RunningService<RoleClient, ClientConfig>,
    name: &'static str,
    args: Value,
) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn search_and_read_over_mcp() {
    let (portal_base, portal, stop_portal) = start_portal().await;
    seed(&portal_base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let syncer = Syncer::new(sync_config(&portal_base), store.clone());
    let runtime = Arc::new(Runtime::new(store.clone(), syncer));
    assert!(runtime.sync_once().await);

    let (base, cancel) = serve(runtime.clone(), Some(site(&portal_base)), products()).await;
    let http = reqwest::Client::new();

    // Wrong or missing token: refused.
    let res = http
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(
        http.get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // Ready: the index answers, has pages, and was synced within the hour.
    let ready = http.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(ready.status(), 200);
    assert_eq!(ready.json::<Value>().await.unwrap()["ready"], true);

    let client = connect(&base).await;
    let tools = client.list_tools(Default::default()).await.unwrap().tools;
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(names, vec!["search_docs", "get_doc", "list_products"]);
    // The configured name and products, nothing built in.
    let search = &tools[0];
    assert!(
        search
            .description
            .as_deref()
            .unwrap()
            .starts_with(&format!("Search the Acme docs ({portal_base})")),
        "{:?}",
        search.description
    );
    assert_eq!(
        search.input_schema["properties"]["product"]["enum"],
        json!(["vault", "cloud", "general"])
    );
    let info = client.peer_info().unwrap();
    assert!(info
        .instructions
        .as_deref()
        .unwrap()
        .contains("the Acme docs"));

    let found = call(
        &client,
        "search_docs",
        json!({"query": "key rotation", "product": "vault", "limit": 3}),
    )
    .await;
    let out = found.structured_content.unwrap();
    assert_eq!(out["results"][0]["title"], "Acme Vault Key Rotation");
    assert_eq!(
        out["results"][0]["url"],
        format!("{portal_base}/docs/vault-key-rotation")
    );
    assert_eq!(out["results"][0]["product"], "vault");
    assert!(out.get("note").is_none());

    // The product filter narrows results; an unknown product is refused, naming the ids.
    let cloud = call(
        &client,
        "search_docs",
        json!({"query": "key rotation enclaves", "product": "cloud"}),
    )
    .await
    .structured_content
    .unwrap();
    assert_eq!(cloud["count"], 1);
    assert_eq!(cloud["results"][0]["product"], "cloud");
    let unknown = call(
        &client,
        "search_docs",
        json!({"query": "x", "product": "spaceship"}),
    )
    .await;
    assert_eq!(unknown.is_error, Some(true));
    let failure = unknown.structured_content.unwrap();
    assert_eq!(failure["code"], "bad_args");
    assert!(
        failure["message"]
            .as_str()
            .unwrap()
            .contains("vault, cloud, general"),
        "{failure}"
    );

    // Read by the page link a person would paste.
    let before = portal.total_hits();
    let doc = call(
        &client,
        "get_doc",
        json!({"url": format!("{portal_base}/docs/vault-backup")}),
    )
    .await;
    let doc = doc.structured_content.unwrap();
    assert!(doc["markdown"]
        .as_str()
        .unwrap()
        .contains("Back up the cluster"));
    assert_eq!(doc["truncated"], false);
    assert_eq!(
        portal.total_hits(),
        before,
        "served from the index, no fetch"
    );

    // Anything outside the index is refused, and never fetched.
    for url in [
        format!("{portal_base}/docs/not-indexed"),
        format!("{portal_base}/blog/vault-backup"),
        "https://evil.example/docs/vault-backup.md".to_string(),
        format!("{portal_base}/docs/../admin"),
        "http://169.254.169.254/latest/meta-data".to_string(),
    ] {
        let r = call(&client, "get_doc", json!({ "url": url })).await;
        assert_eq!(r.is_error, Some(true), "{url}");
    }
    let outside = call(&client, "get_doc", json!({"url": "https://evil.example/x"}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(
        outside["message"],
        format!("only pages of the Acme docs under {portal_base}/docs/ can be read")
    );
    assert_eq!(portal.total_hits(), before);

    let bad = call(&client, "search_docs", json!({"query": "x", "limit": 99})).await;
    assert_eq!(bad.is_error, Some(true));
    let bad = call(&client, "search_docs", json!({"query": "x", "sql": "drop"})).await;
    assert_eq!(bad.is_error, Some(true));

    let products = call(&client, "list_products", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(
        products["products"],
        json!([
            {"product": "vault", "name": "Acme Vault", "pages": 2},
            {"product": "cloud", "name": "Acme Cloud", "pages": 1},
            {"product": "general", "name": "general", "pages": 1},
        ])
    );

    // Status and a manual sync, behind the same token.
    let status: Value = http
        .get(format!("{base}/admin/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["pages"], 4);
    assert_eq!(status["configured"], true);
    assert_eq!(status["last_sync"]["fetched"], 4);
    assert_eq!(
        http.post(format!("{base}/admin/sync"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let started = http
        .post(format!("{base}/admin/sync"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), 202);

    let _ = client.cancel().await;
    cancel.cancel();
    stop_portal.cancel();
}

#[tokio::test]
async fn with_no_docs_site_the_server_starts_and_says_so() {
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let runtime = Arc::new(Runtime::unconfigured(store.clone()));
    assert!(!runtime.configured());
    assert!(
        !runtime.due(None, chrono::Utc::now()).await,
        "nothing to sync"
    );
    assert!(!runtime.sync_once().await);
    let (base, cancel) = serve(runtime, None, Products::default()).await;
    let http = reqwest::Client::new();

    let ready = http.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(ready.status(), 503);
    let ready: Value = ready.json().await.unwrap();
    assert_eq!(ready["ready"], false);
    assert_eq!(ready["detail"], "docs site not configured");

    let sync = http
        .post(format!("{base}/admin/sync"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(sync.status(), 503);
    let sync: Value = sync.json().await.unwrap();
    assert!(sync["error"]
        .as_str()
        .unwrap()
        .contains("PMOS_DOCS_BASE_URL"));
    let status: Value = http
        .get(format!("{base}/admin/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["configured"], false);
    assert_eq!(status["pages"], 0);

    let client = connect(&base).await;
    let tools = client.list_tools(Default::default()).await.unwrap().tools;
    assert!(tools[0]
        .description
        .as_deref()
        .unwrap()
        .contains("no docs site is configured"));
    assert_eq!(
        tools[0].input_schema["properties"]["product"]["enum"],
        json!(["general"])
    );

    let found = call(&client, "search_docs", json!({"query": "key rotation"})).await;
    assert_ne!(found.is_error, Some(true), "a plain answer, not an error");
    assert_eq!(
        found.structured_content.unwrap(),
        json!({
            "count": 0,
            "results": [],
            "note": "No docs site is configured; ask a platform owner to set PMOS_DOCS_BASE_URL.",
        })
    );
    // Arguments are still checked.
    let bad = call(
        &client,
        "search_docs",
        json!({"query": "x", "product": "vault"}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));

    let read = call(
        &client,
        "get_doc",
        json!({"url": "https://docs.example.com/docs/x"}),
    )
    .await;
    assert_eq!(read.is_error, Some(true));
    assert_eq!(read.structured_content.unwrap()["code"], "not_configured");

    let products = call(&client, "list_products", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(
        products["products"],
        json!([{"product": "general", "name": "general", "pages": 0}])
    );
    assert!(products["note"].as_str().unwrap().contains("No docs site"));

    let _ = client.cancel().await;
    cancel.cancel();
}
