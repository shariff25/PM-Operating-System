//! `pmos-docs` over HTTP with a real MCP client, after a sync from the mock portal.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{seed, start_portal};
use pmos_docs::app::{router, AppConfig, Runtime};
use pmos_docs::store::{DocsStore, MemoryStore};
use pmos_docs::sync::{SyncConfig, Syncer};
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "docs-test-token-0123456789abcdef0123";

#[tokio::test]
async fn search_and_read_over_mcp() {
    let (portal_base, portal, stop_portal) = start_portal().await;
    seed(&portal_base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let syncer = Syncer::new(
        SyncConfig {
            base: portal_base.clone(),
            delay: Duration::from_millis(1),
            ..SyncConfig::default()
        },
        store.clone(),
    );
    let runtime = Arc::new(Runtime::new(store.clone(), syncer));
    assert!(runtime.sync_once().await);

    let cancel = CancellationToken::new();
    let app = router(
        runtime.clone(),
        AppConfig {
            token: TOKEN.into(),
            base: portal_base.clone(),
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

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")).auth_header(TOKEN),
    );
    let client = ClientConfig::default().serve(transport).await.unwrap();
    let names: Vec<_> = client
        .list_tools(Default::default())
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names, vec!["search_docs", "get_doc", "list_products"]);

    let call = |name: &'static str, args: Value| {
        let client = &client;
        async move {
            client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args.as_object().unwrap().clone()),
                )
                .await
                .unwrap()
        }
    };

    let found = call(
        "search_docs",
        json!({"query": "key rotation", "product": "dsm", "limit": 3}),
    )
    .await;
    let out = found.structured_content.unwrap();
    assert_eq!(out["results"][0]["title"], "Fortanix DSM Key Rotation");
    assert_eq!(
        out["results"][0]["url"],
        format!("{portal_base}/docs/dsm-key-rotation")
    );
    assert_eq!(out["results"][0]["product"], "dsm");

    // Read by the page link a person would paste.
    let before = portal.total_hits();
    let doc = call(
        "get_doc",
        json!({"url": format!("{portal_base}/docs/dsm-backup")}),
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
        "https://evil.example/docs/dsm-backup.md".to_string(),
        format!("{portal_base}/docs/../admin"),
        "http://169.254.169.254/latest/meta-data".to_string(),
    ] {
        let r = call("get_doc", json!({ "url": url })).await;
        assert_eq!(r.is_error, Some(true), "{url}");
    }
    assert_eq!(portal.total_hits(), before);

    let bad = call("search_docs", json!({"query": "x", "limit": 99})).await;
    assert_eq!(bad.is_error, Some(true));
    let bad = call("search_docs", json!({"query": "x", "sql": "drop"})).await;
    assert_eq!(bad.is_error, Some(true));

    let products = call("list_products", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(products["products"].as_array().unwrap().len(), 3);

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
    let _ = Utc::now();
}
