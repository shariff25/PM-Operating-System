//! Slice G4d-2: a gate that has an Authority (spec §7.6.3, points 4 to 6). The gate and the
//! shim run in one process; the Authority's answers are made by hand, so that the test says
//! exactly what the gate was told and when. An agent's calls are recorded as proven only
//! under a complete, fresh lease, stamped with the Authority's time, and as that agent's
//! failed proofs, with the reason, once its mandate is revoked.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Bytes;
use axum::routing::post;
use axum::{Json, Router};
use gov_core::action::{Grant, Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::lease::Lease;
use gov_core::mandate::{AgentRef, IssuedBy, IssuerKind, Mandate};
use gov_core::policy::{open_bundle, OnlineKeys, PolicyBundle};
use gov_core::receipt::{Proof, ProofFailure, Receipt};
use gov_core::renew::{Renew, RenewRefused, Renewed, Standing};
use gov_core::revocation::{Revocable, RevocationEntry, RevocationFeed};
use gov_core::{open, seal, Envelope, Id, KeySet, ResourcePattern, Scope, SigningKey};
use gov_gate::forward::{Forwarder, ForwarderConfig, FromAuthority, MAX_IN_FLIGHT};
use gov_gate::sessions::{why, SessionsConfig};
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
fn root() -> SigningKey {
    key("authority-root-1", 61)
}
fn mandates() -> SigningKey {
    key("authority:mandates-1", 71)
}
fn leases() -> SigningKey {
    key("authority:leases-1", 72)
}

/// The gate's own count of time, which the test moves.
static MONO: AtomicU64 = AtomicU64::new(1_000);
fn mono() -> u64 {
    MONO.load(Ordering::SeqCst)
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
        labels: Vec::new(),
        clearances: Vec::new(),
        server: id("jira-mcp"),
        system: id("jira"),
        fold_case: true,
        tools: vec![ToolMap {
            results: None,
            tool: id("jira_update_issue"),
            digest: tool_digest(&update_tool()).unwrap(),
            operation: id("issue.update"),
            kind: Kind::Write,
            reversal: Reversal::None,
            outward: true,
            resource: ResourceTemplate::new("jira/project/{key:before(-)}/issue/{key}").unwrap(),
        }],
    }
}

fn bundle() -> Envelope {
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id("v1"),
        number: 1,
        issued_at: 1_800_000_000,
        keys: OnlineKeys {
            mandates: vec![mandates().public()],
            leases: vec![leases().public()],
        },
        freshness: Default::default(),
        rules: Vec::new(),
        connectors: vec![map()],
        templates: Vec::new(),
    };
    seal(&bundle, &root()).unwrap()
}

