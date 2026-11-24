//! Slice M6: Jira behind the switch (`docs/mcp-servers-spec.md` §6). With `PMOS_MCP_SYSTEMS`
//! empty (the default) Jira stays on the built-in connector; with `jira` it is served by the
//! one approved record in `content/mcp/` that serves it. Here the record is loaded from a
//! content tree, checked by the lint and against the user database's Admins, and served by the
//! fake MCP server; the same Jira calls run through the whole tool service on the built-in fake
//! and on the MCP backend, and must answer the same and audit the same.

mod common;

use std::sync::Arc;

use common::{fixture, memory_users, now, Fixture};
use pmos_toolserver::approvals::Decision;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::mcp_client::fake::{self, FakeMcp, FAKE_HOST};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::ToolService;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fake-mcp-token-for-jira-0123456789abcdef";

/// The fixture with Jira served by `content/mcp/fake-jira.yaml` (written into its content tree).
async fn on_mcp() -> (Fixture, FakeMcp, CancellationToken) {
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new();
    let addr = fake::serve(
        fake.clone(),
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    let mut f = fixture();
    let mut record = fake::fake_records(addr).remove(0);
    record.approved_by = common::FIXTURE_ADMIN.0.into();
    for t in &mut record.tools {
        t.approved_by = common::FIXTURE_ADMIN.0.into();
    }
    common::write_content(
        &f.root,
        "mcp/fake-jira.yaml",
        &serde_norway::to_string(&record).unwrap(),
    );
    // The content lint accepts it, and the tool server loads it.
    let report = pmos_contract::lint(
        &f.root,
        &pmos_contract::LintOptions::new(now().date_naive()),
    );
    assert!(
        !report.has_errors(),
        "{:?}",
        report.errors().collect::<Vec<_>>()
    );
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(f.root.join("tools.yaml")).unwrap(),
    )
    .unwrap();
    let records = pmos_contract::mcp::load(&f.root, &registry).unwrap();
    // Approved by an active Admin in the user database.
    let users = memory_users(vec![], &f.stores).await;
    let dir = users.snapshot(now()).await.unwrap();
    pmos_toolserver::mcp_client::check_approvals(&records, &dir).unwrap();
    let served = pmos_toolserver::mcp_client::connectors_for(
        &records,
        &["jira".to_string()],
        |k| (k == "PMOS_MCP_TOKEN_FAKE").then(|| TOKEN.to_string()),
        None,
        Some(f.stores.audit.clone()),
        &TestRoutes::default(),
    )
    .unwrap();
    let catalog = Catalog::load(&f.root, now().date_naive()).unwrap();
    let mut connectors: Vec<Arc<dyn Connector>> = served.connectors;
    connectors.extend([
        f.confluence.clone() as Arc<dyn Connector>,
        f.slack.clone(),
        Arc::new(FakeConnector::docs()),
    ]);
    f.svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );
    (f, fake, cancel)
}

/// Strip what is only the MCP route's own: the audit detail naming the server.
fn shape(f: &Fixture, tool: &str) -> Vec<(String, pmos_toolserver::audit::Outcome, Vec<String>)> {
    f.audit
        .events()
        .into_iter()
        .filter(|e| e.tool == tool)
        .map(|e| (e.tool, e.outcome, e.sources))
        .collect()
}

