//! The parity test (`docs/mcp-servers-spec.md` §4, slice M2): for every read the fixtures can
//! answer, the in-process fakes and the same fixtures served by `pmos-fake-mcp` through the MCP
//! client return the same wrapped items (so the same audit `sources`) and the same errors (by
//! kind: an MCP server's error text is marked as the server's). Jira's one write returns the same
//! reference. No model, no network beyond loopback.

use std::sync::Arc;

use pmos_contract::bots::Reads;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::{untrusted, Caller, Connector, ConnectorError, Item, TeamBot};
use pmos_toolserver::mcp_client::fake::{self, FakeMcp, FAKE_HOST};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth};
use pmos_toolserver::vault::Secret;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fake-mcp-token-for-parity-0123456789abcd";

fn kind(e: &ConnectorError) -> &'static str {
    match e {
        ConnectorError::NotFound(_) => "not_found",
        ConnectorError::BadArgs(_) => "bad_args",
        ConnectorError::Unsupported(_) => "unsupported",
        ConnectorError::Unavailable(_) => "unavailable",
        ConnectorError::NotConnected(_) => "not_connected",
        ConnectorError::Reconnect(_) => "reconnect",
        ConnectorError::Forbidden(_) => "forbidden",
        ConnectorError::RateLimited(_) => "rate_limited",
        ConnectorError::NotConfigured(_) => "not_configured",
    }
}

/// What the model and the audit log see of a read: the wrapped items and their `sources`, or
/// the error's kind.
fn seen(r: Result<Vec<Item>, ConnectorError>) -> Result<(Vec<Value>, Vec<String>), &'static str> {
    r.map(|items| {
        (
            items.iter().map(untrusted).collect(),
            items
                .iter()
                .map(|i| format!("{}:{}", i.source.system, i.source.id))
                .collect(),
        )
    })
    .map_err(|e| kind(&e))
}

struct Backends {
    fakes: Vec<Arc<dyn Connector>>,
    mcp: Vec<Arc<dyn Connector>>,
    _cancel: CancellationToken,
}

