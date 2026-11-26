//! The Ledger, against both of its stores. Slice G2's "done when": removing, altering or
//! reordering a receipt is reported, and a Ledger or a gate that has gone quiet shows.

mod common;

use common::*;
use gov_core::lease::{Checkpoint, Pruned};
use gov_core::{seal, Envelope, Scope};
use gov_ledger::store::{StoreError, StoredReceipt};
use gov_ledger::verify::{verify, Check, Finding, Problem, Report};
use gov_ledger::witness::WitnessError;
use gov_ledger::{
    DirWitness, Export, Ingested, Ledger, LedgerError, MemoryStore, Store, TrustedKeys, Witness,
};

/// Run `$body` with a memory store, then with Postgres when it is configured.
macro_rules! both_stores {
    ($name:ident, $body:ident) => {
        #[tokio::test]
        async fn $name() {
            $body(MemoryStore::default()).await;
            if let Some(mut pg) = TestPg::new().await {
                $body(pg.store.take().unwrap()).await;
                pg.drop_schema().await;
            }
        }
    };
}

async fn ledger_with_gates<S: Store>(store: S) -> Ledger<S> {
    let ledger = Ledger::new(store, scope());
    for (gate, kid, seed) in [("gate-1", "gate-1-key", 20), ("gate-2", "gate-2-key", 30)] {
        ledger
            .store()
            .add_gate_key(&scope(), &id(gate), &key(kid, seed).public())
            .await
            .unwrap();
    }
    ledger
}

fn refused(r: Result<Ingested, LedgerError>) -> &'static str {
    match r {
        Err(LedgerError::Refused(code)) => code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// ----- intake ---------------------------------------------------------------------------

async fn intake<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let g1 = key("gate-1-key", 20);
    let g2 = key("gate-2-key", 30);
    let c1 = chain("gate-1", &g1, 4, T0, 1);
    let c2 = chain("gate-2", &g2, 2, T0, 1);

    assert_eq!(ledger.ingest(&c1[0], T0).await.unwrap(), Ingested::Added);
    assert_eq!(ledger.ingest(&c2[0], T0).await.unwrap(), Ingested::Added);
    assert_eq!(ledger.ingest(&c1[1], T0).await.unwrap(), Ingested::Added);
    // A batch sent twice.
    assert_eq!(
        ledger.ingest(&c1[0], T0).await.unwrap(),
        Ingested::AlreadyHeld
    );
    assert_eq!(
        ledger.ingest(&c1[1], T0).await.unwrap(),
        Ingested::AlreadyHeld
    );
    // A gap: 4 before 3.
    assert_eq!(refused(ledger.ingest(&c1[3], T0).await), "broken_chain");
    // A fork: another receipt 2, validly signed by the gate.
    let mut other = receipt("gate-1", 2, Some(hash_of(&c1[0], &g1)), T0 + 9);
    other.outcome.as_mut().unwrap().upstream_ref = Some(id("PAY-13"));
    assert_eq!(
        refused(ledger.ingest(&seal(&other, &g1).unwrap(), T0).await),
        "fork"
    );
    // A key nobody enrolled, and an enrolled key id with the wrong key behind it.
    let stranger = key("gate-9-key", 99);
    assert_eq!(
        refused(
            ledger
                .ingest(&chain("gate-9", &stranger, 1, T0, 1)[0], T0)
                .await
        ),
        "unknown_key"
    );
    let impostor = key("gate-1-key", 98);
    assert_eq!(
        refused(
            ledger
                .ingest(&chain("gate-1", &impostor, 1, T0, 1)[0], T0)
                .await
        ),
        "bad_signature"
    );
    // Gate 2's key signing into gate 1's chain.
    let crossed = seal(&receipt("gate-1", 3, Some(hash_of(&c1[1], &g1)), T0), &g2).unwrap();
    assert_eq!(refused(ledger.ingest(&crossed, T0).await), "wrong_gate");
    // Another tenant's receipt.
    let mut foreign = receipt("gate-1", 3, Some(hash_of(&c1[1], &g1)), T0);
    foreign.tenant = id("globex");
    assert_eq!(
        refused(ledger.ingest(&seal(&foreign, &g1).unwrap(), T0).await),
        "wrong_tenant"
    );
    // A receipt that breaks its own rules.
    let mut broken = receipt("gate-1", 3, Some(hash_of(&c1[1], &g1)), T0);
    broken.outcome = c_outcome();
    assert_eq!(
        refused(ledger.ingest(&seal(&broken, &g1).unwrap(), T0).await),
        "malformed"
    );

    // The chain itself is untouched by all of that, and goes on.
    assert_eq!(ledger.ingest(&c1[2], T0).await.unwrap(), Ingested::Added);
    assert_eq!(ledger.ingest(&c1[3], T0).await.unwrap(), Ingested::Added);
    assert_eq!(
        ledger
            .store()
            .head(&scope(), &id("gate-1"))
            .await
            .unwrap()
            .unwrap()
            .seq,
        4
    );

    // Every refusal is recorded; the envelope is kept only where its signature held.
    let refusals = ledger.store().refusals(&scope()).await.unwrap();
    let codes: Vec<(&str, bool)> = refusals
        .iter()
        .map(|r| (r.code.as_str(), r.envelope.is_some()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("broken_chain", true),
            ("fork", true),
            ("unknown_key", false),
            ("bad_signature", false),
            ("wrong_gate", true),
            ("wrong_tenant", false),
            ("malformed", true),
        ]
    );
}
both_stores!(intake_takes_a_chain_and_refuses_everything_else, intake);