async fn run(f: &Fixture) -> Vec<Value> {
    let mut out = Vec::new();
    let s = f
        .svc
        .create_session("ana", "field-watch", "r1", None, now())
        .await
        .unwrap();
    for (tool, args) in [
        ("jira_search", json!({"jql": "sso"})),
        ("jira_search", json!({"jql": "story OR bug", "limit": 2})),
        ("jira_read", json!({"key": "PM-102"})),
        ("jira_read", json!({"key": "PM-999"})),
        ("jira_search", json!({"jql": "\"unclosed"})),
    ] {
        out.push(match f.svc.call(&s, tool, args, now()).await {
            Ok(v) => v,
            Err(e) => json!({"error_kind": format!("{e:?}").split('(').next().unwrap()}),
        });
    }
    let p = f
        .svc
        .create_session("ana", "portfolio-review", "r2", None, now())
        .await
        .unwrap();
    for group in ["project", "epic", "status", "assignee", "fix_version"] {
        out.push(
            f.svc
                .call(
                    &p,
                    "jira_rollup",
                    json!({"jql": "project in (PM, CLD)", "group_by": group}),
                    now(),
                )
                .await
                .unwrap(),
        );
    }
    // A team bot inside its scope (Jira project PM).
    let b = f
        .svc
        .create_bot_session("ana", "scout", "tsk_1", None, now())
        .await
        .unwrap();
    out.push(
        f.svc
            .call(&b, "jira_search", json!({"jql": "sso OR export"}), now())
            .await
            .unwrap(),
    );
    out
}

#[tokio::test]
async fn jira_on_its_record_answers_and_audits_as_the_built_in_connector() {
    let builtin = fixture();
    let (mcp, fake, _cancel) = on_mcp().await;
    assert_eq!(run(&mcp).await, run(&builtin).await);
    for tool in ["jira.search", "jira.read", "jira.rollup"] {
        assert_eq!(shape(&mcp, tool), shape(&builtin, tool), "{tool}");
    }
    assert!(fake
        .calls()
        .iter()
        .any(|c| c == "tools/call:jira_search_raw"));
    assert_eq!(mcp.svc.served_by("jira.read"), "fake-jira");
    assert_eq!(builtin.svc.served_by("jira.read"), "fake");
}

#[tokio::test]
async fn jiras_write_goes_through_the_approval_gate_on_its_record() {
    let (f, fake, _cancel) = on_mcp().await;
    let s = f
        .svc
        .create_session("ana", "meeting-notes", "r1", None, now())
        .await
        .unwrap();
    let args =
        json!({"project": "PM", "issue_type": "Story", "summary": "SCIM", "description": "D"});
    // A grant for exactly these arguments runs once.
    f.svc
        .grant(&s, "jira_create_issue", &args, now())
        .await
        .unwrap();
    let done = f
        .svc
        .call(&s, "jira_create_issue", args.clone(), now())
        .await
        .unwrap();
    assert_eq!(done["status"], "done");
    assert_eq!(done["id"], "JIRA-W1");
    // Without one it waits in the inbox, and runs as the PM once approved.
    let res = f
        .svc
        .call(&s, "jira_create_issue", args, now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
    let id = res["approval_id"].as_str().unwrap();
    assert_eq!(fake.jira_writes().len(), 1);
    f.svc
        .decide(id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(fake.jira_writes().len(), 2);
    assert!(fake.jira_writes()[1].args["description"]
        .as_str()
        .unwrap()
        .ends_with("Drafted with PM OS · run r1"));
    // The ledger keeps its row as for a connector write.
    let rows = f
        .svc
        .ledger(pmos_toolserver::ledger::LedgerQuery::all(now()), now())
        .await
        .unwrap();
    assert!(rows
        .iter()
        .any(|r| r.id == id && r.tool == "jira.create_issue"));
}

#[tokio::test]
async fn a_record_an_admin_didnt_approve_is_refused() {
    let (f, _fake, _cancel) = on_mcp().await;
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(f.root.join("tools.yaml")).unwrap(),
    )
    .unwrap();
    let mut records = pmos_contract::mcp::load(&f.root, &registry).unwrap();
    records[0].approved_by = "ana".into();
    let mut ana = common::member("ana", "U0ANA", vec![pmos_contract::model::Role::Pm]);
    ana.email = None;
    let users = memory_users(vec![ana], &f.stores).await;
    let dir = users.snapshot(now()).await.unwrap();
    assert!(pmos_toolserver::mcp_client::check_approvals(&records, &dir).is_err());
    // No record serves Confluence: switching it would be refused.
    assert!(pmos_toolserver::mcp_client::connectors_for(
        &records,
        &["confluence".to_string()],
        |_| None,
        None,
        None,
        &TestRoutes::default()
    )
    .is_err());
}
