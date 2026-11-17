//! The stats tools end to end: allowlisted per skill, audited, answered by the tool server
//! itself (no connector) with fake and with live connectors, over the internal call path.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{content_tree, now, write_content};
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::atlassian::{Confluence, Jira};
use pmos_toolserver::connectors::docs::Docs;
use pmos_toolserver::connectors::slack::Slack;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::oauth::{AccessToken, TokenError, TokenSource};
use pmos_toolserver::service::ToolError;
use pmos_toolserver::vault::Provider;
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "service-token-0123456789abcdef-0123";

/// A skill that declares both stats tools (the real `experiment-design` and
/// `experiment-readout` each declare one; this keeps the test independent of their text).
const STATS_SKILL: &str = "---\nname: stats-test\ndescription: Test skill that sizes and reads experiments.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"reasoning\"\n  data-class: \"internal\"\n  tools: \"context.search stats.sample_size stats.compare\"\n---\n\n# /stats-test\n\nTest.\n";

fn sizing() -> Value {
    json!({"metric": "proportion", "baseline": 0.10, "mde": 0.10, "daily_users": 2000})
}

fn readout() -> Value {
    json!({
        "control": {"users": 1000, "conversions": 100},
        "treatment": {"users": 1000, "conversions": 130}
    })
}

#[tokio::test]
async fn stats_over_the_internal_call_path() {
    let mut f = common::fixture();
    write_content(&f.root, "skills/stats-test/SKILL.md", STATS_SKILL);
    // Reload the catalog with the test skill, as a content merge would.
    let catalog = Catalog::load(&f.root, now().date_naive()).expect("content lints");
    let connectors: Vec<Arc<dyn Connector>> = vec![
        f.jira.clone(),
        f.confluence.clone(),
        f.slack.clone(),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::docs()),
    ];
    f.svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );

    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            console_token: None,
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
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

    // The HTTP layer checks sessions against the real clock.
    let stats = f
        .svc
        .create_session("ana", "stats-test", "r1", None, chrono::Utc::now())
        .await
        .unwrap();
    let ask = f
        .svc
        .create_session("ana", "ask", "r2", None, chrono::Utc::now())
        .await
        .unwrap();
    let call = |token: String, tool: &'static str, args: Value| {
        let base = base.clone();
        async move {
            let res = reqwest::Client::new()
                .post(format!("{base}/internal/call"))
                .bearer_auth(SERVICE_TOKEN)
                .header("x-pmos-session", token)
                .json(&json!({"tool": tool, "args": args}))
                .send()
                .await
                .unwrap();
            let status = res.status().as_u16();
            (status, res.json::<Value>().await.unwrap())
        }
    };

    // The model sees the tools with underscores, like every other tool.
    let names: Vec<String> = f
        .svc
        .list_tools(&stats)
        .iter()
        .map(|t| t.harness_name.clone())
        .collect();
    assert!(
        names.contains(&"stats_sample_size".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"stats_compare".to_string()), "{names:?}");

    let (status, body) = call(stats.token.clone(), "stats_sample_size", sizing()).await;
    assert_eq!(status, 200, "{body}");
    let out = &body["result"];
    assert_eq!(out["per_arm"], 14749);
    assert_eq!(out["total"], 29498);
    assert_eq!(out["days"], 15);
    // Computed, not external content: returned as is, not wrapped as untrusted.
    assert!(
        out.get("untrusted").is_none() && out.get("results").is_none(),
        "{out}"
    );

    let (status, body) = call(stats.token.clone(), "stats_compare", readout()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["p_value"], 0.03549);
    assert_eq!(body["result"]["significant"], true);

    // Bad input is refused as invalid arguments, by the schema or by the maths.
    let (status, body) = call(
        stats.token.clone(),
        "stats_sample_size",
        json!({"metric": "proportion", "baseline": 0.10, "mde": 0}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("mde"), "{body}");
    let (status, _) = call(
        stats.token.clone(),
        "stats_compare",
        json!({"control": {"users": 10, "conversions": 11}, "treatment": {"users": 10, "conversions": 1}}),
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = call(
        stats.token.clone(),
        "stats_sample_size",
        json!({"metric": "proportion", "baseline": "ten", "mde": 0.1}),
    )
    .await;
    assert_eq!(status, 400);

    // A skill that doesn't declare the tool can't call it.
    let (status, body) = call(ask.token.clone(), "stats_sample_size", sizing()).await;
    assert_eq!(status, 403, "{body}");

    // Every call is audited, under the registry name.
    let events: Vec<_> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool.starts_with("stats."))
        .collect();
    let seen: Vec<(&str, &str, Outcome)> = events
        .iter()
        .map(|e| (e.run_id.as_str(), e.tool.as_str(), e.outcome))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("r1", "stats.sample_size", Outcome::Ok),
            ("r1", "stats.compare", Outcome::Ok),
            ("r1", "stats.sample_size", Outcome::Error),
            ("r1", "stats.compare", Outcome::Error),
            ("r1", "stats.sample_size", Outcome::Error),
            ("r2", "stats.sample_size", Outcome::Denied),
        ]
    );
    assert!(events.iter().all(|e| e.client == Client::Orchestrator));
    assert!(events.iter().all(|e| e.args_hash.is_some()));
    cancel.cancel();
}