fn c_outcome() -> Option<gov_core::receipt::Outcome> {
    Some(gov_core::receipt::Outcome {
        status: gov_core::receipt::OutcomeStatus::Ok,
        upstream_ref: None,
        response_digest: None,
    })
}

async fn isolation<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let g1 = key("gate-1-key", 20);
    for env in chain("gate-1", &g1, 2, T0, 1) {
        ledger.ingest(&env, T0).await.unwrap();
    }
    let store = ledger.store();
    for other in [
        Scope::new("globex", "prod").unwrap(),
        Scope::new("acme", "test").unwrap(),
    ] {
        assert!(store
            .gate_key(&other, &id("gate-1-key"))
            .await
            .unwrap()
            .is_none());
        assert!(store.gate_keys(&other).await.unwrap().is_empty());
        assert!(store.head(&other, &id("gate-1")).await.unwrap().is_none());
        assert!(store.heads(&other).await.unwrap().is_empty());
        assert!(store
            .receipts(&other, &id("gate-1"))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .receipt(&other, &id("gate-1"), 1)
            .await
            .unwrap()
            .is_none());
        assert!(store.refusals(&other).await.unwrap().is_empty());
        assert!(store.last_checkpoint(&other).await.unwrap().is_none());
        assert!(store.pruned(&other, &id("gate-1")).await.unwrap().is_none());
        // The same gate and number in another tenant is another chain.
        let mine = store
            .receipt(&scope(), &id("gate-1"), 1)
            .await
            .unwrap()
            .unwrap();
        store.append(&other, &id("gate-1"), 0, &mine).await.unwrap();
        assert_eq!(
            store
                .head(&scope(), &id("gate-1"))
                .await
                .unwrap()
                .unwrap()
                .seq,
            2
        );
    }
}
both_stores!(
    one_tenant_or_environment_never_sees_anothers_rows,
    isolation
);

async fn stale_head<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let g1 = key("gate-1-key", 20);
    let c = chain("gate-1", &g1, 2, T0, 1);
    ledger.ingest(&c[0], T0).await.unwrap();
    let second = StoredReceipt {
        seq: 2,
        hash: hash_of(&c[1], &g1),
        at: T0,
        envelope: c[1].clone(),
    };
    // A writer that read the head before receipt 1 arrived.
    let r = ledger
        .store()
        .append(&scope(), &id("gate-1"), 0, &second)
        .await;
    assert!(matches!(r, Err(StoreError::HeadMoved)), "{r:?}");
    ledger
        .store()
        .append(&scope(), &id("gate-1"), 1, &second)
        .await
        .unwrap();
    let r = ledger
        .store()
        .append(&scope(), &id("gate-1"), 1, &second)
        .await;
    assert!(matches!(r, Err(StoreError::HeadMoved)), "{r:?}");
}
both_stores!(a_writer_with_a_stale_head_cannot_append, stale_head);

