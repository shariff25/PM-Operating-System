//! The Ledger, against both of its stores. Slice G2's "done when": removing, altering or
//! reordering a receipt is reported, and a Ledger or a gate that has gone quiet shows.

mod common;

use common::*;
use gov_core::lease::{Checkpoint, Pruned};
use gov_core::{seal, Envelope, Scope, SigningKey};
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
            .enrol_gate_key(&id(gate), &key(kid, seed).public())
            .await
            .unwrap();
    }
    ledger
}

async fn ledger_with_gate_1<S: Store>(store: S) -> Ledger<S> {
    let ledger = Ledger::new(store, scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &key("gate-1-key", 20).public())
        .await
        .unwrap();
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
    let mut broken = receipt("gate-1", 3, Some(hash_of(&c1[1], &g1)), T0 + 1);
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

    // A receipt dated before the one it follows: an honest gate's clock can step back a
    // second after a slow lease renewal, and refusing would end its chain (finding GR-42). It
    // is taken, and the step back is recorded.
    let earlier = receipt("gate-1", 5, Some(hash_of(&c1[3], &g1)), T0 - 5);
    assert_eq!(
        ledger
            .ingest(&seal(&earlier, &g1).unwrap(), T0)
            .await
            .unwrap(),
        Ingested::Added
    );

    // Every refusal is recorded; the envelope is kept only where its signature held.
    let refusals = ledger.store().refusals(&scope()).await.unwrap();
    let codes: Vec<(&str, bool)> = refusals
        .iter()
        .map(|r| (r.code.as_str(), r.envelope.is_some()))
        .collect();
    // Only the last is a receipt that was taken; a reader of this table can tell.
    let taken: Vec<bool> = refusals.iter().map(|r| r.taken).collect();
    assert_eq!(taken.iter().filter(|t| **t).count(), 1);
    assert_eq!(taken.last(), Some(&true));
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
            ("time_went_back", false),
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
        received_at: T0,
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
    let mut c1 = chain("gate-1", &g1, 6, T0, 10);
    // The seventh is written after the last checkpoint, so no checkpoint should cover it yet.
    c1.push(
        seal(
            &receipt("gate-1", 7, Some(hash_of(&c1[5], &g1)), T0 + 190),
            &g1,
        )
        .unwrap(),
    );
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
        clock_skew_secs: 60,
        retention_secs: None,
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
        clock_skew_secs: 60,
        retention_secs: None,
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
    let ledger = ledger_with_gate_1(store).await;
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
    let witness = DirWitness::new(dir.path().join("w"));
    // Pruning trusts that checkpoint, so it puts it in the witness before it deletes anything
    // (review finding GR-14).
    let pruned = ledger
        .prune(
            &id("gate-1"),
            T0 + 1_000,
            &ledger_key(),
            &id("ledger"),
            T0 + 100,
            &witness,
        )
        .await
        .unwrap();
    assert_eq!(pruned, Some(1));
    assert_eq!(witness.all().unwrap().len(), 1);
    // The next run makes the next one.
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 120, &witness)
        .await
        .unwrap();
    let cps = witness.all().unwrap();
    assert_eq!(cps.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![1, 2]);
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    let report = verify(&Check {
        scope: &scope(),
        keys: &keys,
        export: &export,
        checkpoints: &cps,
        interval_secs: 60,
        now: Some(T0 + 130),
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: Some(0),
    });
    assert!(report.ok(), "{:?}", report.findings);
}
both_stores!(
    a_checkpoint_stored_but_not_witnessed_goes_out_next,
    crash_between_store_and_witness
);

// ----- pruning --------------------------------------------------------------------------

const DAY: i64 = 86_400;

