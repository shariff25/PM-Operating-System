//! Slice G3b: the MCP forwarder in observe mode, between a real HTTP client and a small fake
//! Jira MCP server, with a real spool, Ledger and witness behind it.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use gov_core::action::{Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::receipt::{Coverage, Mode, OutcomeStatus, Phase, Proof, Receipt};
use gov_core::{open, Envelope, Id, KeySet, Scope, SigningKey};
use gov_gate::forward::{Forwarder, ForwarderConfig};
use gov_gate::{
    DirWitnessReader, FileSpool, Gate, GateConfig, LedgerLink, LinkAnswer, Shipped, Spool,
};
use gov_ledger::verify::{verify, Check};
use gov_ledger::{DirWitness, Ledger, MemoryStore, Witness};
use serde_json::{json, Value};

const SECRET: &str = "Bearer agent-jira-token-do-not-record";

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
fn gate_key() -> SigningKey {
    SigningKey::from_seed(id("gate-1-key"), &[20; 32]).unwrap()
}
fn ledger_key() -> SigningKey {
    SigningKey::from_seed(id("ledger-1"), &[50; 32]).unwrap()
}

// ----- a fake Jira MCP server: enough of the protocol to list and call tools --------------

fn update_tool() -> Value {
    json!({
        "name": "jira_update_issue",
        "description": "Update an issue",
        "inputSchema": {"type": "object", "properties": {"project": {"type": "string"}, "key": {"type": "string"}}},
    })
}
fn delete_tool() -> Value {
    json!({"name": "jira_delete_project", "description": "Delete a project", "inputSchema": {"type": "object"}})
}

/// What the fake server saw, so a test can say what did and did not reach it.
#[derive(Default)]
struct Upstream {
    hits: AtomicU64,
    last_body: Mutex<Vec<u8>>,
    last_accept_encoding: Mutex<Option<String>>,
}

async fn fake_jira(State(seen): State<Arc<Upstream>>, headers: HeaderMap, body: Bytes) -> Response {
    seen.hits.fetch_add(1, Ordering::SeqCst);
    *seen.last_body.lock().unwrap() = body.to_vec();
    *seen.last_accept_encoding.lock().unwrap() = headers
        .get("accept-encoding")
        .map(|v| v.to_str().unwrap().to_string());
    // As a real server does, it refuses a session id it never issued.
    let made_up = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|id| id.starts_with("made-up-"));
    if made_up {
        return (StatusCode::NOT_FOUND, "no such session").into_response();
    }
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(SECRET) {
        return (StatusCode::UNAUTHORIZED, "no token").into_response();
    }
    // The gate's own header must not reach the server.
    assert!(headers.get("x-gov-agent").is_none());
    let msg: Value = serde_json::from_slice(&body).unwrap();
    let rpc_id = msg["id"].clone();
    let result = match msg["method"].as_str().unwrap_or_default() {
        "initialize" => {
            json!({"protocolVersion": "2025-06-18", "serverInfo": {"name": "fake-jira"}, "capabilities": {"tools": {}}})
        }
        // A test asks for a listing with nothing in it through a header of its own.
        "tools/list" if headers.get("x-fake-list").is_some() => json!({"tools": []}),
        "tools/list" => json!({"tools": [update_tool(), delete_tool()]}),
        "tools/call" => {
            let args = &msg["params"]["arguments"];
            if args["key"] == "PAY-404" {
                json!({"isError": true, "content": [{"type": "text", "text": "no such issue"}]})
            } else {
                json!({"content": [{"type": "text", "text": "done: customer 4411 refunded"}]})
            }
        }
        _ => json!({}),
    };
    let reply = json!({"jsonrpc": "2.0", "id": rpc_id, "result": result});
    // The session id this server issues: the one a test asks for, or its usual one.
    let issued = headers
        .get("x-fake-issue")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("s-77")
        .to_string();
    let key = msg["params"]["arguments"]["key"]
        .as_str()
        .unwrap_or_default();
    match key {
        // Slow, but it answers.
        "PAY-SLOW" => tokio::time::sleep(Duration::from_millis(400)).await,
        // It never answers in any time the gate will wait.
        "PAY-HANG" => tokio::time::sleep(Duration::from_secs(30)).await,
        // It sends the client somewhere else.
        "PAY-307" => {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", "http://elsewhere.invalid/mcp")],
                "",
            )
                .into_response()
        }
        // More than the gate will hold.
        "PAY-BIG" => {
            return (
                [("content-type", "application/json")],
                "x".repeat(5 * 1024 * 1024),
            )
                .into_response()
        }
        _ => {}
    }
    // A server error that still carries a well-formed answer.
    if msg["params"]["arguments"]["key"] == "PAY-500" {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "application/json")],
            reply.to_string(),
        )
            .into_response();
    }
    // A server may answer a call with an event stream; this one does when asked to.
    if msg["params"]["arguments"]["stream"] == true {
        let sse = format!("event: message\ndata: {reply}\n\n");
        return (
            [
                ("content-type", "text/event-stream".to_string()),
                ("mcp-session-id", issued),
            ],
            sse,
        )
            .into_response();
    }
    (
        [
            ("content-type", "application/json".to_string()),
            ("mcp-session-id", issued),
        ],
        reply.to_string(),
    )
        .into_response()
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