async fn backends() -> Backends {
    let cancel = CancellationToken::new();
    let addr = fake::serve(
        FakeMcp::new(),
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    let mcp = fake::fake_records(addr)
        .into_iter()
        .map(|r| {
            let system = r.systems[0].clone();
            let server = Arc::new(
                McpServer::new(
                    r,
                    ServerAuth::Token(Secret::new(TOKEN)),
                    None,
                    &TestRoutes::default(),
                )
                .unwrap()
                // Hundreds of calls from one test user: no rate cap here.
                .with_limits(pmos_toolserver::mcp_client::server::Limits {
                    per_user_per_minute: 10_000,
                    ..Default::default()
                }),
            );
            Arc::new(McpConnector::new(&system, server, "fake-mcp").unwrap()) as Arc<dyn Connector>
        })
        .collect();
    Backends {
        fakes: vec![
            Arc::new(FakeConnector::jira()),
            Arc::new(FakeConnector::confluence()),
            Arc::new(FakeConnector::slack()),
        ],
        mcp,
        _cancel: cancel,
    }
}

/// Every fixture-backed read, by system: (action, arguments).
fn reads() -> Vec<(&'static str, &'static str, Value)> {
    let mut out = Vec::new();
    for jql in [
        "sso",
        "SCIM",
        "export",
        "story epic",
        "nothing-matches-this",
        "sso ORDER BY key",
        "PM-102",
    ] {
        out.push(("jira", "search", json!({"jql": jql})));
        out.push(("jira", "search", json!({"jql": jql, "limit": 1})));
    }
    for key in ["PM-101", "PM-102", "PM-103", "PM-999"] {
        out.push(("jira", "read", json!({"key": key})));
    }
    for group in ["project", "epic", "status", "assignee", "fix_version"] {
        out.push((
            "jira",
            "rollup",
            json!({"jql": "project in (PM, CLD)", "group_by": group}),
        ));
    }
    out.push(("jira", "rollup", json!({"jql": "project = PM"})));
    out.push((
        "jira",
        "rollup",
        json!({"jql": "project = PM", "max_issues": 2}),
    ));
    out.push((
        "jira",
        "rollup",
        json!({"jql": "x", "group_by": "nonsense"}),
    ));
    for cql in ["sso", "PRD", "customer call", "nothing-matches-this"] {
        out.push(("confluence", "search", json!({"cql": cql})));
    }
    for page in ["9001", "9002", "1234"] {
        out.push(("confluence", "read", json!({"page_id": page})));
    }
    for q in ["<@U0EVALS>", "SAML", "SCIM", "nothing-matches-this"] {
        out.push(("slack", "search", json!({"query": q})));
        out.push(("slack", "search", json!({"query": q, "limit": 2})));
    }
    for (channel, ts) in [
        ("C0FIELD", "1731500000.000100"),
        ("C0PMTEAM", "1731600300.000200"),
        ("C0PMTEAM", "1731600360.000300"),
        ("C0GLOBEXCX", "1731601800.000800"),
        ("C0PMTEAM", "1999999999.000000"),
    ] {
        out.push((
            "slack",
            "read_thread",
            json!({"channel": channel, "ts": ts}),
        ));
    }
    out
}

async fn check(b: &Backends, caller: Caller<'_>, who: &str) -> usize {
    let mut checked = 0;
    for (system, action, args) in reads() {
        let fake = b.fakes.iter().find(|c| c.system() == system).unwrap();
        let mcp = b.mcp.iter().find(|c| c.system() == system).unwrap();
        let want = seen(fake.read(caller, action, &args).await);
        let got = seen(mcp.read(caller, action, &args).await);
        assert_eq!(got, want, "{who}: {system}.{action} {args}");
        checked += 1;
    }
    checked
}

#[tokio::test]
async fn every_fixture_read_is_the_same_through_the_mcp_client() {
    let b = backends().await;
    let n = check(&b, Caller::new("evals", "r1"), "a PM").await;
    assert!(n >= 40, "{n}");
}

#[tokio::test]
async fn team_bots_scopes_give_the_same_answers() {
    let b = backends().await;
    for (name, reads) in [
        (
            "scoped",
            Reads {
                jira_projects: vec!["PM".into()],
                confluence_spaces: vec!["PM".into()],
                slack_channels: vec!["C0FIELD".into(), "C0PMTEAM".into()],
            },
        ),
        (
            "other",
            Reads {
                jira_projects: vec!["CLD".into()],
                confluence_spaces: vec!["ENG".into()],
                slack_channels: vec!["C0GLOBEXCX".into()],
            },
        ),
        ("blind", Reads::default()),
    ] {
        let identity = format!("bots.{name}");
        let caller = Caller::new("evals", "r1").for_team_bot(TeamBot {
            name,
            identity: &identity,
            reads: &reads,
        });
        check(&b, caller, name).await;
    }
}

#[tokio::test]
async fn jiras_write_returns_the_same_reference() {
    let b = backends().await;
    let caller = Caller::new("evals", "r1");
    let args = json!({"project": "PM", "issue_type": "Story", "summary": "SCIM", "description": "D", "epic": "PM-101"});
    let fake = b.fakes[0]
        .write(caller, "create_issue", &args)
        .await
        .unwrap();
    let mcp = b.mcp[0].write(caller, "create_issue", &args).await.unwrap();
    assert_eq!(mcp, fake);
    // Writes the record doesn't map stay on the built-in connector.
    assert!(matches!(
        b.mcp[1]
            .write(
                caller,
                "create_draft",
                &json!({"space": "PM", "title": "T", "body": "B"})
            )
            .await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(matches!(
        b.mcp[2].audience(caller, "C0FIELD").await,
        Err(ConnectorError::Unsupported(_))
    ));
}

#[tokio::test]
async fn the_fake_mcp_mode_serves_jira_through_the_tool_server_as_the_fakes_do() {
    // `PMOS_CONNECTORS=fake` with `PMOS_MCP_SYSTEMS=jira`, as `main` builds it.
    let cancel = CancellationToken::new();
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../content"));
    let (stores, audit) = pmos_toolserver::Stores::in_memory(root);
    let (jira, fake) =
        pmos_toolserver::mcp_client::start_fake(Some(stores.audit.clone()), cancel.clone())
            .await
            .unwrap();
    assert_eq!(jira[0].mode(), "fake-mcp");
    let catalog =
        pmos_toolserver::catalog::Catalog::load(root, chrono::Utc::now().date_naive()).unwrap();
    let mut connectors = jira;
    connectors.push(Arc::new(FakeConnector::confluence()));
    connectors.push(Arc::new(FakeConnector::slack()));
    connectors.push(Arc::new(FakeConnector::docs()));
    let svc = pmos_toolserver::ToolService::new(catalog, stores, connectors)
        .unwrap()
        .with_open_directory();
    let now = chrono::Utc::now();
    let s = svc
        .create_session("evals", "meeting-notes", "r1", None, now)
        .await
        .unwrap();
    let out = svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now)
        .await
        .unwrap();
    let want: Vec<Value> = FakeConnector::jira()
        .read(Caller::new("evals", "r1"), "search", &json!({"jql": "sso"}))
        .await
        .unwrap()
        .iter()
        .map(untrusted)
        .collect();
    assert_eq!(out["results"], json!(want));
    assert!(fake
        .calls()
        .contains(&"tools/call:jira_search_issues".to_string()));
    let e = audit
        .events()
        .into_iter()
        .find(|e| e.tool == "jira.search")
        .unwrap();
    assert_eq!(e.sources, vec!["jira:PM-101"]);
    assert!(e.detail.unwrap().contains("via mcp server fake-jira"));
    assert_eq!(svc.served_by("jira.search"), "fake-jira");
    assert_eq!(svc.served_by("confluence.search"), "fake");
    cancel.cancel();
}