// ----- checkpoints and verification -----------------------------------------------------

struct Stack<S> {
    ledger: Ledger<S>,
    witness_dir: tempfile::TempDir,
    keys: TrustedKeys,
}

/// Two gates, six and three receipts, three checkpoints a minute apart, then one more receipt
/// on gate 1 that no checkpoint covers yet.
async fn stack<S: Store>(store: S) -> Stack<S> {
    let ledger = ledger_with_gates(store).await;
    let witness_dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(witness_dir.path());
    let lk = ledger_key();
    let g1 = key("gate-1-key", 20);
    let g2 = key("gate-2-key", 30);
    let c1 = chain("gate-1", &g1, 7, T0, 10);
    let c2 = chain("gate-2", &g2, 3, T0, 10);
    for env in c1[..3].iter().chain(&c2[..3]) {
        ledger.ingest(env, T0).await.unwrap();
    }
    ledger
        .checkpoint(&lk, &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    for env in &c1[3..6] {
        ledger.ingest(env, T0).await.unwrap();
    }
    ledger
        .checkpoint(&lk, &id("ledger"), T0 + 120, &witness)
        .await
        .unwrap();
    ledger
        .checkpoint(&lk, &id("ledger"), T0 + 180, &witness)
        .await
        .unwrap();
    ledger.ingest(&c1[6], T0).await.unwrap();
    let keys = ledger.keys(&lk).await.unwrap();
    Stack {
        ledger,
        witness_dir,
        keys,
    }
}

fn check(
    keys: &TrustedKeys,
    export: &Export,
    checkpoints: &[(u64, Vec<u8>)],
    now: Option<i64>,
) -> Report {
    verify(&Check {
        scope: &scope(),
        keys,
        export,
        checkpoints,
        interval_secs: 60,
        now,
        gate_quiet_secs: None,
    })
}

fn problems(report: &Report) -> Vec<(Problem, Option<&str>, Option<u64>)> {
    report
        .findings
        .iter()
        .map(|Finding { problem, gate, seq }| {
            (problem.clone(), gate.as_ref().map(|g| g.as_str()), *seq)
        })
        .collect()
}

async fn untouched<S: Store>(store: S) {
    let s = stack(store).await;
    let export = s.ledger.export().await.unwrap();
    let cps = DirWitness::new(s.witness_dir.path()).all().unwrap();
    assert_eq!(cps.len(), 3);
    let report = check(&s.keys, &export, &cps, Some(T0 + 200));
    assert!(report.ok(), "{:?}", report.findings);
    assert_eq!(report.receipts_verified, 10);
    assert_eq!(report.checkpoints_verified, 3);
    let g1 = &report.gates[0];
    assert_eq!(
        (g1.gate.as_str(), g1.verified, g1.unwitnessed),
        ("gate-1", Some((1, 7)), 1)
    );
    // Gate 1 last moved at the second checkpoint, gate 2 at the first.
    assert_eq!(g1.last_moved_at, Some(T0 + 120));
    assert_eq!(report.gates[1].last_moved_at, Some(T0 + 60));
    // The export and the keys survive a trip through JSON, as the command reads them.
    let export2: Export = serde_json::from_str(&serde_json::to_string(&export).unwrap()).unwrap();
    let keys2: TrustedKeys =
        serde_json::from_str(&serde_json::to_string(&s.keys).unwrap()).unwrap();
    assert!(check(&keys2, &export2, &cps, None).ok());
}
both_stores!(an_untouched_export_verifies, untouched);

/// The attacks of threat T10, each made on a good export or a good witness.
async fn tampering<S: Store>(store: S) {
    let s = stack(store).await;
    let good = s.ledger.export().await.unwrap();
    let cps = DirWitness::new(s.witness_dir.path()).all().unwrap();
    let g1 = key("gate-1-key", 20);
    let run = |export: &Export| check(&s.keys, export, &cps, None);

    // Removed from the middle.
    let mut e = good.clone();
    e.gates[0].receipts.remove(2);
    assert!(problems(&run(&e)).contains(&(Problem::BrokenChain, Some("gate-1"), Some(3))));

    // Removed from the end, back past what a checkpoint covers.
    let mut e = good.clone();
    e.gates[0].receipts.truncate(4);
    assert_eq!(
        problems(&run(&e)),
        vec![
            (Problem::ReceiptMissing, Some("gate-1"), Some(6)),
            (Problem::ReceiptMissing, Some("gate-1"), Some(6)),
        ]
    );

    // Reordered.
    let mut e = good.clone();
    e.gates[0].receipts.swap(1, 2);
    assert!(problems(&run(&e)).contains(&(Problem::BrokenChain, Some("gate-1"), Some(2))));

    // Altered: one character of a payload.
    let mut e = good.clone();
    let p = &mut e.gates[1].receipts[1].payload;
    let flipped = if p.ends_with('A') { "B" } else { "A" };
    p.replace_range(p.len() - 1.., flipped);
    let report = run(&e);
    let found = problems(&report);
    assert!(
        found
            .iter()
            .any(|(p, g, n)| matches!(p, Problem::BadReceipt(_))
                && *g == Some("gate-2")
                && *n == Some(2)),
        "{found:?}"
    );

    // Replaced by the gate itself: a different receipt 6, validly signed and correctly linked.
    // Only the witness can show it.
    let mut e = good.clone();
    let prev = hash_of(&e.gates[0].receipts[4], &g1);
    let mut rewritten = receipt("gate-1", 6, Some(prev), T0 + 50);
    rewritten.outcome.as_mut().unwrap().upstream_ref = Some(id("PAY-99"));
    e.gates[0].receipts[5] = seal(&rewritten, &g1).unwrap();
    e.gates[0].receipts.truncate(6);
    assert_eq!(
        problems(&run(&e)),
        vec![
            (Problem::CheckpointMismatch, Some("gate-1"), Some(6)),
            (Problem::CheckpointMismatch, Some("gate-1"), Some(6)),
        ]
    );

    // A whole gate left out.
    let mut e = good.clone();
    e.gates.remove(1);
    assert!(problems(&run(&e)).contains(&(Problem::ReceiptMissing, Some("gate-2"), Some(3))));

    // A gate the verifier has no key for.
    let mut keys = s.keys.clone();
    keys.gates.remove(&id("gate-2"));
    assert!(problems(&check(&keys, &good, &cps, None)).contains(&(
        Problem::UnknownGate,
        Some("gate-2"),
        None
    )));

    // A checkpoint taken out of the witness.
    let without_second: Vec<_> = cps.iter().filter(|(n, _)| *n != 2).cloned().collect();
    assert!(
        problems(&check(&s.keys, &good, &without_second, None)).contains(&(
            Problem::CheckpointMissing,
            None,
            Some(2)
        ))
    );

    // A checkpoint the Ledger's key did not sign, and one stored under another number.
    let forged_key = key("ledger-1", 51);
    let forged: Envelope = serde_json::from_slice(&cps[2].1).unwrap();
    let doc = gov_core::open::<Checkpoint>(
        &forged,
        &gov_core::KeySet::new([ledger_key().public()]),
        &scope(),
    )
    .unwrap()
    .doc;
    let mut forged_cps = cps.clone();
    forged_cps[2].1 = serde_json::to_vec(&seal(&doc, &forged_key).unwrap()).unwrap();
    assert!(
        problems(&check(&s.keys, &good, &forged_cps, None)).contains(&(
            Problem::BadCheckpoint("bad_signature"),
            None,
            Some(3)
        ))
    );
    let mut renumbered = cps.clone();
    renumbered[2].1 = cps[1].1.clone();
    assert!(
        problems(&check(&s.keys, &good, &renumbered, None)).contains(&(
            Problem::BadCheckpoint("malformed"),
            None,
            Some(3)
        ))
    );

    // A later checkpoint that shows a shorter chain: history rewritten in the Ledger.
    let mut shorter = doc.clone();
    shorter.seq = 4;
    shorter.at = T0 + 240;
    shorter.heads[0].seq = 3;
    shorter.heads[0].hash = hash_of(&good.gates[0].receipts[2], &g1);
    let mut rolled = cps.clone();
    rolled.push((
        4,
        serde_json::to_vec(&seal(&shorter, &ledger_key()).unwrap()).unwrap(),
    ));
    assert!(problems(&check(&s.keys, &good, &rolled, None)).contains(&(
        Problem::HeadWentBack,
        Some("gate-1"),
        Some(4)
    )));
}
both_stores!(
    a_removed_reordered_altered_or_replaced_receipt_is_reported,
    tampering
);

async fn silence<S: Store>(store: S) {
    let s = stack(store).await;
    let witness = DirWitness::new(s.witness_dir.path());
    let export = s.ledger.export().await.unwrap();
    let cps = witness.all().unwrap();

    // The Ledger stopped publishing: seen from outside, with nothing but the witness.
    assert!(check(&s.keys, &export, &cps, Some(T0 + 180 + 120)).ok());
    assert_eq!(
        problems(&check(&s.keys, &export, &cps, Some(T0 + 180 + 121))),
        vec![(Problem::LedgerSilent, None, None)]
    );
    // It came back after a gap: the gap stays visible.
    s.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 600, &witness)
        .await
        .unwrap();
    let cps = witness.all().unwrap();
    let export = s.ledger.export().await.unwrap();
    assert_eq!(
        problems(&check(&s.keys, &export, &cps, Some(T0 + 610))),
        vec![(Problem::LedgerSilent, None, Some(4))]
    );

    // A gate gone quiet: checkpoints keep coming and its head does not move.
    let report = verify(&Check {
        scope: &scope(),
        keys: &s.keys,
        export: &export,
        checkpoints: &cps,
        interval_secs: 600,
        now: Some(T0 + 610),
        gate_quiet_secs: Some(300),
    });
    assert_eq!(
        problems(&report),
        vec![(Problem::GateQuiet, Some("gate-2"), None)]
    );
    assert_eq!(report.gates[1].last_moved_at, Some(T0 + 60));
    assert_eq!(report.gates[0].last_moved_at, Some(T0 + 600));
}
both_stores!(a_ledger_or_a_gate_that_goes_quiet_shows, silence);