fn jira_map() -> ServerMap {
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

struct Stack {
    forwarder: Arc<Forwarder<FileSpool>>,
    gate_url: String,
    upstream: Arc<Upstream>,
    dir: tempfile::TempDir,
    client: reqwest::Client,
}

async fn stack(spool_limit: usize) -> Stack {
    stack_holding(spool_limit, gov_gate::forward::MAX_IN_FLIGHT).await
}

/// A stack whose gate records at most `max_in_flight` calls at once.
async fn stack_holding(spool_limit: usize, max_in_flight: usize) -> Stack {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Upstream::default());
    let upstream = serve(
        Router::new()
            .route("/mcp", any(fake_jira))
            .with_state(seen.clone()),
    )
    .await;
    let config = GateConfig {
        scope: scope(),
        gate: id("gate-1"),
        ledger_keys: KeySet::new([ledger_key().public()]),
        spool_limit,
        batch: 100,
    };
    let spool = FileSpool::open(dir.path().join("spool")).unwrap();
    let gate = Gate::open(config, gate_key(), spool, 0).unwrap();
    let forwarder = Forwarder::new(
        gate,
        ForwarderConfig {
            upstream: format!("http://{upstream}/mcp"),
            map: jira_map(),
            call_timeout: Duration::from_secs(2),
            sessions: None,
            max_in_flight,
        },
    )
    .unwrap();
    let addr = serve(forwarder.clone().router()).await;
    Stack {
        forwarder,
        gate_url: format!("http://{addr}/mcp"),
        upstream: seen,
        dir,
        client: reqwest::Client::new(),
    }
}

impl Stack {
    async fn post(&self, body: Value, token: Option<&str>) -> (u16, String, String) {
        self.send(body.to_string(), token, &[]).await
    }
    /// The bytes as given, with extra headers.
    async fn send(
        &self,
        body: String,
        token: Option<&str>,
        extra: &[(&str, &str)],
    ) -> (u16, String, String) {
        let mut req = self
            .client
            .post(&self.gate_url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("x-gov-agent", "triage-bot");
        if let Some(t) = token {
            req = req.header("authorization", t);
        }
        for (name, value) in extra {
            req = req.header(*name, *value);
        }
        let res = req.body(body).send().await.unwrap();
        let status = res.status().as_u16();
        let session = res
            .headers()
            .get("mcp-session-id")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (status, session, res.text().await.unwrap())
    }
    async fn call(&self, n: u64, tool: &str, args: Value) -> (u16, String) {
        let body = json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let (status, _, text) = self.post(body, Some(SECRET)).await;
        (status, text)
    }
    async fn list(&self) {
        self.post(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            Some(SECRET),
        )
        .await;
    }
    fn hits(&self) -> u64 {
        self.upstream.hits.load(Ordering::SeqCst)
    }
    fn receipts(&self) -> Vec<Receipt> {
        let keys = KeySet::new([gate_key().public()]);
        FileSpool::open(self.dir.path().join("spool"))
            .unwrap()
            .held()
            .unwrap()
            .iter()
            .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
            .collect()
    }
}

/// A file's text, and with it the text of everything in it that is base64: a receipt's body
/// is carried encoded, so a search of the raw file alone would miss what is inside it.
fn laid_bare(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    fn strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        let mut found = Vec::new();
        strings(&value, &mut found);
        for s in found {
            if let Ok(decoded) = URL_SAFE_NO_PAD.decode(&s) {
                if std::str::from_utf8(&decoded).is_ok() {
                    text.push('\n');
                    text.push_str(&laid_bare(&decoded));
                }
            }
        }
    }
    text
}