/// What an Authority would answer `request` with, at its time `at`, with these revoked.
fn answer(request: &Envelope, gate: &SigningKey, at: i64, revoked: &[RevocationEntry]) -> Renewed {
    let asked = open::<Renew>(request, &KeySet::new([gate.public()]), &scope())
        .unwrap()
        .doc;
    let opened = open_bundle(&bundle(), &root().public(), &scope()).unwrap();
    let through = revoked.last().map_or(0, |e| e.seq);
    let lease = Lease {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        gate: asked.gate.clone(),
        nonce: asked.nonce.clone(),
        authority_time: at,
        policy_version: opened.policy_version(),
        revocation_seq: through,
        valid_for_secs: 120,
    };
    let entries: Vec<RevocationEntry> = revoked
        .iter()
        .filter(|e| e.seq > asked.revocations_through)
        .cloned()
        .collect();
    let feed = RevocationFeed {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        after: asked.revocations_through,
        through,
        dropped: 0,
        at,
        entries,
    };
    Renewed {
        lease: seal(&lease, &leases()).unwrap(),
        feed: seal(&feed, &leases()).unwrap(),
        bundle: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_calls_are_proven_only_while_the_gate_holds_a_fresh_lease_and_nothing_is_revoked()
{
    std::env::set_var("GOV_DATA_CLASS", "fake");
    let dir = tempfile::tempdir().unwrap();
    let upstream = serve(Router::new().route("/mcp", post(fake_jira))).await;
    let (agent, gate_key) = (key("refund-bot:k1", 7), key("gate-1:k1", 20));
    let wall = gov_run::now();
    // The Authority's clock is a minute ahead of this machine's: enough to tell them apart
    // on a receipt, and within what a session's opening allows.
    let authority_time = wall + 60;
    let mandate = seal(
        &Mandate {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            id: id("m-1"),
            issuer: id("authority"),
            issued_at: wall - 60,
            expires_at: wall + 3600,
            agent: AgentRef {
                id: id("refund-bot"),
                key: agent.public(),
            },
            issued_by: IssuedBy {
                kind: IssuerKind::Person,
                id: id("ana"),
                session: Some(id("ses-1")),
            },
            template: id("triage@1"),
            grants: vec![Grant {
                system: id("jira"),
                operations: [id("issue.update")].into(),
                resources: vec![ResourcePattern::new("jira/project/pay/*").unwrap()],
            }],
            limits: Default::default(),
            requires: Default::default(),
            parent: None,
        },
        &mandates(),
    )
    .unwrap();

    let opened = open_bundle(&bundle(), &root().public(), &scope()).unwrap();
    let policy_in_use = opened.policy_version();
    let standing = Standing::new(
        scope(),
        id("gate-1"),
        id("authority"),
        root().public(),
        Some(opened),
    );
    let gate = Gate::open(
        GateConfig {
            scope: scope(),
            gate: id("gate-1"),
            ledger_keys: KeySet::new([key("ledger-1", 50).public()]),
            spool_limit: 1000,
            batch: 100,
            policy_version: Some(policy_in_use.clone()),
        },
        key("gate-1:k1", 20),
        FileSpool::open(dir.path().join("spool")).unwrap(),
        wall,
    )
    .unwrap();
    let forwarder = Forwarder::new(
        gate,
        ForwarderConfig {
            authority: Some(FromAuthority {
                standing,
                mono,
                policy_in_use,
            }),
            upstream: format!("http://{upstream}/mcp"),
            map: map(),
            call_timeout: Duration::from_secs(5),
            sessions: Some(SessionsConfig::new(
                scope(),
                id("gate-1"),
                KeySet::new([mandates().public()]),
            )),
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
    let call = |n: u64| async move {
        let body = json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "jira_update_issue", "arguments": {"key": "PAY-1"}}});
        assert_eq!(send(body).await.unwrap().status().as_u16(), 200);
    };
    for method in ["initialize", "tools/list"] {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method});
        assert_eq!(send(body).await.unwrap().status().as_u16(), 200);
    }
    let renew = |at: i64, revoked: Vec<RevocationEntry>| {
        let forwarder = forwarder.clone();
        let gate_key = key("gate-1:k1", 20);
        async move {
            let (request, asked) = forwarder.renewal(&gate_key).unwrap().unwrap();
            let answer = answer(&request, &gate_key, at, &revoked);
            let taken = forwarder.renewed(&answer, asked).await.unwrap();
            (answer, taken)
        }
    };
    let _ = &gate_key;

    // 1. Before its first lease the gate opens no session: the call goes through, as every
    //    call does in observe mode, and is recorded as a client's bare claim, at this
    //    machine's time.
    call(10).await;

    // 2. A lease: the session opens, the call is proven, and the time is the Authority's.
    let (first_answer, taken) = renew(authority_time, Vec::new()).await;
    taken.unwrap();
    MONO.store(6_000, Ordering::SeqCst);
    call(20).await;

    // 3. The lease runs out (it is good for two minutes): the proof still holds and the
    //    call is recorded as the agent's failed proof, because the gate cannot say that
    //    nothing was revoked.
    MONO.store(1_000 + 121_000, Ordering::SeqCst);
    call(30).await;

    // 4. Renewed: proven again, under the same session.
    renew(authority_time + 125, Vec::new()).await.1.unwrap();
    call(40).await;

    // 5. The mandate is revoked. The session is kept as a marker: the next calls are the
    //    agent's, failed, and say what was revoked. The shim does not open another.
    let revoked = vec![RevocationEntry {
        seq: 1,
        kind: Revocable::Mandate,
        id: id("m-1"),
        until: wall + 3600,
    }];
    let (_, taken) = renew(authority_time + 130, revoked.clone()).await;
    assert_eq!(taken.unwrap().revoked, revoked);
    call(50).await;
    call(51).await;

    // 6. An answer the gate has already taken is not taken again, and is counted.
    let (_, asked) = forwarder.renewal(&key("gate-1:k1", 20)).unwrap().unwrap();
    assert_eq!(
        forwarder.renewed(&first_answer, asked).await.unwrap(),
        Err(RenewRefused::NotThisRequest)
    );
    assert_eq!(
        forwarder
            .counters
            .renew_refused
            .counts()
            .get("not_this_request"),
        Some(&1)
    );

    let keys = KeySet::new([key("gate-1:k1", 20).public()]);
    let receipts: Vec<Receipt> = FileSpool::open(dir.path().join("spool"))
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
        .collect();
    // An intent and an outcome for each of the six calls.
    assert_eq!(receipts.len(), 12);
    let said: Vec<(Proof, Option<ProofFailure>, bool)> = receipts
        .iter()
        .step_by(2)
        .map(|r| (r.agent.proof, r.agent.failure, r.at_from_lease))
        .collect();
    assert_eq!(
        said,
        vec![
            (Proof::Claimed, None, false),
            (Proof::Proven, None, true),
            (Proof::Failed, Some(ProofFailure::NoLease), true),
            (Proof::Proven, None, true),
            (Proof::Failed, Some(ProofFailure::MandateRevoked), true),
            (Proof::Failed, Some(ProofFailure::MandateRevoked), true),
        ]
    );
    // Every call under the session names the agent its mandate names, proven or not.
    for r in &receipts[2..] {
        assert_eq!(r.agent.id, id("refund-bot"));
        assert_eq!(r.mandate, vec![id("m-1")]);
    }
    // An outcome is stamped like its intent.
    for pair in receipts.chunks(2) {
        assert_eq!(pair[0].at_from_lease, pair[1].at_from_lease);
    }
    // The first call's time is this machine's. From the first lease on it is the
    // Authority's, a minute ahead, counted on by the gate's own clock: 5 seconds after the
    // lease was asked for, then 121, then from the second lease.
    assert!(
        (wall..wall + 30).contains(&receipts[0].at),
        "{}",
        receipts[0].at
    );
    assert_eq!(receipts[2].at, authority_time + 5);
    assert_eq!(receipts[4].at, authority_time + 121);
    assert_eq!(receipts[6].at, authority_time + 125);
    assert!(receipts.iter().all(|r| !r.at_clamped));

    // The operator's counts: refused openings by why, and what was and was not proven.
    let refused = forwarder.sessions_refused().unwrap();
    assert_eq!(refused.get(why::NO_LEASE), Some(&1));
    assert_eq!(refused.get(why::REVOKED), None, "the shim kept its session");
    let c = &forwarder.counters;
    assert_eq!(
        (
            c.proven.load(Ordering::Relaxed),
            c.proof_failed.load(Ordering::Relaxed)
        ),
        (2, 3)
    );
}

