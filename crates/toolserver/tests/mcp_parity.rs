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
    records: Vec<pmos_contract::mcp::ServerRecord>,
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
    let records = fake::fake_records(addr);
    let mcp = records
        .clone()
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
        records,
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
    // Writes a record doesn't map stay on the built-in connector. (Changed with slice M8: the
    // fake records now map Confluence's writes and Slack's audience checks, so this is shown on
    // records without them; their own parity is in the tests below.)
    let reads_only = |n: usize| {
        let mut r = b.records[n].clone();
        r.tools
            .retain(|t| t.class() == pmos_contract::mcp::ToolClass::Read);
        r.checks.clear();
        let server = Arc::new(
            McpServer::new(
                r,
                ServerAuth::Token(Secret::new(TOKEN)),
                None,
                &TestRoutes::default(),
            )
            .unwrap(),
        );
        let system = b.records[n].systems[0].clone();
        McpConnector::new(&system, server, "fake-mcp").unwrap()
    };
    assert!(matches!(
        reads_only(1)
            .write(
                caller,
                "create_draft",
                &json!({"space": "PM", "title": "T", "body": "B"})
            )
            .await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(matches!(
        reads_only(2).audience(caller, "C0FIELD").await,
        Err(ConnectorError::Unsupported(_))
    ));
}

#[tokio::test]
async fn confluences_writes_return_the_same_reference_and_errors() {
    // Slice M8. Each backend has its own pages, all at version 1.
    let b = backends().await;
    let caller = Caller::new("evals", "r1");
    let mut checked = 0;
    for (action, args) in [
        (
            "create_draft",
            json!({"space": "PM", "title": "T", "body": "B <b>&</b>"}),
        ),
        (
            "create_draft",
            json!({"space": "PM", "title": "Child", "body": "B", "parent_id": "9001"}),
        ),
        (
            "update_page",
            json!({"page_id": "9002", "version": 1, "body": "New", "version_comment": "c"}),
        ),
        // Now stale on both.
        (
            "update_page",
            json!({"page_id": "9002", "version": 1, "body": "Again"}),
        ),
        (
            "update_page",
            json!({"page_id": "9001", "version": 7, "body": "Never read"}),
        ),
        (
            "create_draft",
            json!({"space": "pm lower", "title": "T", "body": "B"}),
        ),
        (
            "update_page",
            json!({"page_id": "abc", "version": 1, "body": "B"}),
        ),
    ] {
        let fake = b.fakes[1].write(caller, action, &args).await;
        let mcp = b.mcp[1].write(caller, action, &args).await;
        match (&fake, &mcp) {
            (Ok(x), Ok(y)) => assert_eq!(x, y, "{action} {args}"),
            (Err(x), Err(y)) => assert_eq!(kind(x), kind(y), "{action} {args}"),
            // The built-in fake doesn't check a key's or an id's shape (the live connector
            // does, before it calls Confluence, and so does the MCP route).
            (Ok(_), Err(ConnectorError::BadArgs(_)))
                if args["space"] == "pm lower" || args["page_id"] == "abc" => {}
            _ => panic!("{action} {args}: {fake:?} vs {mcp:?}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 7);
}

#[tokio::test]
async fn slacks_posts_audiences_and_labels_are_the_same() {
    // Slice M8: the fake record approves both audience checks, so posts move too.
    let b = backends().await;
    let caller = Caller::new("evals", "r1");
    let mut checked = 0;
    for (action, args) in [
        (
            "post_reply",
            json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"}),
        ),
        (
            "post_reply",
            json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "message_ts": "1731600300.000200", "text": "Tagged"}),
        ),
        (
            "send_message",
            json!({"channel": "D0EVALSBEN", "text": "Hi"}),
        ),
        ("send_message", json!({"channel": "c0lower", "text": "Hi"})),
        (
            "post_reply",
            json!({"channel": "C0PMTEAM", "thread_ts": "not-a-ts", "text": "x"}),
        ),
    ] {
        let fake = b.fakes[2].write(caller, action, &args).await;
        let mcp = b.mcp[2].write(caller, action, &args).await;
        match (&fake, &mcp) {
            (Ok(x), Ok(y)) => assert_eq!(x, y, "{action} {args}"),
            (Err(x), Err(y)) => assert_eq!(kind(x), kind(y), "{action} {args}"),
            // The built-in fake doesn't check an id's shape (the live connector does, before it
            // calls Slack, and so does the MCP route).
            (Ok(_), Err(ConnectorError::BadArgs(_)))
                if args["channel"] == "c0lower" || args["thread_ts"] == "not-a-ts" => {}
            _ => panic!("{action} {args}: {fake:?} vs {mcp:?}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 5);
    for channel in [
        "C0PMTEAM",
        "C0FIELD",
        "C0GLOBEXCX",
        "D0EVALSBEN",
        "C0NOWHERE",
    ] {
        let (fake, mcp) = (
            b.fakes[2].audience(caller, channel).await,
            b.mcp[2].audience(caller, channel).await,
        );
        match (&fake, &mcp) {
            (Ok(x), Ok(y)) => assert_eq!(x, y, "{channel}"),
            (Err(x), Err(y)) => assert_eq!(kind(x), kind(y), "{channel}"),
            _ => panic!("{channel}: {fake:?} vs {mcp:?}"),
        }
        assert_eq!(
            b.fakes[2].conversation(caller, channel).await.ok(),
            b.mcp[2].conversation(caller, channel).await.ok(),
            "{channel}"
        );
    }
}

#[tokio::test]
async fn the_fake_mcp_mode_serves_confluence_and_slack_and_policy_admin_shows_the_checks() {
    // `PMOS_CONNECTORS=fake` with `PMOS_MCP_SYSTEMS=confluence,slack`, as `main` builds it.
    let cancel = CancellationToken::new();
    let (served, fake) = pmos_toolserver::mcp_client::start_fake_for(
        &["confluence".to_string(), "slack".to_string()],
        FakeMcp::new(),
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert!(
        served.reads_only.is_empty(),
        "the fake approves both checks"
    );
    let builtin: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
    ];
    let over = served.over(&builtin).unwrap();
    assert_eq!(
        (
            over[0].served_by("create_draft"),
            over[1].served_by("post_reply")
        ),
        ("fake-confluence".to_string(), "fake-slack".to_string())
    );
    let ready = pmos_toolserver::mcp_client::readiness(&served.servers).await;
    assert_eq!(ready["fake-confluence"]["status"], "ok");
    assert_eq!(ready["fake-slack"]["status"], "ok");
    let view = pmos_toolserver::mcp_client::policy_view(&served.servers);
    let slack = view
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "fake-slack")
        .unwrap();
    assert_eq!(slack["slack_posts"], "mcp");
    let checks = slack["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks
        .iter()
        .all(|c| c["class"] == "read" && c["state"] == "approved"));
    assert_eq!(view[0]["slack_posts"], Value::Null);
    // The checks are never listed to a bot: only registry tools are.
    assert!(!fake.calls().iter().any(|c| c.starts_with("tools/call:")));

    // A Slack record without the checks: reads over MCP, posts paired with the built-in.
    let addr = fake::serve(
        FakeMcp::new(),
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    let mut record = fake::fake_records(addr).remove(2);
    record.checks.clear();
    record
        .tools
        .retain(|t| t.class() == pmos_contract::mcp::ToolClass::Read);
    let served = pmos_toolserver::mcp_client::connectors_for(
        &[record],
        &["slack".to_string()],
        |_| Some(TOKEN.to_string()),
        None,
        None,
        &TestRoutes::default(),
    )
    .unwrap();
    assert_eq!(served.reads_only.len(), 1);
    // On its own it refuses posts; paired, they go to the built-in connector.
    let caller = Caller::new("evals", "r1");
    let post = json!({"channel": "C0PMTEAM", "text": "Hi"});
    assert!(matches!(
        served.connectors[0]
            .write(caller, "send_message", &post)
            .await,
        Err(ConnectorError::Unsupported(_))
    ));
    let builtin = Arc::new(FakeConnector::slack());
    let slack = served
        .over(&[builtin.clone() as Arc<dyn Connector>])
        .unwrap()
        .remove(0);
    assert_eq!(slack.served_by("search"), "fake-slack");
    assert_eq!(slack.served_by("send_message"), "fake");
    assert!(slack.route("send_message").is_none());
    slack.write(caller, "send_message", &post).await.unwrap();
    assert_eq!(builtin.writes().len(), 1);
    assert!(slack.audience(caller, "C0PMTEAM").await.is_ok());
    assert_eq!(
        slack
            .read(caller, "search", &json!({"query": "SAML"}))
            .await
            .unwrap()
            .len(),
        FakeConnector::slack()
            .read(caller, "search", &json!({"query": "SAML"}))
            .await
            .unwrap()
            .len()
    );
    let view = pmos_toolserver::mcp_client::policy_view(&served.servers);
    assert_eq!(view[0]["slack_posts"], "builtin");
    // Without the built-in connector to pair with, it is refused rather than left post-less.
    assert!(served.over(&[]).is_err());
    cancel.cancel();
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
    let jira = jira.connectors;
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

#[tokio::test]
async fn readyz_and_policy_admin_show_each_server_and_its_approved_tools() {
    let cancel = CancellationToken::new();
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../content"));
    let (stores, _) = pmos_toolserver::Stores::in_memory(root);
    let (served, fake) = pmos_toolserver::mcp_client::start_fake(None, cancel.clone())
        .await
        .unwrap();
    let catalog =
        pmos_toolserver::catalog::Catalog::load(root, chrono::Utc::now().date_naive()).unwrap();
    let mut connectors = served.connectors;
    connectors.push(Arc::new(FakeConnector::confluence()));
    connectors.push(Arc::new(FakeConnector::slack()));
    connectors.push(Arc::new(FakeConnector::docs()));
    let servers = served.servers;
    let svc = Arc::new(
        pmos_toolserver::ToolService::new(catalog, stores, connectors)
            .unwrap()
            .with_open_directory()
            .with_mcp_servers(servers.clone()),
    );
    let ready = pmos_toolserver::mcp_client::readiness(svc.mcp_servers()).await;
    assert_eq!(
        ready,
        json!({"fake-jira": {"status": "ok", "hosts": ["fake-mcp.pmos.internal"]}})
    );
    let view = pmos_toolserver::mcp_client::policy_view(svc.mcp_servers());
    let s = &view[0];
    assert_eq!(s["id"], "fake-jira");
    assert_eq!(
        (s["operator"].as_str(), s["auth"].as_str()),
        (Some("pmos"), Some("service_token"))
    );
    let tools = s["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert!(tools.iter().all(|t| t["state"] == "approved"));
    assert_eq!(
        tools
            .iter()
            .find(|t| t["registry"] == "jira.create_issue")
            .unwrap()["class"],
        "write"
    );
    assert!(
        !view.to_string().to_lowercase().contains("token\":"),
        "no token field"
    );
    // A changed tool shows on both.
    fake.tricks
        .change_search
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let fresh = pmos_toolserver::mcp_client::start_fake(None, cancel.clone())
        .await
        .unwrap();
    fresh
        .1
        .tricks
        .change_search
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let ready = pmos_toolserver::mcp_client::readiness(&fresh.0.servers).await;
    assert_eq!(ready["fake-jira"]["status"], "changed");
    cancel.cancel();
}