struct InProcess(Ledger<MemoryStore>);
#[async_trait]
impl LedgerLink for InProcess {
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String> {
        let now = self.0.store_time();
        let (ack, refused) = self
            .0
            .take_shipment(request, batch, &ledger_key(), &id("ledger"), now)
            .await
            .map_err(|e| e.to_string())?;
        Ok(LinkAnswer {
            ack,
            refused: refused.map(str::to_string),
        })
    }
}
trait Now {
    fn store_time(&self) -> i64;
}
impl Now for Ledger<MemoryStore> {
    fn store_time(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}

#[tokio::test]
async fn every_tool_call_through_the_gate_leaves_receipts_and_is_changed_by_nothing() {
    let s = stack(100).await;

    // The handshake and the listing pass through untouched, session header and all.
    let (status, session, text) = s
        .post(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            Some(SECRET),
        )
        .await;
    assert_eq!((status, session.as_str()), (200, "s-77"));
    assert!(text.contains("fake-jira"));
    let (_, _, text) = s
        .post(
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            Some(SECRET),
        )
        .await;
    assert!(text.contains("jira_update_issue") && text.contains("jira_delete_project"));
    assert!(s.receipts().is_empty(), "only tool calls leave receipts");

    // A mapped write.
    let (status, text) = s
        .call(
            3,
            "jira_update_issue",
            json!({"project": "PAY", "key": "PAY-12", "summary": "Refund flow"}),
        )
        .await;
    assert_eq!(status, 200);
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["id"], 3);
    assert_eq!(
        reply["result"]["content"][0]["text"],
        "done: customer 4411 refunded"
    );
    // One the server answers with an error, one answered as an event stream, and one for a
    // tool the map does not know.
    let (_, text) = s
        .call(
            4,
            "jira_update_issue",
            json!({"project": "PAY", "key": "PAY-404"}),
        )
        .await;
    assert!(text.contains("no such issue"));
    let (_, text) = s
        .call(
            5,
            "jira_update_issue",
            json!({"project": "PAY", "key": "PAY-13", "stream": true}),
        )
        .await;
    assert!(text.starts_with("event: message\ndata: "), "{text}");
    let (_, text) = s
        .call(6, "jira_delete_project", json!({"project": "PAY"}))
        .await;
    assert!(text.contains("done"));

    let r = s.receipts();
    assert_eq!(r.len(), 8);
    for (i, receipt) in r.iter().enumerate() {
        // Observe mode, in the path only, by an agent that only said who it was.
        assert_eq!(receipt.mode, Mode::Observe);
        assert_eq!(receipt.coverage, Coverage::InLine);
        assert_eq!(
            (receipt.agent.id.as_str(), receipt.agent.proof),
            ("triage-bot", Proof::Claimed)
        );
        assert_eq!(
            receipt.phase,
            if i % 2 == 0 {
                Phase::Intent
            } else {
                Phase::Outcome
            }
        );
    }
    // What each call was comes from the signed map.
    assert_eq!(r[0].action.operation.as_str(), "issue.update");
    assert_eq!(r[0].action.kind, Kind::Write);
    assert_eq!(
        r[0].action.resource.as_str(),
        "jira/project/pay/issue/pay-12"
    );
    assert_eq!(r[0].reversal, Reversal::Restore);
    let outcomes: Vec<OutcomeStatus> = r
        .iter()
        .filter_map(|x| x.outcome.as_ref().map(|o| o.status))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            OutcomeStatus::Ok,
            OutcomeStatus::Failed,
            OutcomeStatus::Ok,
            OutcomeStatus::Ok
        ]
    );
    // The unmapped tool is recorded as unknown, with the worst assumed of it.
    assert_eq!(
        (r[6].action.kind, r[6].action.resource.as_str()),
        (Kind::Unknown, "jira/unmapped")
    );
    // Its name is not in the record, only a digest of it.
    assert!(r[6].action.operation.as_str().starts_with("unmapped-"));
    assert_eq!(r[6].reversal, Reversal::None);
    // The same arguments twice give different digests: each call has its own salt.
    assert_ne!(r[0].action.args_digest, r[2].action.args_digest);
    let c = &s.forwarder.counters;
    assert_eq!(
        (
            c.recorded.load(Ordering::Relaxed),
            c.not_recorded.load(Ordering::Relaxed),
            c.unmapped.load(Ordering::Relaxed)
        ),
        (4, 0, 1)
    );

    // Nothing the agent sent or the server answered is on the gate's disk: not the
    // credential, not an argument value, not the reply.
    let mut spooled = String::new();
    for entry in std::fs::read_dir(s.dir.path().join("spool")).unwrap() {
        spooled.push_str(&laid_bare(&std::fs::read(entry.unwrap().path()).unwrap()));
    }
    // The search reaches inside the receipts: what a receipt does hold is found.
    assert!(spooled.contains("triage-bot") && spooled.contains("issue.update"));
    for secret in [
        "agent-jira-token",
        "Refund flow",
        "customer 4411",
        "no such issue",
    ] {
        assert!(!spooled.contains(secret), "{secret} is in the spool");
    }

    // And the chain it wrote is one an auditor can check from files alone.
    let ledger = Ledger::new(MemoryStore::default(), scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate_key().public())
        .await
        .unwrap();
    let link = InProcess(ledger);
    assert_eq!(s.forwarder.ship(&link).await.unwrap(), Shipped::Advanced(8));
    let now = link.0.store_time();
    let witness = DirWitness::new(s.dir.path().join("witness"));
    link.0
        .checkpoint(&ledger_key(), &id("ledger"), now, &witness)
        .await
        .unwrap();
    let reader = DirWitnessReader::new(s.dir.path().join("witness"));
    assert_eq!(
        s.forwarder
            .gate()
            .await
            .trim_to_witness(&reader)
            .unwrap()
            .receipts,
        8
    );
    let report = verify(&Check {
        scope: &scope(),
        keys: &link.0.keys(&ledger_key()).await.unwrap(),
        export: &link.0.export().await.unwrap(),
        checkpoints: &witness.all().unwrap(),
        interval_secs: 60,
        now: Some(now),
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: None,
    });
    assert!(report.ok(), "{:?}", report.findings);
    assert_eq!(report.receipts_verified, 8);
}