// ----- the witness ----------------------------------------------------------------------

#[test]
fn the_witness_never_rewrites_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let w = DirWitness::new(dir.path().join("witness"));
    assert!(w.all().unwrap().is_empty());
    w.put(1, b"first").unwrap();
    w.put(1, b"first").unwrap();
    assert!(matches!(w.put(1, b"other"), Err(WitnessError::Conflict(1))));
    w.put(2, b"second").unwrap();
    assert_eq!(
        w.all().unwrap(),
        vec![(1, b"first".to_vec()), (2, b"second".to_vec())]
    );
    let file = dir
        .path()
        .join("witness")
        .join(format!("checkpoint-{:020}.json", 1));
    assert!(std::fs::metadata(file).unwrap().permissions().readonly());
}

async fn crash_between_store_and_witness<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let g1 = key("gate-1-key", 20);
    ledger
        .ingest(&chain("gate-1", &g1, 1, T0, 1)[0], T0)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    // The first checkpoint is stored but its witness write fails: the directory is a file.
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"").unwrap();
    let r = ledger
        .checkpoint(
            &ledger_key(),
            &id("ledger"),
            T0 + 60,
            &DirWitness::new(&blocked),
        )
        .await;
    assert!(
        matches!(r, Err(LedgerError::Witness(_))),
        "{:?}",
        r.map(|_| ())
    );
    // The next run publishes it before making the next one.
    let witness = DirWitness::new(dir.path().join("w"));
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 120, &witness)
        .await
        .unwrap();
    let cps = witness.all().unwrap();
    assert_eq!(cps.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![1, 2]);
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    assert!(check(&keys, &export, &cps, None).ok());
}
both_stores!(
    a_checkpoint_stored_but_not_witnessed_goes_out_next,
    crash_between_store_and_witness
);