fn check_days(
    keys: &TrustedKeys,
    export: &Export,
    cps: &[(u64, Vec<u8>)],
    retention_days: Option<u64>,
    now: Option<i64>,
) -> Report {
    verify(&Check {
        scope: &scope(),
        keys,
        export,
        checkpoints: cps,
        // Checkpoints in this test are days apart.
        interval_secs: 100 * DAY as u32,
        now,
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: retention_days.map(|d| d * DAY as u64),
    })
}

async fn pruning<S: Store>(store: S) {
    let ledger = ledger_with_gate_1(store).await;
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    let lk = ledger_key();
    let li = id("ledger");
    let g1 = key("gate-1-key", 20);
    let gate = id("gate-1");
    // Ten receipts, arriving one a day. The gate writes the same time in every one: the age
    // that matters is when the Ledger received it, not what the gate says (finding GR-9).
    let c = chain("gate-1", &g1, 10, T0, 0);

    // Nothing is pruned before a checkpoint covers it.
    for (i, env) in c[..6].iter().enumerate() {
        ledger.ingest(env, T0 + i as i64 * DAY).await.unwrap();
    }
    let far = T0 + 100 * DAY;
    let day_6 = T0 + 6 * DAY;
    assert_eq!(
        ledger
            .prune(&gate, far, &lk, &li, day_6, &witness)
            .await
            .unwrap(),
        None
    );
    ledger.checkpoint(&lk, &li, day_6, &witness).await.unwrap();
    for (i, env) in c[6..].iter().enumerate() {
        ledger.ingest(env, T0 + (6 + i as i64) * DAY).await.unwrap();
    }
    // Everything is older than the cutoff, but only six receipts are witnessed.
    let day_10 = T0 + 10 * DAY;
    assert_eq!(
        ledger
            .prune(&gate, far, &lk, &li, day_10, &witness)
            .await
            .unwrap(),
        Some(6)
    );
    ledger.checkpoint(&lk, &li, day_10, &witness).await.unwrap();
    // A cutoff inside the chain: receipts 7 and 8 arrived before day 8, receipt 9 did not.
    let cutoff = T0 + 8 * DAY;
    let pruned_at = T0 + 40 * DAY;
    assert_eq!(
        ledger
            .prune(&gate, cutoff, &lk, &li, pruned_at, &witness)
            .await
            .unwrap(),
        Some(8)
    );
    assert_eq!(
        ledger
            .prune(&gate, cutoff, &lk, &li, pruned_at, &witness)
            .await
            .unwrap(),
        None
    );

    let export = ledger.export().await.unwrap();
    assert_eq!(export.gates[0].receipts.len(), 2);
    let keys = ledger.keys(&lk).await.unwrap();
    let cps = witness.all().unwrap();
    // Verified an hour after the prune, on day 40: receipt 8 was first covered on day 10.
    let later = Some(pruned_at + 3_600);
    let report = check_days(&keys, &export, &cps, Some(30), later);
    assert!(report.ok(), "{:?}", report.findings);
    assert_eq!(report.gates[0].verified, Some((9, 10)));
    assert_eq!(report.gates[0].pruned_through, 8);
    // The head is where it was, and the chain goes on from it.
    assert_eq!(
        ledger
            .store()
            .head(&scope(), &gate)
            .await
            .unwrap()
            .unwrap()
            .seq,
        10
    );
    let next = seal(&receipt("gate-1", 11, Some(hash_of(&c[9], &g1)), T0), &g1).unwrap();
    assert_eq!(
        ledger.ingest(&next, pruned_at).await.unwrap(),
        Ingested::Added
    );
    // A receipt from the pruned stretch, sent again, cannot be checked any more.
    assert_eq!(
        refused(ledger.ingest(&c[2], pruned_at).await),
        "already_pruned"
    );

    let record = |through_seq: u64, hash: &gov_core::Digest, at: i64| {
        let doc = Pruned {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            issuer: id("ledger"),
            gate: id("gate-1"),
            through_seq,
            hash: hash.clone(),
            at,
        };
        seal(&doc, &lk).unwrap()
    };
    let found = |export: &Export, retention: Option<u64>, now: Option<i64>| {
        let report = check_days(&keys, export, &cps, retention, now);
        report
            .findings
            .iter()
            .map(|f| (f.problem.clone(), f.seq))
            .collect::<Vec<_>>()
    };

    // A prune record that lies about where the chain was cut does not fit the next receipt.
    let mut lying = export.clone();
    lying.gates[0].pruned = Some(record(8, &hash_of(&c[0], &g1), pruned_at));
    assert!(found(&lying, Some(30), later).contains(&(Problem::BrokenChain, Some(9))));
    // Nor does an export that hides that anything was pruned.
    let mut hiding = export.clone();
    hiding.gates[0].pruned = None;
    assert!(found(&hiding, Some(30), later).contains(&(Problem::BrokenChain, Some(1))));
    // Nor a record the Ledger's key did not sign.
    let mut forged = export.clone();
    let doc = gov_core::open::<Pruned>(
        export.gates[0].pruned.as_ref().unwrap(),
        &gov_core::KeySet::new([lk.public()]),
        &scope(),
    )
    .unwrap()
    .doc;
    forged.gates[0].pruned = Some(seal(&doc, &g1).unwrap());
    let f = found(&forged, Some(30), later);
    assert!(
        f.iter()
            .any(|(p, _)| matches!(p, Problem::BadPruneRecord(_))),
        "{f:?}"
    );

    // Review finding GR-3. Whoever runs the Ledger deletes every receipt and signs a record
    // for it. Past what the witness ever covered: refused outright.
    let mut erased = export.clone();
    erased.gates[0].receipts.clear();
    erased.gates[0].pruned = Some(record(9999, &hash_of(&c[9], &g1), pruned_at));
    assert!(found(&erased, Some(30), later).contains(&(Problem::PruneNotWitnessed, Some(9999))));
    // At the witnessed head with a hash the witness does not have: refused.
    erased.gates[0].pruned = Some(record(10, &hash_of(&c[0], &g1), pruned_at));
    let f = found(&erased, Some(30), later);
    assert!(f.contains(&(Problem::PruneNotWitnessed, Some(10))), "{f:?}");

    // Review finding GR-22. At the witnessed head with the right hash, which the operator
    // knows: the witness cannot tell this from an honest prune, so the customer's retention
    // has to. Receipt 10 was first covered on day 10; the wipe is checked on day 12.
    let day_12 = Some(T0 + 12 * DAY);
    erased.gates[0].pruned = Some(record(10, &hash_of(&c[9], &g1), T0 + 12 * DAY - 60));
    assert_eq!(
        found(&erased, Some(30), day_12),
        vec![(Problem::PrunedTooSoon, Some(10))]
    );
    // Without the retention the pruning cannot be checked, and that is a finding, not "OK".
    assert_eq!(
        found(&erased, None, day_12),
        vec![(Problem::PruneUnchecked, Some(10))]
    );
    // The Ledger dates its record 91 days ahead: age is measured by the verifier's clock and
    // the witness, never by the record, and a record dated after now is itself reported.
    erased.gates[0].pruned = Some(record(10, &hash_of(&c[9], &g1), T0 + 103 * DAY));
    assert_eq!(
        found(&erased, Some(30), day_12),
        vec![
            (Problem::BadPruneRecord("dated_ahead"), Some(10)),
            (Problem::PrunedTooSoon, Some(10)),
        ]
    );
    // With no clock given, nothing can be shown old enough.
    assert_eq!(
        found(&export, Some(30), None),
        vec![(Problem::PrunedTooSoon, Some(8))]
    );
    // The honest prune, checked 30 days and an hour after the cover on day 10.
    assert!(found(&export, Some(30), later).is_empty());
    assert_eq!(
        found(&export, Some(31), later),
        vec![(Problem::PrunedTooSoon, Some(8))]
    );

    // A prune record numbered past the largest number does not crash the check (GR-11).
    erased.gates[0].pruned = Some(record(u64::MAX, &hash_of(&c[9], &g1), pruned_at));
    erased.gates[0].receipts = vec![c[0].clone()];
    assert!(found(&erased, Some(30), later).contains(&(Problem::PruneNotWitnessed, Some(u64::MAX))));
}
both_stores!(pruning_keeps_the_chain_verifiable, pruning);