/// A tool whose definition changes after it was mapped is no longer the mapped tool (T14),
/// and a tool called before any listing is not assumed to be.
#[tokio::test]
async fn a_tool_is_only_what_was_mapped_while_its_definition_is_the_one_reviewed() {
    let s = stack(100).await;
    // Called before the server has listed anything in this run.
    s.call(
        1,
        "jira_update_issue",
        json!({"project": "PAY", "key": "PAY-12"}),
    )
    .await;
    assert_eq!(s.receipts()[0].action.kind, Kind::Unknown);
    s.post(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        Some(SECRET),
    )
    .await;
    s.call(
        3,
        "jira_update_issue",
        json!({"project": "PAY", "key": "PAY-12"}),
    )
    .await;
    assert_eq!(s.receipts()[2].action.kind, Kind::Write);
}

/// Observe mode blocks nothing: a call the server refuses, a call when the spool is full and
/// a call when the server is gone all come back to the agent as the server (or its absence)
/// answered, and each is accounted for.
#[tokio::test]
async fn observe_mode_blocks_nothing_and_says_what_it_did_not_record() {
    let s = stack(4).await;
    s.post(
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        Some(SECRET),
    )
    .await;

    // No credential: the server's own refusal reaches the agent, and the outcome is failed.
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "jira_update_issue", "arguments": {"project": "PAY", "key": "PAY-1"}}});
    let (status, _, text) = s.post(body, None).await;
    assert_eq!((status, text.as_str()), (401, "no token"));
    assert_eq!(
        s.receipts()[1].outcome.as_ref().unwrap().status,
        OutcomeStatus::Failed
    );

    // A server error whose body still looks like a good answer is recorded as failed.
    let s5 = stack(100).await;
    s5.post(
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        Some(SECRET),
    )
    .await;
    let (status, _) = s5
        .call(
            2,
            "jira_update_issue",
            json!({"project": "PAY", "key": "PAY-500"}),
        )
        .await;
    assert_eq!(status, 500);
    assert_eq!(
        s5.receipts()[1].outcome.as_ref().unwrap().status,
        OutcomeStatus::Failed
    );

    // The spool holds four; the next call fits, the one after does not. It still goes
    // through, and is counted as not recorded.
    s.call(
        3,
        "jira_update_issue",
        json!({"project": "PAY", "key": "PAY-2"}),
    )
    .await;
    let (status, text) = s
        .call(
            4,
            "jira_update_issue",
            json!({"project": "PAY", "key": "PAY-3"}),
        )
        .await;
    assert_eq!(status, 200);
    assert!(text.contains("done"));
    assert_eq!(s.receipts().len(), 4);
    let c = &s.forwarder.counters;
    assert_eq!(
        (
            c.recorded.load(Ordering::Relaxed),
            c.not_recorded.load(Ordering::Relaxed)
        ),
        (2, 1)
    );

    // A batch could carry calls past the receipts; the gate does not take one, and nothing
    // is forwarded.
    let batch = json!([{"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "jira_delete_project", "arguments": {}}}]);
    let (status, _, text) = s.post(batch, Some(SECRET)).await;
    assert_eq!(status, 400);
    assert!(text.contains("batched"));
    assert_eq!(c.unmapped.load(Ordering::Relaxed), 0);

    // What was not recorded is still known after a restart (review finding GR-73).
    let state = FileSpool::open(s.dir.path().join("spool"))
        .unwrap()
        .state()
        .unwrap();
    assert_eq!(state.not_recorded, 1);
}

fn update(n: u64, key: &str) -> String {
    json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "jira_update_issue", "arguments": {"key": key}}}).to_string()
}

