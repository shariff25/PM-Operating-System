//! Slice M3: the public docs on the generic MCP client layer (`docs/mcp-servers-spec.md` §6).
//!
//! A real `pmos-docs` server (in this process, on loopback) is reached two ways: by the
//! built-in docs client (`connectors::docs::Docs`, still the default) and through a `pmos-docs`
//! server record (`operator: pmos`, `service_token`, internal URL at a pinned address, public
//! data). Both return the same items and the same kinds of error, and the record's digests
//! must match that deployment's own tool definitions: `pmos-docs` names its site in its tool
//! descriptions, so each deployment's Admin approves its own digests.

use std::sync::Arc;

use chrono::Utc;
use pmos_docs::page::{DocPage, Products, Site};
use pmos_docs::store::{DocsStore, MemoryStore};
use pmos_docs::sync::{SyncConfig, Syncer};
use pmos_toolserver::connectors::docs::Docs;
use pmos_toolserver::connectors::{untrusted, Caller, Connector, ConnectorError, Item};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth};
use pmos_toolserver::vault::Secret;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const DOCS_TOKEN: &str = "docs-token-for-mcp-tests-0123456789abcdef";
const BASE: &str = "https://docs.example.com";
const NAME: &str = "the Acme docs";

fn page(slug: &str, title: &str, body: &str) -> DocPage {
    DocPage {
        url: format!("{BASE}/docs/{slug}.md"),
        slug: slug.into(),
        title: title.into(),
        section: "docs".into(),
        product: "vault".into(),
        description: None,
        headings: String::new(),
        body: body.into(),
        updated: None,
        etag: None,
        fetched_at: Utc::now(),
    }
}

fn products() -> Products {
    Products::parse("products:\n  - id: vault\n    name: Acme Vault\n    keywords: [vault]\n")
        .unwrap()
}