// ----- the reviewer's findings of 2026-11-26 --------------------------------------------

/// GR-4: with no witness cover, `verify` must not say all is well.
async fn no_cover<S: Store>(store: S) {
    let s = stack(store).await;
    let good = s.ledger.export().await.unwrap();
    let cps = DirWitness::new(s.witness_dir.path()).all().unwrap();

    // (a) An empty witness.
    assert_eq!(
        problems(&check(&s.keys, &good, &[], None)),
        vec![(Problem::NoCheckpoint, None, None)]
    );

    // (b) A gate the Ledger leaves out of every checkpoint: it could be erased at any time.
    let only_gate_1: Vec<(u64, Vec<u8>)> = cps
        .iter()
        .map(|(n, bytes)| {
            let env: Envelope = serde_json::from_slice(bytes).unwrap();
            let mut doc = gov_core::open::<Checkpoint>(
                &env,
                &gov_core::KeySet::new([ledger_key().public()]),
                &scope(),
            )
            .unwrap()
            .doc;
            doc.heads.retain(|h| h.gate.as_str() == "gate-1");
            (
                *n,
                serde_json::to_vec(&seal(&doc, &ledger_key()).unwrap()).unwrap(),
            )
        })
        .collect();
    let report = check(&s.keys, &good, &only_gate_1, None);
    assert!(problems(&report).contains(&(Problem::GateNotWitnessed, Some("gate-2"), None)));

    // (c) That gate then dropped from the export, while the customer still holds its key.
    let mut dropped = good.clone();
    dropped.gates.retain(|g| g.gate.as_str() != "gate-2");
    let report = check(&s.keys, &dropped, &only_gate_1, None);
    assert!(problems(&report).contains(&(Problem::GateMissing, Some("gate-2"), None)));

    // (c2) Or kept in the export as an empty entry, with no prune record (finding GR-23).
    let mut emptied = good.clone();
    emptied.gates[1].receipts.clear();
    let report = check(&s.keys, &emptied, &only_gate_1, None);
    assert_eq!(
        problems(&report),
        vec![(Problem::GateNeverSeen, Some("gate-2"), None)]
    );

    // (d) A receipt older than the latest checkpoint that no checkpoint covers.
    let g1 = key("gate-1-key", 20);
    let mut late = good.clone();
    let old = receipt(
        "gate-1",
        7,
        Some(hash_of(&late.gates[0].receipts[5], &g1)),
        T0 + 60,
    );
    late.gates[0].receipts[6] = seal(&old, &g1).unwrap();
    assert_eq!(
        problems(&check(&s.keys, &late, &cps, None)),
        vec![(Problem::ReceiptNotWitnessed, Some("gate-1"), Some(7))]
    );
}
both_stores!(
    verify_does_not_pass_what_the_witness_does_not_cover,
    no_cover
);