/// What the gate read is what the server gets (review finding GR-62). A body the gate cannot
/// read, or could read two ways, goes nowhere; and what is forwarded is the gate's own
/// writing-out of what it read.
#[tokio::test]
async fn the_server_never_reads_a_request_differently_from_the_gate() {
    let s = stack(100).await;
    s.list().await;
    let before = s.hits();
    for body in [
        // Two methods: a parser that takes the first sees a ping, one that takes the last
        // sees a tool call.
        r#"{"jsonrpc":"2.0","id":2,"method":"ping","method":"tools/call","params":{"name":"jira_delete_project","arguments":{}}}"#,
        // Two names inside the call.
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"jira_update_issue","name":"jira_delete_project","arguments":{}}}"#,
        // Two messages in one body.
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}{"jsonrpc":"2.0","id":3,"method":"tools/call"}"#,
        "not json at all",
        "",
    ] {
        let (status, _, _) = s.send(body.to_string(), Some(SECRET), &[]).await;
        assert_eq!(status, 400, "{body}");
    }
    assert_eq!(s.hits(), before, "none of them reached the server");
    assert!(s.receipts().is_empty());

    // A tool call written so that a careless reader would not see one: the gate reads it as
    // the call it is, records it, and the server is sent the plain form.
    let disguised = r#"{ "jsonrpc":"2.0", "id":7, "\u006dethod":"tools\/call", "params":{"name":"jira_update_issue","arguments":{"key":"PAY-12"}} }"#;
    let (status, _, _) = s.send(disguised.to_string(), Some(SECRET), &[]).await;
    assert_eq!(status, 200);
    assert_eq!(s.receipts().len(), 2);
    assert_eq!(s.receipts()[0].action.kind, Kind::Write);
    let sent = String::from_utf8(s.upstream.last_body.lock().unwrap().clone()).unwrap();
    assert!(sent.contains(r#""method":"tools/call""#), "{sent}");
    assert!(!sent.contains("\\u006d") && !sent.contains(r"\/"), "{sent}");

    // Only the three methods the protocol uses are taken, and a GET carries no body upstream.
    let put = s
        .client
        .put(&s.gate_url)
        .header("authorization", SECRET)
        .body(update(8, "PAY-12"))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status().as_u16(), 405);
    let hits = s.hits();
    s.client
        .get(&s.gate_url)
        .header("authorization", SECRET)
        .body(update(9, "PAY-12"))
        .send()
        .await
        .unwrap();
    assert_eq!(s.hits(), hits + 1);
    assert!(s.upstream.last_body.lock().unwrap().is_empty());
    assert_eq!(s.receipts().len(), 2);

    // A tool call with no id has no answer the gate could read: it is not taken (GR-70).
    let no_id = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"jira_update_issue","arguments":{"key":"PAY-12"}}}"#;
    let hits = s.hits();
    let (status, _, _) = s.send(no_id.to_string(), Some(SECRET), &[]).await;
    assert_eq!((status, s.hits()), (400, hits));

    // The gate asks the server for an answer it can read, whatever the client asked for
    // (GR-68).
    s.send(
        update(10, "PAY-12"),
        Some(SECRET),
        &[("accept-encoding", "gzip, br")],
    )
    .await;
    assert_eq!(
        s.upstream.last_accept_encoding.lock().unwrap().as_deref(),
        Some("identity")
    );
}

/// An intent is always closed (review finding GR-63): when the client goes away before the
/// server answers, and when the server never answers at all.
#[tokio::test]
async fn a_call_is_closed_whatever_becomes_of_the_client_or_the_server() {
    let s = stack(100).await;
    s.list().await;

    // The client gives up after 50 ms; the server answers after 400.
    let impatient = |ms: u64| {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(ms))
            .build()
            .unwrap();
        let url = s.gate_url.clone();
        async move {
            client
                .post(url)
                .header("authorization", SECRET)
                .body(update(2, "PAY-SLOW"))
                .send()
                .await
        }
    };
    assert!(impatient(50).await.is_err());
    tokio::time::sleep(Duration::from_millis(900)).await;
    let r = s.receipts();
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].outcome.as_ref().unwrap().status, OutcomeStatus::Ok);

    // Clients that give up at every moment of a call, some of them while the intent is
    // being written: whatever each reached, no intent is left without its outcome.
    let s2 = stack(1000).await;
    s2.list().await;
    for ms in [1, 2, 3, 5, 8, 12, 20, 35] {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(ms))
            .build()
            .unwrap();
        let _ = client
            .post(&s2.gate_url)
            .header("authorization", SECRET)
            .body(update(2, "PAY-SLOW"))
            .send()
            .await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let r2 = s2.receipts();
    assert!(!r2.is_empty() && r2.len() % 2 == 0, "{} receipts", r2.len());
    let intents: Vec<u64> = r2
        .iter()
        .filter(|x| x.phase == Phase::Intent)
        .map(|x| x.seq)
        .collect();
    for intent in &intents {
        assert!(
            r2.iter()
                .any(|x| x.phase == Phase::Outcome && x.intent == Some(*intent)),
            "intent {intent} was left open"
        );
    }

    // The server never answers: the gate gives up, tells the agent so, and records that it
    // does not know what the server did.
    let (status, _, _) = s.send(update(3, "PAY-HANG"), Some(SECRET), &[]).await;
    assert_eq!(status, 502);
    // A redirect is not passed on to the client (GR-69), and an answer larger than the gate
    // will hold is not read to the end (GR-67).
    for (n, key) in [(4, "PAY-307"), (5, "PAY-BIG")] {
        let res = s
            .client
            .post(&s.gate_url)
            .header("authorization", SECRET)
            .body(update(n, key))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 502, "{key}");
        assert!(res.headers().get("location").is_none());
    }
    let r = s.receipts();
    assert_eq!(r.len(), 8);
    for outcome in [&r[3], &r[5], &r[7]] {
        assert_eq!(
            outcome.outcome.as_ref().unwrap().status,
            OutcomeStatus::Unknown
        );
    }
    // Every intent has its outcome: nothing is left open.
    assert!(r.chunks(2).all(|pair| pair[0].phase == Phase::Intent
        && pair[1].phase == Phase::Outcome
        && pair[1].intent == Some(pair[0].seq)));
}