/// A `pmos-docs` at `127.0.0.1`, answering to `docs.pmos.internal` too; its URL and the
/// digests of its tools.
async fn start_docs(
    configured: bool,
) -> (
    std::net::SocketAddr,
    Vec<(String, String)>,
    CancellationToken,
) {
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    store
        .upsert(&page(
            "vault-saml-sso",
            "Configure SAML SSO for Acme Vault",
            "# Configure SAML SSO\n\nAcme Vault supports SAML 2.0 single sign-on with Okta and Azure AD.",
        ))
        .await
        .unwrap();
    store
        .upsert(&page(
            "vault-backup",
            "Backup and Restore",
            "Back up the cluster nightly.",
        ))
        .await
        .unwrap();
    let site = Site::new(BASE).with_prefixes(&["/docs/"]);
    let digests = pmos_docs::mcp::DocsMcp::new(
        store.clone(),
        NAME,
        configured.then(|| site.clone()),
        products(),
    )
    .tools()
    .iter()
    .map(|t| {
        (
            t.name.to_string(),
            pmos_toolserver::mcp_client::digest::digest(t),
        )
    })
    .collect();
    let runtime = Arc::new(if configured {
        pmos_docs::app::Runtime::new(
            store.clone(),
            Syncer::new(SyncConfig::new(site.clone()), store),
        )
    } else {
        pmos_docs::app::Runtime::unconfigured(Arc::new(MemoryStore::default()))
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let app = pmos_docs::app::router(
        runtime,
        pmos_docs::app::AppConfig {
            token: DOCS_TOKEN.into(),
            name: NAME.into(),
            site: configured.then_some(site),
            products: products(),
            allowed_hosts: vec![
                "127.0.0.1".into(),
                "docs.pmos.internal".into(),
                format!("docs.pmos.internal:{}", addr.port()),
            ],
            stale_after: None,
        },
        cancel.clone(),
    );
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (addr, digests, cancel)
}

/// The deployment's `pmos-docs` record, as an Admin would approve it.
fn record(
    addr: std::net::SocketAddr,
    digests: &[(String, String)],
) -> pmos_contract::mcp::ServerRecord {
    let d = |name: &str| digests.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let errors = "{code: code, message: message, codes: {not_found: not_found, not_configured: not_configured, unavailable: unavailable}, default: bad_args}";
    let yaml = format!(
        r##"id: pmos-docs
operator: pmos
transport: streamable_http
url: http://docs.pmos.internal:{port}/mcp
identity: {{address: "127.0.0.1"}}
auth: service_token
secret_env: PMOS_DOCS_TOKEN
data_class: public
systems: [docs]
approved_by: ana
pull: "#41"
tools:
  - registry: docs.search
    upstream: search_docs
    digest: "{search}"
    class: read
    approved_by: ana
    args: {{pass: [query, product, limit]}}
    result: {{items: results, id: source_url, url: url, title: title, empty_note: note, content: {{product: product, section: section, updated: updated, snippet: snippet}}}}
    errors: {errors}
  - registry: docs.read
    upstream: get_doc
    digest: "{read}"
    class: read
    approved_by: ana
    args: {{pass: [url]}}
    result: {{id: source_url, url: url, title: title, content: {{product: product, updated: updated, markdown: markdown, truncated: truncated}}}}
    errors: {errors}
"##,
        port = addr.port(),
        search = d("search_docs"),
        read = d("get_doc"),
    );
    pmos_contract::mcp::parse(&yaml).unwrap()
}

fn mcp_docs(record: pmos_contract::mcp::ServerRecord) -> McpConnector {
    let server = McpServer::new(
        record,
        ServerAuth::Token(Secret::new(DOCS_TOKEN)),
        None,
        &TestRoutes::default(),
    )
    .unwrap();
    McpConnector::new("docs", Arc::new(server), "mcp").unwrap()
}

fn kind(e: &ConnectorError) -> String {
    format!("{e:?}").split('(').next().unwrap().to_string()
}

fn seen(r: Result<Vec<Item>, ConnectorError>) -> Result<Vec<Value>, String> {
    r.map(|items| items.iter().map(untrusted).collect())
        .map_err(|e| kind(&e))
}

#[tokio::test]
async fn the_docs_record_serves_what_the_built_in_client_serves() {
    let (addr, digests, _stop) = start_docs(true).await;
    let builtin = Docs::new(format!("http://127.0.0.1:{}/mcp", addr.port()), DOCS_TOKEN);
    let record = record(addr, &digests);
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../content/tools.yaml"
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        pmos_contract::mcp::validate(&record, "pmos-docs", &registry),
        vec![]
    );
    assert!(
        pmos_toolserver::mcp_client::fake_data_refusals(std::slice::from_ref(&record)).is_empty(),
        "pmos-docs is allowed with fake data"
    );
    let mcp = mcp_docs(record);
    assert_eq!(mcp.data_class(), Some(pmos_contract::Sensitivity::Public));
    let caller = Caller::new("ana", "r1");
    let calls = [
        ("search", json!({"query": "SAML"})),
        (
            "search",
            json!({"query": "SAML", "product": "vault", "limit": 1}),
        ),
        ("search", json!({"query": "nothing-matches-this"})),
        ("search", json!({"query": "backup", "product": "nope"})),
        (
            "read",
            json!({"url": format!("{BASE}/docs/vault-saml-sso")}),
        ),
        (
            "read",
            json!({"url": format!("{BASE}/docs/vault-backup.md")}),
        ),
        ("read", json!({"url": format!("{BASE}/docs/missing")})),
        ("read", json!({"url": "https://elsewhere.example.com/x"})),
    ];
    for (action, args) in calls {
        let want = seen(builtin.read(caller, action, &args).await);
        let got = seen(mcp.read(caller, action, &args).await);
        assert_eq!(got, want, "docs.{action} {args}");
    }
    assert!(mcp.ready().await.is_ok());
}

#[tokio::test]
async fn an_unconfigured_docs_site_says_so_on_both() {
    let (addr, digests, _stop) = start_docs(false).await;
    let builtin = Docs::new(format!("http://127.0.0.1:{}/mcp", addr.port()), DOCS_TOKEN);
    let mcp = mcp_docs(record(addr, &digests));
    let caller = Caller::new("ana", "r1");
    let args = json!({"query": "SAML"});
    let want = builtin.read(caller, "search", &args).await;
    let got = mcp.read(caller, "search", &args).await;
    assert!(
        matches!(want, Err(ConnectorError::NotConfigured(_))),
        "{want:?}"
    );
    assert!(
        matches!(got, Err(ConnectorError::NotConfigured(_))),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_docs_site_configured_after_approval_needs_its_digests_approved_again() {
    // Approved while unconfigured; the server now names its site in its descriptions.
    let (addr, _, _stop) = start_docs(true).await;
    let (_, unconfigured, _stop2) = start_docs(false).await;
    let mcp = mcp_docs(record(addr, &unconfigured));
    let err = mcp
        .read(
            Caller::new("ana", "r1"),
            "search",
            &json!({"query": "SAML"}),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("tool_changed: pmos-docs.search_docs"),
        "{err}"
    );
}

#[tokio::test]
async fn the_docs_token_reaches_only_its_own_server() {
    // A wrong token: refused by pmos-docs, so nothing is served.
    let (addr, digests, _stop) = start_docs(true).await;
    let server = McpServer::new(
        record(addr, &digests),
        ServerAuth::Token(Secret::new("wrong-token-0123456789abcdef0123456789")),
        None,
        &TestRoutes::default(),
    )
    .unwrap();
    let mcp = McpConnector::new("docs", Arc::new(server), "mcp").unwrap();
    assert!(mcp
        .read(
            Caller::new("ana", "r1"),
            "search",
            &json!({"query": "SAML"})
        )
        .await
        .is_err());
    assert!(mcp.ready().await.is_err());
}