/// GR-15 and GR-12: an export that lists a gate twice or is of another version; a witness with
/// look-alike file names; a checkpoint number that repeats.
async fn odd_inputs<S: Store>(store: S) {
    let s = stack(store).await;
    let good = s.ledger.export().await.unwrap();
    let witness = DirWitness::new(s.witness_dir.path());
    let cps = witness.all().unwrap();

    let mut twice = good.clone();
    twice.gates.push(good.gates[0].clone());
    assert!(problems(&check(&s.keys, &twice, &cps, None)).contains(&(
        Problem::BadExport("gate_listed_twice"),
        Some("gate-1"),
        None
    )));
    let mut v2 = good.clone();
    v2.v = 2;
    assert!(problems(&check(&s.keys, &v2, &cps, None)).contains(&(
        Problem::BadExport("unknown_version"),
        None,
        None
    )));

    // Files that only look like checkpoints are not read as checkpoints.
    for name in [
        "checkpoint-1.json",
        "checkpoint-+0000000000000000001.json",
        "checkpoint-00000000000000000001.json.bak",
    ] {
        std::fs::write(s.witness_dir.path().join(name), b"{}").unwrap();
    }
    assert_eq!(witness.all().unwrap(), cps);
    // A number that repeats or falls is reported, not silently taken.
    let mut repeated = cps.clone();
    repeated.insert(2, cps[1].clone());
    assert!(
        problems(&check(&s.keys, &good, &repeated, None)).contains(&(
            Problem::BadCheckpoint("number_repeated"),
            None,
            Some(2)
        ))
    );
}
both_stores!(odd_exports_and_witnesses_are_reported, odd_inputs);