/// A listing belongs to the session it was made in, and the latest one is the one that
/// counts (review finding GR-64).
#[tokio::test]
async fn one_sessions_listing_is_not_anothers_and_a_dropped_tool_is_forgotten() {
    let s = stack(100).await;
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let kind = |i: usize| s.receipts()[i].action.kind;

    for session in ["a", "b"] {
        let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}});
        s.send(init.to_string(), Some(SECRET), &[("x-fake-issue", session)])
            .await;
    }
    // Listed in session a; called in session b, where the server may have shown another
    // definition, or none.
    s.send(list.clone(), Some(SECRET), &[("mcp-session-id", "a")])
        .await;
    s.send(update(2, "PAY-1"), Some(SECRET), &[("mcp-session-id", "b")])
        .await;
    assert_eq!(kind(0), Kind::Unknown);
    s.send(update(3, "PAY-1"), Some(SECRET), &[("mcp-session-id", "a")])
        .await;
    assert_eq!(kind(2), Kind::Write);

    // A second page adds to the listing: the tool from the first page is still known.
    let page_two =
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {"cursor": "p2"}})
            .to_string();
    s.send(
        page_two,
        Some(SECRET),
        &[("mcp-session-id", "a"), ("x-fake-list", "empty")],
    )
    .await;
    s.send(update(5, "PAY-1"), Some(SECRET), &[("mcp-session-id", "a")])
        .await;
    assert_eq!(kind(4), Kind::Write);

    // A fresh listing without the tool: the server has dropped it, and so has the gate.
    s.send(
        list,
        Some(SECRET),
        &[("mcp-session-id", "a"), ("x-fake-list", "empty")],
    )
    .await;
    s.send(update(6, "PAY-1"), Some(SECRET), &[("mcp-session-id", "a")])
        .await;
    assert_eq!(kind(6), Kind::Unknown);
}

/// A Ledger link that makes a tool call through the gate while the gate waits for it.
struct CallsWhileShipping<'a> {
    inner: InProcess,
    stack: &'a Stack,
}
#[async_trait]
impl LedgerLink for CallsWhileShipping<'_> {
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String> {
        let (status, _, _) = self
            .stack
            .send(update(50, "PAY-9"), Some(SECRET), &[])
            .await;
        assert_eq!(status, 200);
        self.inner.ship(request, batch).await
    }
}

/// Calls do not wait for the Ledger (review finding GR-67): the gate is not held while a
/// batch is on its way, so a call made in the middle of shipping goes through, and its
/// receipts go in the next batch.
#[tokio::test]
async fn a_slow_ledger_holds_up_no_call() {
    let s = stack(100).await;
    s.list().await;
    s.send(update(2, "PAY-1"), Some(SECRET), &[]).await;
    let ledger = Ledger::new(MemoryStore::default(), scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate_key().public())
        .await
        .unwrap();
    let link = CallsWhileShipping {
        inner: InProcess(ledger),
        stack: &s,
    };
    let shipped = tokio::time::timeout(Duration::from_secs(10), s.forwarder.ship(&link))
        .await
        .expect("the call inside the shipment did not wait for the shipment");
    assert_eq!(shipped.unwrap(), Shipped::Advanced(2));
    assert_eq!(s.receipts().len(), 4);
    assert_eq!(
        s.forwarder.ship(&link.inner).await.unwrap(),
        Shipped::Advanced(2)
    );
}

