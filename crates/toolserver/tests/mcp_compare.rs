//! Slice M7: old and new side by side for Jira's reads, on fake data only
//! (`docs/mcp-servers-spec.md` §6, the owner's decision of 2026-11-24). Every Jira read the
//! fixtures answer runs on the in-process fake (whose answer the caller gets) and on the fake MCP
//! server, and each comparison is audited as `mcp.compare` with counts, ids and field names only.
//! The comparison's result on the fixtures is asserted here and recorded in the spec's status note.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{fixture, now};
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::mcp_client::compare::SideBySide;
use pmos_toolserver::ToolService;
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn side_by_side() -> (
    common::Fixture,
    pmos_toolserver::mcp_client::fake::FakeMcp,
    CancellationToken,
) {
    let cancel = CancellationToken::new();
    let mut f = fixture();
    let (served, fake) =
        pmos_toolserver::mcp_client::start_fake(Some(f.stores.audit.clone()), cancel.clone())
            .await
            .unwrap();
    let jira: Arc<dyn Connector> = Arc::new(SideBySide::new(
        f.jira.clone(),
        served.connectors[0].clone(),
        f.stores.audit.clone(),
    ));
    let catalog = Catalog::load(&f.root, now().date_naive()).unwrap();
    f.svc = Arc::new(
        ToolService::new(
            catalog,
            f.stores.clone(),
            vec![
                jira,
                f.confluence.clone(),
                f.slack.clone(),
                Arc::new(FakeConnector::docs()),
            ],
        )
        .unwrap()
        .with_open_directory(),
    );
    (f, fake, cancel)
}

async fn every_read(f: &common::Fixture) -> usize {
    let mut n = 0;
    let s = f
        .svc
        .create_session("ana", "field-watch", "r1", None, now())
        .await
        .unwrap();
    let p = f
        .svc
        .create_session("ana", "portfolio-review", "r2", None, now())
        .await
        .unwrap();
    let b = f
        .svc
        .create_bot_session("ana", "scout", "tsk_1", None, now())
        .await
        .unwrap();
    let mut calls = Vec::new();
    for jql in [
        "sso",
        "SCIM",
        "export",
        "story epic",
        "nothing-matches",
        "sso ORDER BY key",
    ] {
        calls.push((&s, "jira_search", json!({"jql": jql})));
        calls.push((&s, "jira_search", json!({"jql": jql, "limit": 1})));
        calls.push((&b, "jira_search", json!({"jql": jql})));
    }
    for key in ["PM-101", "PM-102", "PM-103", "PM-999"] {
        calls.push((&s, "jira_read", json!({"key": key})));
        calls.push((&b, "jira_read", json!({"key": key})));
    }
    for group in ["project", "epic", "status", "assignee", "fix_version"] {
        calls.push((
            &p,
            "jira_rollup",
            json!({"jql": "project in (PM, CLD)", "group_by": group}),
        ));
    }
    calls.push((
        &p,
        "jira_rollup",
        json!({"jql": "project = PM", "max_issues": 2}),
    ));
    for (session, tool, args) in calls {
        let _ = f.svc.call(session, tool, args, now()).await;
        n += 1;
    }
    n
}

#[tokio::test]
async fn on_the_fixtures_every_jira_read_is_the_same_on_both() {
    let (f, fake, _cancel) = side_by_side().await;
    let n = every_read(&f).await;
    let compared: Vec<_> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool == "mcp.compare")
        .collect();
    // The team bot's out-of-scope read is refused before either backend is called.
    assert!(compared.len() >= n - 4, "{} of {n}", compared.len());
    let differ: Vec<String> = compared
        .iter()
        .filter(|e| e.outcome != Outcome::Ok)
        .map(|e| e.detail.clone().unwrap())
        .collect();
    assert!(differ.is_empty(), "{differ:#?}");
    eprintln!(
        "side by side on the fixtures: {} Jira reads compared, {} differed",
        compared.len(),
        differ.len()
    );
    for e in &compared {
        let d = e.detail.as_deref().unwrap();
        assert!(d.contains(": same; builtin "), "{d}");
        assert!(
            e.args_hash.is_some() && e.sources.is_empty(),
            "metadata only"
        );
        assert!(
            !d.contains("SSO") && !d.contains("Acme"),
            "no titles or text: {d}"
        );
    }
    assert!(fake
        .calls()
        .iter()
        .any(|c| c == "tools/call:jira_search_raw"));
}

#[tokio::test]
async fn a_difference_is_recorded_and_the_caller_still_gets_the_built_in_answer() {
    let (f, fake, _cancel) = side_by_side().await;
    let s = f
        .svc
        .create_session("ana", "field-watch", "r1", None, now())
        .await
        .unwrap();
    fake.tricks.drop_read.store(true, Ordering::SeqCst);
    let out = f
        .svc
        .call(&s, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .unwrap();
    assert_eq!(out["results"][0]["source"]["id"], "PM-101");
    let e = f
        .audit
        .events()
        .into_iter()
        .find(|e| e.tool == "mcp.compare")
        .unwrap();
    assert_eq!(e.outcome, Outcome::Error);
    assert_eq!(
        e.detail.as_deref(),
        Some("jira.read: differ; builtin 1 items, mcp error forbidden")
    );
}

#[tokio::test]
async fn writes_go_to_the_built_in_backend_only() {
    let (f, fake, _cancel) = side_by_side().await;
    let s = f
        .svc
        .create_session("ana", "meeting-notes", "r1", None, now())
        .await
        .unwrap();
    let args = json!({"project": "PM", "issue_type": "Task", "summary": "T"});
    f.svc
        .grant(&s, "jira_create_issue", &args, now())
        .await
        .unwrap();
    let done = f
        .svc
        .call(&s, "jira_create_issue", args, now())
        .await
        .unwrap();
    assert_eq!(done["status"], "done");
    assert_eq!(f.jira.writes().len(), 1);
    assert!(fake.jira_writes().is_empty(), "never sent twice");
    assert!(!fake.calls().iter().any(|c| c.contains("create")));
}