/// GR-6: a gate key anyone could forge for is not enrolled, nor one key under two ids.
async fn enrolment<S: Store>(store: S) {
    let ledger = ledger_with_gates(store).await;
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let weak = gov_core::PublicKey::from_bytes_unchecked(id("gate-3-key"), &identity);
    let r = ledger.enrol_gate_key(&id("gate-3"), &weak).await;
    assert!(
        matches!(r, Err(LedgerError::Core(gov_core::Error::WeakKey))),
        "{r:?}"
    );
    // Gate 1's key again, under a new id and for another gate.
    let same = SigningKey::from_seed(id("gate-3-key"), &[20; 32])
        .unwrap()
        .public();
    assert!(ledger.enrol_gate_key(&id("gate-3"), &same).await.is_err());
    // And a key id that is taken.
    assert!(ledger
        .enrol_gate_key(&id("gate-3"), &key("gate-1-key", 77).public())
        .await
        .is_err());
    // The store itself refuses, whoever calls it (finding GR-24).
    assert!(ledger
        .store()
        .add_gate_key(&scope(), &id("gate-3"), &weak)
        .await
        .is_err());
    assert_eq!(ledger.store().gate_keys(&scope()).await.unwrap().len(), 2);
}
both_stores!(a_weak_or_repeated_gate_key_is_not_enrolled, enrolment);

/// GR-16 and GR-42: a receipt dated before the one it follows is reported by `verify`. GR-43:
/// so is a checkpoint dated before a receipt it covers.
async fn clocks<S: Store>(store: S) {
    let s = stack(store).await;
    let good = s.ledger.export().await.unwrap();
    let cps = DirWitness::new(s.witness_dir.path()).all().unwrap();
    let g1 = key("gate-1-key", 20);

    let mut back = good.clone();
    let stepped = receipt(
        "gate-1",
        7,
        Some(hash_of(&back.gates[0].receipts[5], &g1)),
        T0 + 30,
    );
    back.gates[0].receipts[6] = seal(&stepped, &g1).unwrap();
    assert!(problems(&check(&s.keys, &back, &cps, None)).contains(&(
        Problem::BadReceipt("time_went_back"),
        Some("gate-1"),
        Some(7)
    )));

    // The Ledger re-dates its first checkpoint a day early. The receipts it covers say otherwise.
    let env: Envelope = serde_json::from_slice(&cps[0].1).unwrap();
    let mut doc = gov_core::open::<Checkpoint>(
        &env,
        &gov_core::KeySet::new([ledger_key().public()]),
        &scope(),
    )
    .unwrap()
    .doc;
    doc.at = T0 - 86_400;
    let mut redated = cps.clone();
    redated[0].1 = serde_json::to_vec(&seal(&doc, &ledger_key()).unwrap()).unwrap();
    let report = check(&s.keys, &good, &redated, None);
    assert!(problems(&report).contains(&(Problem::CheckpointBackdated, Some("gate-1"), Some(1))));
    assert!(check(&s.keys, &good, &cps, None).ok());
}
both_stores!(clocks_that_disagree_are_reported, clocks);