/// A link that takes its time.
struct Slow(InProcess);
#[async_trait]
impl LedgerLink for Slow {
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.0.ship(request, batch).await
    }
}

/// One round of shipping at a time (review finding GR-79): two rounds started together do
/// not send the same batch twice.
#[tokio::test]
async fn two_rounds_of_shipping_do_not_send_the_same_batch() {
    let s = stack(100).await;
    s.list().await;
    s.send(update(2, "PAY-1"), Some(SECRET), &[]).await;
    let ledger = Ledger::new(MemoryStore::default(), scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate_key().public())
        .await
        .unwrap();
    let link = Slow(InProcess(ledger));
    let (a, b) = tokio::join!(s.forwarder.ship(&link), s.forwarder.ship(&link));
    let mut both = vec![a.unwrap(), b.unwrap()];
    both.sort_by_key(|x| matches!(x, Shipped::NothingToSend));
    assert_eq!(both, vec![Shipped::Advanced(2), Shipped::NothingToSend]);
    assert_eq!(
        s.forwarder
            .counters
            .ship_no_progress
            .load(Ordering::Relaxed),
        0
    );
}

/// A listing is remembered only for a session id the server itself issued (review finding
/// GR-76), so a client making up session ids pushes nobody's listing out; and when the
/// server has issued more than the table holds, only the oldest is forgotten.
#[tokio::test]
async fn made_up_sessions_push_nobody_out_and_a_full_table_forgets_only_the_oldest() {
    let s = stack(100_000).await;
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}).to_string();
    // The server issues three sessions, and each lists the tools.
    for session in ["first", "second", "third"] {
        s.send(init.clone(), Some(SECRET), &[("x-fake-issue", session)])
            .await;
        s.send(list.clone(), Some(SECRET), &[("mcp-session-id", session)])
            .await;
    }
    // A client lists under more made-up ids than the table holds.
    for n in 0..gov_gate::forward::MAX_SESSIONS + 10 {
        let session = format!("made-up-{n}");
        s.send(list.clone(), Some(SECRET), &[("mcp-session-id", &session)])
            .await;
    }
    let kind_in = |session: &'static str, n: u64| {
        let s = &s;
        async move {
            s.send(
                update(n, "PAY-1"),
                Some(SECRET),
                &[("mcp-session-id", session)],
            )
            .await;
            s.receipts().last().unwrap().action.kind
        }
    };
    // Nothing was remembered for a made-up id, and nobody was pushed out.
    assert_eq!(kind_in("made-up-7", 2).await, Kind::Unknown);
    assert_eq!(kind_in("first", 3).await, Kind::Write);

    // Four sessions are issued so far: the three above, and the one this server names when
    // it is not asked for another. It now issues as many more as fill the table and one over.
    for n in 0..gov_gate::forward::MAX_SESSIONS - 3 {
        let session = format!("issued-{n}");
        s.send(init.clone(), Some(SECRET), &[("x-fake-issue", &session)])
            .await;
    }
    // "first" was the oldest and is forgotten; "second", issued just after it, is not.
    assert_eq!(kind_in("first", 4).await, Kind::Unknown);
    assert_eq!(kind_in("second", 5).await, Kind::Write);
}

/// A spool that takes its time to write a receipt, as a busy disk does.
struct SlowSpool(FileSpool);
impl Spool for SlowSpool {
    fn state(&self) -> Result<gov_gate::SpoolState, gov_gate::SpoolError> {
        self.0.state()
    }
    fn append(
        &mut self,
        receipt: &gov_gate::Spooled,
        state: &gov_gate::SpoolState,
    ) -> Result<(), gov_gate::SpoolError> {
        std::thread::sleep(Duration::from_millis(300));
        self.0.append(receipt, state)
    }
    fn close_intent(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.0.close_intent(seq)
    }
    fn set_state(&mut self, state: &gov_gate::SpoolState) -> Result<(), gov_gate::SpoolError> {
        self.0.set_state(state)
    }
    fn held(&self) -> Result<Vec<gov_gate::Spooled>, gov_gate::SpoolError> {
        self.0.held()
    }
    fn get(&self, seq: u64) -> Result<Option<gov_gate::Spooled>, gov_gate::SpoolError> {
        self.0.get(seq)
    }
    fn remove(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.0.remove(seq)
    }
    fn trim(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.0.trim(seq)
    }
    fn keep_checkpoint(&mut self, bytes: &[u8]) -> Result<(), gov_gate::SpoolError> {
        self.0.keep_checkpoint(bytes)
    }
    fn kept_checkpoint(&self) -> Result<Option<Vec<u8>>, gov_gate::SpoolError> {
        self.0.kept_checkpoint()
    }
    fn keep_alarm(&mut self, seq: u64, bytes: &[u8]) -> Result<(), gov_gate::SpoolError> {
        self.0.keep_alarm(seq, bytes)
    }
    fn kept_alarms(&self) -> Result<Vec<(u64, Vec<u8>)>, gov_gate::SpoolError> {
        self.0.kept_alarms()
    }
}

