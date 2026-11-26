//! Slice G3b: a gate ships to the Ledger's listener over HTTP, and the listener answers only
//! a gate it knows (review findings GR-20 and GR-55).

use std::sync::Arc;

use gov_core::action::{CanonicalAction, Kind, Reversal};
use gov_core::lease::{batch_digest, ShipRequest};
use gov_core::receipt::{Coverage, OutcomeStatus};
use gov_core::{seal, Id, KeySet, ResourcePath, Scope, SigningKey};
use gov_gate::link::HttpLedgerLink;
use gov_gate::{FileSpool, Gate, GateConfig, GateError, Observed, Shipped, Spool};
use gov_ledger::http::{router, Listener};
use gov_ledger::{Ledger, MemoryStore, Store};
use serde_json::json;

const T0: i64 = 1_800_000_000;

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

async fn listener() -> (String, Arc<Listener<MemoryStore>>) {
    let ledger = Ledger::new(MemoryStore::default(), scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate_key().public())
        .await
        .unwrap();
    let state = Arc::new(Listener {
        health: Default::default(),
        ledger,
        key: ledger_key(),
        issuer: id("ledger"),
        clock: || T0 + 5,
    });
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn gate(dir: &std::path::Path) -> Gate<FileSpool> {
    let config = GateConfig {
        scope: scope(),
        gate: id("gate-1"),
        ledger_keys: KeySet::new([ledger_key().public()]),
        spool_limit: 100,
        batch: 100,
        policy_version: None,
    };
    Gate::open(config, gate_key(), FileSpool::open(dir).unwrap(), T0).unwrap()
}

fn call(gate: &mut Gate<FileSpool>, n: u64) {
    let agent = id("triage-bot");
    let action = CanonicalAction::new(
        id("jira"),
        id("issue.update"),
        Kind::Write,
        ResourcePath::new("jira/project/pay/issue/pay-12").unwrap(),
        None,
        &json!({"n": n}),
        &[9; 32],
    )
    .unwrap();
    let observed = Observed {
        who: gov_gate::Who::claimed(agent.clone()),
        action: &action,
        reversal: Reversal::Restore,
        coverage: Coverage::InLine,
    };
    let intent = gate.begin(&observed, T0).unwrap();
    gate.finish(intent, OutcomeStatus::Ok, None, T0).unwrap();
}

#[tokio::test]
async fn a_gate_ships_over_http_and_the_ledger_answers_only_a_gate_it_knows() {
    let (base, state) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut g = gate(dir.path());
    call(&mut g, 1);
    call(&mut g, 2);
    let link = HttpLedgerLink::new(&base).unwrap();
    assert_eq!(g.ship(&link).await.unwrap(), Shipped::Advanced(4));
    assert_eq!(g.ship(&link).await.unwrap(), Shipped::NothingToSend);
    let head = state
        .ledger
        .store()
        .head(&scope(), &id("gate-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.seq, 4);

    // A Ledger that is not there.
    call(&mut g, 3);
    let nowhere = HttpLedgerLink::new("http://127.0.0.1:1").unwrap();
    assert!(matches!(g.ship(&nowhere).await, Err(GateError::Link(_))));

    // What a stranger can do with the address. The receipts are real ones, copied.
    let receipts: Vec<_> = FileSpool::open(dir.path())
        .unwrap()
        .held()
        .unwrap()
        .into_iter()
        .map(|r| r.envelope)
        .collect();
    let request = |key: &SigningKey, gate: &str, batch: &[gov_core::Envelope]| {
        seal(
            &ShipRequest {
                v: 1,
                tenant: id("acme"),
                env: id("prod"),
                gate: id(gate),
                nonce: id("n-1"),
                count: batch.len() as u64,
                batch: batch_digest(batch),
            },
            key,
        )
        .unwrap()
    };
    let http = reqwest::Client::new();
    let post = |body: serde_json::Value| {
        let http = http.clone();
        let url = format!("{base}/v1/receipts");
        async move {
            http.post(url)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    // Signed by a key that is not enrolled, under the gate's key id and under its own.
    let stranger = SigningKey::from_seed(id("gate-1-key"), &[99; 32]).unwrap();
    let other = SigningKey::from_seed(id("gate-9-key"), &[98; 32]).unwrap();
    for key in [&stranger, &other] {
        let body = json!({"request": request(key, "gate-1", &receipts), "receipts": receipts});
        assert_eq!(post(body).await, 401);
    }
    // No request at all, and a request that is not one.
    assert_eq!(post(json!({"receipts": receipts})).await, 422);
    // A real request, replayed with a different batch: it does not fit.
    let real = request(&gate_key(), "gate-1", &receipts[..2]);
    assert_eq!(
        post(json!({"request": real, "receipts": receipts[2..4]})).await,
        400
    );
    // A real request naming another gate.
    let crossed = request(&gate_key(), "gate-2", &receipts);
    assert_eq!(
        post(json!({"request": crossed, "receipts": receipts})).await,
        400
    );
    // More receipts than one shipment may hold.
    let many: Vec<_> = std::iter::repeat_n(receipts[0].clone(), 1001).collect();
    assert_eq!(
        post(json!({"request": request(&gate_key(), "gate-1", &many), "receipts": many})).await,
        413
    );

    // None of that left a trace: nothing was added to the chain, and the refusals are empty,
    // so a stranger cannot fill them.
    let store = state.ledger.store();
    assert_eq!(
        store
            .head(&scope(), &id("gate-1"))
            .await
            .unwrap()
            .unwrap()
            .seq,
        4
    );
    assert!(store.refusals(&scope()).await.unwrap().is_empty());

    // The gate itself carries on.
    assert_eq!(g.ship(&link).await.unwrap(), Shipped::Advanced(2));
}

/// The Ledger's answer is an acknowledgement and a word: the gate reads no more than that
/// could need, whatever is sent (review finding GR-78).
#[tokio::test]
async fn an_answer_too_large_to_be_an_answer_is_not_read() {
    let big = axum::Router::new().route(
        "/v1/receipts",
        axum::routing::post(|| async {
            format!(
                "{{\"ack\":null,\"refused\":\"{}\"}}",
                "x".repeat(1024 * 1024)
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, big).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut g = gate(dir.path());
    call(&mut g, 1);
    let link = HttpLedgerLink::new(&format!("http://{addr}")).unwrap();
    match g.ship(&link).await {
        Err(GateError::Link(why)) => assert!(why.contains("larger than an answer"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// A Ledger whose checkpoints keep failing says so on its health route (review finding
/// GR-87): receipts are arriving and nothing is witnessing them.
#[tokio::test]
async fn the_ledgers_health_shows_checkpoints_that_keep_failing() {
    use std::sync::atomic::Ordering;
    let state = Arc::new(Listener {
        health: Default::default(),
        ledger: Ledger::new(MemoryStore::default(), scope()),
        key: ledger_key(),
        issuer: id("ledger"),
        clock: || T0,
    });
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/healthz", tcp.local_addr().unwrap());
    let served = state.clone();
    tokio::spawn(async move { axum::serve(tcp, router(served)).await.unwrap() });

    let ok = reqwest::get(&url).await.unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    state.health.checkpoint_failures.store(2, Ordering::Relaxed);
    let failing = reqwest::get(&url).await.unwrap();
    assert_eq!(failing.status().as_u16(), 503);
    let body: serde_json::Value = failing.json().await.unwrap();
    assert_eq!(body["checkpoint_failures"], 2);
}