/// GR-46: an outcome answers one intent of its own chain, for the same action, once.
async fn outcomes<S: Store>(store: S) {
    let ledger = ledger_with_gate_1(store).await;
    let g1 = key("gate-1-key", 20);
    // Intent 1, its outcome 2, and intent 3.
    let c = chain("gate-1", &g1, 3, T0, 1);
    for env in &c {
        ledger.ingest(env, T0).await.unwrap();
    }
    let head = hash_of(&c[2], &g1);
    let honest = receipt("gate-1", 4, Some(head.clone()), T0 + 3);
    assert_eq!(honest.intent, Some(3));

    // An outcome that names an outcome.
    let mut of_outcome = honest.clone();
    of_outcome.intent = Some(2);
    assert_eq!(
        refused(ledger.ingest(&seal(&of_outcome, &g1).unwrap(), T0).await),
        "outcome_without_intent"
    );
    // An outcome that names the right intent, for another action.
    let mut other_action = honest.clone();
    other_action.action = receipt("gate-1", 9, None, T0).action;
    assert_eq!(
        refused(ledger.ingest(&seal(&other_action, &g1).unwrap(), T0).await),
        "outcome_without_intent"
    );
    // The honest outcome of intent 3.
    let honest_env = seal(&honest, &g1).unwrap();
    assert_eq!(
        ledger.ingest(&honest_env, T0).await.unwrap(),
        Ingested::Added
    );

    // A second outcome for the same intent gets past intake's single look-up, and `verify`
    // reports it.
    let mut second = honest.clone();
    second.seq = 5;
    second.prev = Some(hash_of(&honest_env, &g1));
    ledger
        .ingest(&seal(&second, &g1).unwrap(), T0)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 10, &witness)
        .await
        .unwrap();
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    let report = check(&keys, &export, &witness.all().unwrap(), None);
    assert_eq!(
        problems(&report),
        vec![(
            Problem::BadReceipt("outcome_without_intent"),
            Some("gate-1"),
            Some(5)
        )]
    );
}
both_stores!(an_outcome_answers_one_intent_once, outcomes);

/// A refusal is stored once however often it arrives (review finding GR-74): a gate whose
/// chain is stuck resends its batch every round, and a recorded shipment can be replayed.
async fn refusals_once<S: Store>(store: S) {
    let ledger = ledger_with_gate_1(store).await;
    let g1 = key("gate-1-key", 20);
    // Receipt 3 of a chain that has no receipt 1: signed by the gate, and refused.
    let gap = seal(&receipt("gate-1", 3, None, T0), &g1).unwrap();
    for at in [T0, T0 + 5, T0 + 9] {
        assert!(ledger.ingest(&gap, at).await.is_err());
    }
    let refusals = ledger.store().refusals(&scope()).await.unwrap();
    assert_eq!(refusals.len(), 1);
    let r = &refusals[0];
    assert_eq!((r.at, r.seen, r.last_at), (T0, 3, T0 + 9));
    assert!(r.envelope.is_some(), "the evidence is kept, once");
    // Another envelope is another row.
    let other = seal(&receipt("gate-1", 5, None, T0), &g1).unwrap();
    assert!(ledger.ingest(&other, T0 + 10).await.is_err());
    assert!(ledger.ingest(&gap, T0 + 11).await.is_err());
    let refusals = ledger.store().refusals(&scope()).await.unwrap();
    let seen: Vec<u64> = refusals.iter().map(|r| r.seen).collect();
    assert_eq!(seen, vec![4, 1]);
}
both_stores!(
    a_refusal_is_stored_once_however_often_it_arrives,
    refusals_once
);