/// Live connectors that would fail if called: no account is linked for anyone.
struct NoTokens;

#[async_trait]
impl TokenSource for NoTokens {
    async fn token(&self, _user: &str, provider: Provider) -> Result<AccessToken, TokenError> {
        Err(TokenError::NotConnected(provider))
    }
}

#[tokio::test]
async fn stats_need_no_connector_with_live_connectors() {
    let (_dir, root) = content_tree();
    write_content(&root, "skills/stats-test/SKILL.md", STATS_SKILL);
    let (stores, audit) = Stores::in_memory(&root);
    let sessions = stores.sessions.clone();
    let catalog = Catalog::load(&root, now().date_naive()).expect("content lints");
    let tokens: Arc<dyn TokenSource> = Arc::new(NoTokens);
    // Nothing listens here: a connector call would fail.
    let dead = "http://127.0.0.1:9";
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(Jira::new(tokens.clone(), dead)),
        Arc::new(Confluence::new(tokens.clone(), dead)),
        Arc::new(Slack::new(tokens, dead)),
        Arc::new(Docs::new(dead, "docs-token")),
    ];
    let svc = ToolService::new(catalog, stores, connectors)
        .unwrap()
        .with_open_directory();
    let session = svc
        .create_session("ana", "stats-test", "r1", None, now())
        .await
        .unwrap();

    let out = svc
        .call(&session, "stats_sample_size", sizing(), now())
        .await
        .unwrap();
    assert_eq!(out["per_arm"], 14749);
    let out = svc
        .call(&session, "stats_compare", readout(), now())
        .await
        .unwrap();
    assert_eq!(out["significant"], true);
    // Bad input is still refused.
    assert!(matches!(
        svc.call(&session, "stats_compare", json!({"control": {"users": 0, "conversions": 0}, "treatment": {"users": 1, "conversions": 0}}), now()).await,
        Err(ToolError::InvalidArgs(_))
    ));
    let audited: Vec<_> = audit
        .events()
        .into_iter()
        .map(|e| (e.tool, e.outcome, e.client))
        .collect();
    assert_eq!(
        audited,
        vec![
            ("stats.sample_size".to_string(), Outcome::Ok, Client::Mcp),
            ("stats.compare".to_string(), Outcome::Ok, Client::Mcp),
            ("stats.compare".to_string(), Outcome::Error, Client::Mcp),
        ]
    );
    // Reading the stats doesn't raise the session's data class: nothing external was read.
    let stored = sessions.get(&session.token, now()).await.unwrap().unwrap();
    assert_eq!(
        stored.sources_sensitivity,
        pmos_contract::model::Sensitivity::Public
    );
}
