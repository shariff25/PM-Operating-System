//! End to end over HTTP: the orchestrator creates a session, an MCP client (standing in for
//! the harness) lists and calls tools, and the PM approves a write through the inbox API.

mod common;

use std::sync::Arc;

use pmos_toolserver::audit::Outcome;
use pmos_toolserver::{router, AppConfig};
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";

struct Server {
    base: String,
    cancel: CancellationToken,
    fixture: common::Fixture,
}

async fn start() -> Server {
    let fixture = common::fixture();
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&fixture.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
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
    Server {
        base,
        cancel,
        fixture,
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn create_session(base: &str, skill: &str) -> String {
    let res = http()
        .post(format!("{base}/internal/sessions"))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"user": "ana", "skill": skill, "run_id": "run-e2e"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let body: Value = res.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn internal_api_needs_the_service_token() {
    let s = start().await;
    let res = http()
        .post(format!("{}/internal/sessions", s.base))
        .json(&json!({"user": "ana", "skill": "ask", "run_id": "r"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let res = http()
        .post(format!("{}/internal/sessions", s.base))
        .bearer_auth("wrong-token-wrong-token-wrong-token")
        .json(&json!({"user": "ana", "skill": "ask", "run_id": "r"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let res = http()
        .post(format!("{}/internal/sessions", s.base))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"user": "ana", "skill": "nope", "run_id": "r"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    let health = http()
        .get(format!("{}/healthz", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    s.cancel.cancel();
}

#[tokio::test]
async fn mcp_needs_a_session_token() {
    let s = start().await;
    let res = http()
        .post(format!("{}/mcp", s.base))
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    // The service token is not a session token.
    let res = http()
        .post(format!("{}/mcp", s.base))
        .bearer_auth(SERVICE_TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    s.cancel.cancel();
}

#[tokio::test]
async fn draft_waits_for_approval_then_lands() {
    let s = start().await;
    let token = create_session(&s.base, "prd-draft").await;

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", s.base))
            .auth_header(token.clone()),
    );
    let client = ClientConfig::default().serve(transport).await.unwrap();

    // Only the skill's declared tools are listed.
    let tools = client.list_tools(Default::default()).await.unwrap();
    let mut names: Vec<_> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "confluence_create_draft",
            "confluence_read",
            "confluence_search",
            "context_read",
            "context_search",
            "jira_read",
            "jira_search",
        ]
    );

    // An undeclared tool comes back as a readable error, not a protocol failure.
    let denied = client
        .call_tool(
            CallToolRequestParams::new("slack_send_message").with_arguments(
                json!({"channel": "C1", "text": "hi"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));

    // The write queues instead of running.
    let args = json!({"space": "PM", "title": "PRD: SSO for DSM", "body": "Draft body"});
    let result = client
        .call_tool(
            CallToolRequestParams::new("confluence_create_draft")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let out = result.structured_content.expect("structured result");
    assert_eq!(out["status"], "pending_approval");
    let approval_id = out["approval_id"].as_str().unwrap().to_string();
    assert!(s.fixture.confluence.writes().is_empty());

    // The PM sees it in the inbox and approves it.
    let inbox: Value = http()
        .get(format!(
            "{}/internal/approvals?user=ana&status=pending",
            s.base
        ))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(inbox["items"][0]["id"], approval_id.as_str());

    let not_theirs = http()
        .post(format!(
            "{}/internal/approvals/{approval_id}/decision",
            s.base
        ))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"decided_by": "bo", "decision": "approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(not_theirs.status(), 403);

    let decided: Value = http()
        .post(format!(
            "{}/internal/approvals/{approval_id}/decision",
            s.base
        ))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"decided_by": "ana", "decision": "approve"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(decided["status"], "approved");
    assert_eq!(decided["result"]["status"], "done");

    let writes = s.fixture.confluence.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].user, "ana");
    assert_eq!(writes[0].args, args);

    // Granted path: the PM approves in the harness first, then the same call runs.
    let args2 = json!({"space": "PM", "title": "PRD: SCIM", "body": "Second draft"});
    let granted = http()
        .post(format!("{}/internal/grants", s.base))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"session_token": token, "tool": "confluence_create_draft", "args": args2}))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), 201);
    let result = client
        .call_tool(
            CallToolRequestParams::new("confluence_create_draft")
                .with_arguments(args2.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["status"], "done");
    assert_eq!(s.fixture.confluence.writes().len(), 2);

    let outcomes: Vec<_> = s
        .fixture
        .audit
        .events()
        .into_iter()
        .map(|e| (e.tool, e.outcome))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("slack.send_message".to_string(), Outcome::Denied),
            ("confluence.create_draft".to_string(), Outcome::Pending),
            ("confluence.create_draft".to_string(), Outcome::Denied),
            ("confluence.create_draft".to_string(), Outcome::Approved),
            ("confluence.create_draft".to_string(), Outcome::Granted),
            ("confluence.create_draft".to_string(), Outcome::Approved),
        ]
    );

    let _ = client.cancel().await;
    s.cancel.cancel();
}

#[tokio::test]
async fn revoked_sessions_are_refused() {
    let s = start().await;
    let token = create_session(&s.base, "ask").await;
    let res = http()
        .delete(format!("{}/internal/sessions/{token}", s.base))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let res = http()
        .post(format!("{}/mcp", s.base))
        .bearer_auth(&token)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    s.cancel.cancel();
}
