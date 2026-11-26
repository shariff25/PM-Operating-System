//! The shim and the gate must map a call the same way, or an honest proof fails. This runs
//! both in one process, with a gate whose sessions end after one call, to pin when the shim
//! forgets its tool listing: only when the gate says it has lost its own (review findings
//! GR-103 and GR-112).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Bytes;
use axum::routing::post;
use axum::{Json, Router};
use gov_core::action::{Grant, Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::mandate::{AgentRef, IssuedBy, IssuerKind, Mandate};
use gov_core::receipt::{Proof, Receipt};
use gov_core::{open, seal, Id, KeySet, ResourcePattern, Scope, SigningKey};
use gov_gate::forward::{Forwarder, ForwarderConfig, MAX_IN_FLIGHT};
use gov_gate::sessions::SessionsConfig;
use gov_gate::{FileSpool, Gate, GateConfig, Spool};
use gov_run::shim::{Shim, ShimConfig};
use serde_json::{json, Value};

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}

fn update_tool() -> Value {
    json!({
        "name": "jira_update_issue",
        "description": "Update an issue",
        "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}},
    })
}

async fn fake_jira(body: Bytes) -> impl axum::response::IntoResponse {
    let msg: Value = serde_json::from_slice(&body).unwrap();
    let result = match msg["method"].as_str().unwrap_or_default() {
        "tools/list" => json!({"tools": [update_tool()]}),
        _ => json!({"content": [{"type": "text", "text": "done"}]}),
    };
    (
        [("mcp-session-id", "s-1")],
        Json(json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})),
    )
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

fn map() -> ServerMap {
    ServerMap {
        server: id("jira-mcp"),
        system: id("jira"),
        fold_case: true,
        tools: vec![ToolMap {
            tool: id("jira_update_issue"),
            digest: tool_digest(&update_tool()).unwrap(),
            operation: id("issue.update"),
            kind: Kind::Write,
            reversal: Reversal::Restore,
            outward: false,
            resource: ResourceTemplate::new("jira/project/{key:before(-)}/issue/{key}").unwrap(),
        }],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shim_keeps_its_listing_when_only_the_session_ran_out() {
    std::env::set_var("GOV_DATA_CLASS", "fake");
    let dir = tempfile::tempdir().unwrap();
    let upstream = serve(Router::new().route("/mcp", post(fake_jira))).await;
    let (issuer, agent, gate_key) = (key("issuer-1", 1), key("agent-1", 7), key("gate-1-key", 20));
    let now = gov_run::now();
    let mandate = seal(
        &Mandate {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            id: id("m-1"),
            issuer: id("test-issuer"),
            issued_at: now - 60,
            expires_at: now + 3600,
            agent: AgentRef {
                id: id("refund-bot"),
                key: agent.public(),
            },
            issued_by: IssuedBy {
                kind: IssuerKind::Person,
                id: id("ana"),
                session: Some(id("ses-1")),
            },
            template: id("triage@7"),
            grants: vec![Grant {
                system: id("jira"),
                operations: [id("issue.update")].into(),
                resources: vec![ResourcePattern::new("jira/project/pay/*").unwrap()],
            }],
            limits: Default::default(),
            requires: Default::default(),
            parent: None,
        },
        &issuer,
    )
    .unwrap();

    // A gate whose sessions are spent after one call.
    let mut sessions = SessionsConfig::new(scope(), id("gate-1"), KeySet::new([issuer.public()]));
    sessions.max_calls = 1;
    let gate = Gate::open(
        GateConfig {
            scope: scope(),
            gate: id("gate-1"),
            ledger_keys: KeySet::new([key("ledger-1", 50).public()]),
            spool_limit: 1000,
            batch: 100,
        },
        gate_key,
        FileSpool::open(dir.path().join("spool")).unwrap(),
        now,
    )
    .unwrap();
    let forwarder = Forwarder::new(
        gate,
        ForwarderConfig {
            upstream: format!("http://{upstream}/mcp"),
            map: map(),
            call_timeout: Duration::from_secs(5),
            sessions: Some(sessions),
            max_in_flight: MAX_IN_FLIGHT,
        },
    )
    .unwrap();
    let gate_addr = serve(forwarder.clone().router()).await;

    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let shim_addr = socket.local_addr().unwrap();
    let shim = Shim::new(
        ShimConfig {
            scope: scope(),
            gate: id("gate-1"),
            gate_url: format!("http://{gate_addr}"),
            key: agent,
            mandate,
            map: map(),
            secret: "key-test".into(),
            call_timeout: Duration::from_secs(5),
        },
        shim_addr,
    )
    .unwrap();
    tokio::spawn(async move { axum::serve(socket, shim.router()).await.unwrap() });

    let client = reqwest::Client::new();
    let send = |body: Value| {
        client
            .post(format!("http://{shim_addr}/mcp"))
            .header("content-type", "application/json")
            .header("x-gov-shim-key", "key-test")
            .header("mcp-session-id", "s-1")
            .body(body.to_string())
            .send()
    };
    let call = |n: u64| json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "jira_update_issue", "arguments": {"key": "PAY-1"}}});
    // The session is issued, and the tools listed, once.
    for method in ["initialize", "tools/list"] {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method});
        assert_eq!(send(body).await.unwrap().status().as_u16(), 200);
    }
    // First call: proven. Second: the session is spent, a failed proof; the gate still holds
    // the listing and says so. Third: a new session, and proven for the tool as it is
    // mapped, because the shim kept its listing too.
    for n in 2..=4 {
        assert_eq!(send(call(n)).await.unwrap().status().as_u16(), 200);
    }
    let counters = &forwarder.counters;
    assert_eq!(
        (
            counters.proven.load(Ordering::Relaxed),
            counters.proof_failed.load(Ordering::Relaxed)
        ),
        (2, 1)
    );
    let keys = KeySet::new([key("gate-1-key", 20).public()]);
    let receipts: Vec<Receipt> = FileSpool::open(dir.path().join("spool"))
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
        .collect();
    let last = &receipts[4];
    assert_eq!(
        (last.agent.proof, last.action.kind),
        (Proof::Proven, Kind::Write),
        "the tool is still the mapped one on both sides"
    );
}