// ----- pruning --------------------------------------------------------------------------

async fn pruning<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    let lk = ledger_key();
    let g1 = key("gate-1-key", 20);
    // Ten receipts, one a day.
    let day = 86_400;
    let c = chain("gate-1", &g1, 10, T0, day);
    let now = T0 + 10 * day;

    // Nothing is pruned before a checkpoint covers it.
    for env in &c[..6] {
        ledger.ingest(env, now).await.unwrap();
    }
    assert_eq!(
        ledger
            .prune(&id("gate-1"), now, &lk, &id("ledger"), now)
            .await
            .unwrap(),
        None
    );
    ledger
        .checkpoint(&lk, &id("ledger"), now, &witness)
        .await
        .unwrap();
    for env in &c[6..] {
        ledger.ingest(env, now).await.unwrap();
    }
    // Everything is older than the cutoff, but only six receipts are witnessed.
    assert_eq!(
        ledger
            .prune(&id("gate-1"), now + day, &lk, &id("ledger"), now)
            .await
            .unwrap(),
        Some(6)
    );
    ledger
        .checkpoint(&lk, &id("ledger"), now + 60, &witness)
        .await
        .unwrap();
    // A cutoff inside the chain: receipts 7 and 8 are older than day 8, 9 is not.
    assert_eq!(
        ledger
            .prune(&id("gate-1"), T0 + 8 * day, &lk, &id("ledger"), now)
            .await
            .unwrap(),
        Some(8)
    );
    assert_eq!(
        ledger
            .prune(&id("gate-1"), T0 + 8 * day, &lk, &id("ledger"), now)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        ledger
            .prune(&id("gate-2"), now, &lk, &id("ledger"), now)
            .await
            .unwrap(),
        None
    );

    let export = ledger.export().await.unwrap();
    assert_eq!(export.gates[0].receipts.len(), 2);
    let keys = ledger.keys(&lk).await.unwrap();
    let cps = witness.all().unwrap();
    let report = check(&keys, &export, &cps, None);
    assert!(report.ok(), "{:?}", report.findings);
    assert_eq!(report.gates[0].verified, Some((9, 10)));
    // The head is where it was, and the chain goes on from it.
    assert_eq!(
        ledger
            .store()
            .head(&scope(), &id("gate-1"))
            .await
            .unwrap()
            .unwrap()
            .seq,
        10
    );
    let next = seal(&receipt("gate-1", 11, Some(hash_of(&c[9], &g1)), now), &g1).unwrap();
    assert_eq!(ledger.ingest(&next, now).await.unwrap(), Ingested::Added);
    // A receipt from the pruned stretch, sent again, cannot be checked any more.
    assert_eq!(refused(ledger.ingest(&c[2], now).await), "already_pruned");

    // A prune record that lies about where the chain was cut does not fit the next receipt.
    let mut lying = export.clone();
    let record = Pruned {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("ledger"),
        gate: id("gate-1"),
        through_seq: 8,
        hash: hash_of(&c[0], &g1),
        at: now,
    };
    lying.gates[0].pruned = Some(seal(&record, &lk).unwrap());
    assert!(problems(&check(&keys, &lying, &cps, None)).contains(&(
        Problem::BrokenChain,
        Some("gate-1"),
        Some(9)
    )));
    // Nor does one that claims more was pruned than the export then shows.
    let mut hiding = export.clone();
    hiding.gates[0].pruned = None;
    assert!(problems(&check(&keys, &hiding, &cps, None)).contains(&(
        Problem::BrokenChain,
        Some("gate-1"),
        Some(1)
    )));
    // Nor one the Ledger's key did not sign.
    let mut forged = export.clone();
    forged.gates[0].pruned = Some(seal(&record, &g1).unwrap());
    assert!(problems(&check(&keys, &forged, &cps, None))
        .iter()
        .any(|(p, _, _)| matches!(p, Problem::BadPruneRecord(_))));
}
both_stores!(pruning_keeps_the_chain_verifiable, pruning);