/// The client goes away while the intent receipt is still being written, before the call has
/// been sent at all. The call still goes, and the intent still gets its outcome: an intent
/// is never written by code the client can abandon (review finding GR-63; found by CI on
/// 2026-11-26, when a slower disk opened this window).
#[tokio::test]
async fn a_client_that_leaves_while_the_intent_is_written_leaves_no_open_intent() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Upstream::default());
    let upstream = serve(
        Router::new()
            .route("/mcp", any(fake_jira))
            .with_state(seen.clone()),
    )
    .await;
    let config = GateConfig {
        scope: scope(),
        gate: id("gate-1"),
        ledger_keys: KeySet::new([ledger_key().public()]),
        spool_limit: 100,
        batch: 100,
    };
    let spool = SlowSpool(FileSpool::open(dir.path().join("spool")).unwrap());
    let gate = Gate::open(config, gate_key(), spool, 0).unwrap();
    let forwarder = Forwarder::new(
        gate,
        ForwarderConfig {
            upstream: format!("http://{upstream}/mcp"),
            map: jira_map(),
            call_timeout: Duration::from_secs(2),
            sessions: None,
            max_in_flight: gov_gate::forward::MAX_IN_FLIGHT,
        },
    )
    .unwrap();
    let addr = serve(forwarder.router()).await;

    // The intent takes 300 ms to write; the client is gone after 50.
    let impatient = reqwest::Client::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let gone = impatient
        .post(format!("http://{addr}/mcp"))
        .header("authorization", SECRET)
        .body(update(2, "PAY-1"))
        .send()
        .await;
    assert!(gone.is_err());
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // The server was called, and both receipts are there.
    assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
    let keys = KeySet::new([gate_key().public()]);
    let receipts: Vec<Receipt> = FileSpool::open(dir.path().join("spool"))
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
        .collect();
    assert_eq!(receipts.len(), 2, "the intent was left open");
    assert_eq!(
        (receipts[0].phase, receipts[1].phase, receipts[1].intent),
        (Phase::Intent, Phase::Outcome, Some(1))
    );
}

/// No more calls are recorded at once than the gate will hold answers for (review finding
/// GR-99). Past that, observe mode still blocks nothing: the call goes through, and is
/// counted as not recorded.
#[tokio::test]
async fn calls_past_the_cap_go_through_and_are_counted_as_not_recorded() {
    let s = stack_holding(1000, 2).await;
    s.list().await;
    // Four calls at once to a server that takes 400 ms: two are recorded, two are not.
    let call = |n: u64| s.send(update(n, "PAY-SLOW"), Some(SECRET), &[]);
    let (a, b, c, d) = tokio::join!(call(2), call(3), call(4), call(5));
    for (status, _, _) in [a, b, c, d] {
        assert_eq!(status, 200);
    }
    let counters = &s.forwarder.counters;
    assert_eq!(
        (
            counters.recorded.load(Ordering::Relaxed),
            counters.not_recorded.load(Ordering::Relaxed)
        ),
        (2, 2)
    );
    assert_eq!(s.receipts().len(), 4);
    // The cap is on calls in flight, not on calls: once those are done, the next is recorded.
    assert_eq!(s.send(update(6, "PAY-1"), Some(SECRET), &[]).await.0, 200);
    assert_eq!(counters.recorded.load(Ordering::Relaxed), 3);
}

/// After a gate restart a session that is still running is never named by the server again:
/// servers name a session only when it starts. The gate takes a session as the server's own
/// when the server answers a request under it (review finding GR-113), so its listing is
/// remembered, while an id the server refuses is still nobody's.
#[tokio::test]
async fn a_session_the_server_answers_under_is_one_it_issued() {
    let s = stack(100).await;
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    // This gate never saw "running" issued: it was, before the gate started.
    s.send(list.clone(), Some(SECRET), &[("mcp-session-id", "running")])
        .await;
    // The server answered that listing, so the id is its own; the next listing is kept.
    s.send(list.clone(), Some(SECRET), &[("mcp-session-id", "running")])
        .await;
    s.send(
        update(2, "PAY-1"),
        Some(SECRET),
        &[("mcp-session-id", "running")],
    )
    .await;
    assert_eq!(s.receipts()[0].action.kind, Kind::Write);
    // An id the server refuses is not made its own by being asked about.
    for _ in 0..2 {
        s.send(
            list.clone(),
            Some(SECRET),
            &[("mcp-session-id", "made-up-1")],
        )
        .await;
    }
    assert!(!s.forwarder.holds_listing("made-up-1"));
}