/// A lease is complete only while it names the bundle the gate is deciding from. A gate
/// whose map came from another bundle than its lease names has no complete lease, however
/// fresh: it opens no session, so nothing is recorded as proven under the wrong map.
#[tokio::test(flavor = "multi_thread")]
async fn a_lease_for_another_bundle_than_the_gate_decides_from_is_not_complete() {
    std::env::set_var("GOV_DATA_CLASS", "fake");
    let dir = tempfile::tempdir().unwrap();
    let upstream = serve(Router::new().route("/mcp", post(fake_jira))).await;
    let wall = gov_run::now();
    let opened = open_bundle(&bundle(), &root().public(), &scope()).unwrap();
    let build = |policy_in_use: Id, spool: &str| {
        let standing = Standing::new(
            scope(),
            id("gate-1"),
            id("authority"),
            root().public(),
            Some(opened.clone()),
        );
        let gate = Gate::open(
            GateConfig {
                scope: scope(),
                gate: id("gate-1"),
                ledger_keys: KeySet::new([key("ledger-1", 50).public()]),
                spool_limit: 1000,
                batch: 100,
                policy_version: Some(policy_in_use.clone()),
            },
            key("gate-1:k1", 20),
            FileSpool::open(dir.path().join(spool)).unwrap(),
            wall,
        )
        .unwrap();
        Forwarder::new(
            gate,
            ForwarderConfig {
                authority: Some(FromAuthority {
                    standing,
                    // This test's own clock, which stands still: the other test moves its.
                    mono: || 5,
                    policy_in_use,
                }),
                upstream: format!("http://{upstream}/mcp"),
                map: map(),
                call_timeout: Duration::from_secs(5),
                sessions: Some(SessionsConfig::new(
                    scope(),
                    id("gate-1"),
                    KeySet::new([mandates().public()]),
                )),
                max_in_flight: MAX_IN_FLIGHT,
            },
        )
        .unwrap()
    };
    let agent = key("refund-bot:k1", 7);
    let mandate = seal(
        &Mandate {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            id: id("m-1"),
            issuer: id("authority"),
            issued_at: wall - 60,
            expires_at: wall + 3600,
            agent: AgentRef {
                id: id("refund-bot"),
                key: agent.public(),
            },
            issued_by: IssuedBy {
                kind: IssuerKind::Person,
                id: id("ana"),
                session: Some(id("ses-1")),
            },
            template: id("triage@1"),
            grants: vec![Grant {
                system: id("jira"),
                operations: [id("issue.update")].into(),
                resources: vec![ResourcePattern::new("jira/project/pay/*").unwrap()],
            }],
            limits: Default::default(),
            requires: Default::default(),
            parent: None,
        },
        &mandates(),
    )
    .unwrap();
    let open_a_session = |forwarder: std::sync::Arc<Forwarder<FileSpool>>| {
        let mandate = mandate.clone();
        let agent = key("refund-bot:k1", 7);
        async move {
            let addr = serve(forwarder.clone().router()).await;
            let request = seal(
                &gov_core::request::SessionOpen {
                    v: 1,
                    tenant: id("acme"),
                    env: id("prod"),
                    gate: id("gate-1"),
                    mandate: id("m-1"),
                    nonce: gov_core::random_id("open").unwrap(),
                    at: gov_run::now(),
                },
                &agent,
            )
            .unwrap();
            reqwest::Client::new()
                .post(format!("http://{addr}/v1/sessions"))
                .json(&json!({"mandate": mandate, "request": request}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };

    for (policy_in_use, spool, opens) in [
        (opened.policy_version(), "same", true),
        (id("1-0000000000000000"), "other", false),
    ] {
        let forwarder = build(policy_in_use, spool);
        let gate_key = key("gate-1:k1", 20);
        let (request, asked) = forwarder.renewal(&gate_key).unwrap().unwrap();
        let answer = answer(&request, &gate_key, wall, &[]);
        forwarder.renewed(&answer, asked).await.unwrap().unwrap();
        let status = open_a_session(forwarder.clone()).await;
        assert_eq!(status == 200, opens, "{spool}: {status}");
        let refused = forwarder.sessions_refused().unwrap();
        assert_eq!(refused.get(why::NO_LEASE).is_some(), !opens, "{spool}");
    }
}
