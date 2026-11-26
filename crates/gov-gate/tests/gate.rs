//! Slice G3a: the gate's chain, spool and shipping, in observe mode, against a real Ledger
//! (in memory) and a real witness directory.

use async_trait::async_trait;
use gov_core::action::{CanonicalAction, Kind, Reversal};
use gov_core::lease::{Checkpoint, IntakeAck};
use gov_core::receipt::{Coverage, Mode, OutcomeStatus, Phase, Proof, Receipt};
use gov_core::{open, seal, Envelope, Id, KeySet, ResourcePath, Scope, SigningKey};
use gov_gate::{FileSpool, Gate, GateConfig, GateError, LedgerLink, Observed, Spool};
use gov_ledger::verify::{verify, Check};
use gov_ledger::{DirWitness, Ledger, MemoryStore, Witness};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

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
fn config(spool_limit: usize) -> GateConfig {
    GateConfig {
        scope: scope(),
        gate: id("gate-1"),
        ledger_keys: KeySet::new([ledger_key().public()]),
        spool_limit,
        batch: 100,
    }
}
fn action(n: u64) -> CanonicalAction {
    CanonicalAction::new(
        id("jira"),
        id("issue.update"),
        Kind::Write,
        ResourcePath::new("jira/project/PAY/issue/PAY-12").unwrap(),
        None,
        &json!({"n": n, "api_token": "never-in-a-receipt"}),
        &[9; 32],
    )
    .unwrap()
}
fn observed<'a>(agent: &'a Id, action: &'a CanonicalAction) -> Observed<'a> {
    Observed {
        agent,
        proof: Proof::Claimed,
        action,
        reversal: Reversal::Restore,
        coverage: Coverage::InLine,
    }
}

/// The Ledger, reached in process. `lie` makes it answer with someone else's signature;
/// `down` makes it unreachable.
struct InProcess {
    ledger: Ledger<MemoryStore>,
    lie: AtomicBool,
    down: AtomicBool,
}

#[async_trait]
impl LedgerLink for InProcess {
    async fn ship(&self, batch: &[Envelope]) -> Result<Envelope, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("connection refused".into());
        }
        let key = if self.lie.load(Ordering::SeqCst) {
            SigningKey::from_seed(id("ledger-1"), &[51; 32]).unwrap()
        } else {
            ledger_key()
        };
        let (_, ack) = self
            .ledger
            .ingest_batch(batch, &key, &id("ledger"), T0 + 1)
            .await
            .map_err(|e| e.to_string())?;
        Ok(ack)
    }
}

async fn ledger() -> InProcess {
    let ledger = Ledger::new(MemoryStore::default(), scope());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate_key().public())
        .await
        .unwrap();
    InProcess {
        ledger,
        lie: AtomicBool::new(false),
        down: AtomicBool::new(false),
    }
}

fn docs(spool: &FileSpool) -> Vec<Receipt> {
    let keys = KeySet::new([gate_key().public()]);
    spool
        .held()
        .unwrap()
        .iter()
        .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
        .collect()
}

#[tokio::test]
async fn an_observed_call_leaves_two_receipts_that_verify_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let mut gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path().join("spool")).unwrap(),
        T0,
    )
    .unwrap();
    let agent = id("triage-bot");

    for n in 1..=3 {
        let a = action(n);
        let intent = gate.begin(&observed(&agent, &a), T0 + n as i64).unwrap();
        let status = if n == 2 {
            OutcomeStatus::Failed
        } else {
            OutcomeStatus::Ok
        };
        gate.finish(intent, status, Some(id("PAY-12")), T0 + n as i64)
            .unwrap();
    }
    let spool = FileSpool::open(dir.path().join("spool")).unwrap();
    let written = docs(&spool);
    assert_eq!(written.len(), 6);
    // Every receipt says what it is: observed, not enforced, by an agent that only said who
    // it was; and each outcome names its intent.
    for (i, r) in written.iter().enumerate() {
        assert_eq!(r.seq, i as u64 + 1);
        assert_eq!(r.mode, Mode::Observe);
        assert!(!r.mode.enforced());
        assert_eq!(r.agent.proof, Proof::Claimed);
        assert!(r.mandate.is_empty());
        if r.phase == Phase::Outcome {
            assert_eq!(r.intent, Some(r.seq - 1));
        }
    }
    assert_eq!(
        written[3].outcome.as_ref().unwrap().status,
        OutcomeStatus::Failed
    );
    // No argument value is anywhere on disk in the spool, only its salted digest.
    for entry in std::fs::read_dir(dir.path().join("spool")).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains("never-in-a-receipt"));
    }

    assert_eq!(gate.ship(&link).await.unwrap(), 6);
    assert_eq!(gate.ship(&link).await.unwrap(), 0);
    // Acknowledged is not covered: the spool keeps everything until the witness has it.
    assert_eq!(gate.uncovered().unwrap(), 6);
    let checkpoint = link
        .ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    assert_eq!(gate.covered_by(&checkpoint).unwrap(), 6);
    assert_eq!(gate.uncovered().unwrap(), 0);
    assert_eq!(gate.covered_by(&checkpoint).unwrap(), 0);

    // The chain goes on after the trim, and an auditor's check passes on the whole of it.
    let a = action(4);
    let intent = gate.begin(&observed(&agent, &a), T0 + 61).unwrap();
    assert_eq!(intent, 7);
    gate.finish(intent, OutcomeStatus::Ok, None, T0 + 61)
        .unwrap();
    gate.ship(&link).await.unwrap();
    link.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 120, &witness)
        .await
        .unwrap();
    let export = link.ledger.export().await.unwrap();
    let keys = link.ledger.keys(&ledger_key()).await.unwrap();
    let report = verify(&Check {
        scope: &scope(),
        keys: &keys,
        export: &export,
        checkpoints: &witness.all().unwrap(),
        interval_secs: 60,
        now: Some(T0 + 130),
        gate_quiet_secs: None,
        retention_secs: None,
    });
    assert!(report.ok(), "{:?}", report.findings);
    assert_eq!(report.receipts_verified, 8);
}

/// Spec §4.5: a crash between intent and outcome yields `unknown`, never a second call.
#[tokio::test]
async fn a_crash_between_intent_and_outcome_is_recorded_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    {
        let mut gate = Gate::open(
            config(100),
            gate_key(),
            FileSpool::open(dir.path()).unwrap(),
            T0,
        )
        .unwrap();
        let a = action(1);
        let done = gate.begin(&observed(&agent, &a), T0).unwrap();
        gate.finish(done, OutcomeStatus::Ok, None, T0).unwrap();
        let b = action(2);
        gate.begin(&observed(&agent, &b), T0 + 1).unwrap();
        // The process dies here, with the call to the system in flight.
    }
    let mut gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path()).unwrap(),
        T0 + 30,
    )
    .unwrap();
    let written = docs(&FileSpool::open(dir.path()).unwrap());
    assert_eq!(written.len(), 4);
    let last = &written[3];
    assert_eq!((last.phase, last.intent), (Phase::Outcome, Some(3)));
    assert_eq!(
        last.outcome.as_ref().unwrap().status,
        OutcomeStatus::Unknown
    );
    assert_eq!(last.action.digest, written[2].action.digest);
    // The intent is closed: it cannot be finished a second time, and reopening adds nothing.
    assert!(matches!(
        gate.finish(3, OutcomeStatus::Ok, None, T0 + 31),
        Err(GateError::NoSuchIntent(3))
    ));
    drop(gate);
    let gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path()).unwrap(),
        T0 + 60,
    )
    .unwrap();
    assert_eq!(gate.state().head.as_ref().unwrap().seq, 4);
    // The chain is whole: the Ledger takes all four.
    let link = ledger().await;
    let mut gate = gate;
    assert_eq!(gate.ship(&link).await.unwrap(), 4);
}

/// A receipt on disk that the state file never learnt of (a crash between the two writes) is
/// adopted, so the next receipt does not reuse its number.
#[tokio::test]
async fn a_receipt_written_just_before_a_crash_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let a = action(1);
    {
        let mut gate = Gate::open(
            config(100),
            gate_key(),
            FileSpool::open(dir.path()).unwrap(),
            T0,
        )
        .unwrap();
        let n = gate.begin(&observed(&agent, &a), T0).unwrap();
        gate.finish(n, OutcomeStatus::Ok, None, T0).unwrap();
    }
    // Put the state back to before the second receipt.
    let state = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&state).unwrap();
    v["head"]["seq"] = json!(1);
    let first = FileSpool::open(dir.path()).unwrap().held().unwrap()[0]
        .hash
        .clone();
    v["head"]["hash"] = serde_json::to_value(first).unwrap();
    std::fs::write(dir.path().join("state.json"), v.to_string()).unwrap();

    let mut gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path()).unwrap(),
        T0,
    )
    .unwrap();
    assert_eq!(gate.state().head.as_ref().unwrap().seq, 2);
    assert_eq!(gate.begin(&observed(&agent, &a), T0).unwrap(), 3);
}

/// Review findings GR-16 and GR-42: a gate never writes a time earlier than its last
/// receipt's, so a clock stepped back by a lease renewal cannot jam its chain at the Ledger.
#[tokio::test]
async fn a_clock_that_steps_back_does_not_reach_the_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let mut gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path()).unwrap(),
        T0,
    )
    .unwrap();
    let a = action(1);
    let first = gate.begin(&observed(&agent, &a), T0 + 100).unwrap();
    // The clock steps back two seconds before the outcome, and stays behind.
    gate.finish(first, OutcomeStatus::Ok, None, T0 + 98)
        .unwrap();
    let second = gate.begin(&observed(&agent, &a), T0 + 99).unwrap();
    gate.finish(second, OutcomeStatus::Ok, None, T0 + 101)
        .unwrap();
    let times: Vec<i64> = docs(&FileSpool::open(dir.path()).unwrap())
        .iter()
        .map(|r| r.at)
        .collect();
    assert_eq!(times, vec![T0 + 100, T0 + 100, T0 + 100, T0 + 101]);
    // The Ledger takes them without recording a step back.
    let link = ledger().await;
    assert_eq!(gate.ship(&link).await.unwrap(), 4);
    assert!(gov_ledger::Store::refusals(link.ledger.store(), &scope())
        .await
        .unwrap()
        .is_empty());
}

/// Review finding GR-10: the spool is the gate's own proof. It is not trimmed on an answer
/// the Ledger did not sign, on the Ledger's signed answer alone, or on a checkpoint that
/// names another hash.
#[tokio::test]
async fn the_spool_outlives_a_ledger_that_lies_or_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let link = ledger().await;
    let mut gate = Gate::open(
        config(100),
        gate_key(),
        FileSpool::open(dir.path()).unwrap(),
        T0,
    )
    .unwrap();
    let a = action(1);
    let n = gate.begin(&observed(&agent, &a), T0).unwrap();
    gate.finish(n, OutcomeStatus::Ok, None, T0).unwrap();

    link.down.store(true, Ordering::SeqCst);
    assert!(matches!(gate.ship(&link).await, Err(GateError::Link(_))));
    link.down.store(false, Ordering::SeqCst);
    // An answer signed by a key that is not the Ledger's.
    link.lie.store(true, Ordering::SeqCst);
    assert!(matches!(
        gate.ship(&link).await,
        Err(GateError::Core(gov_core::Error::BadSignature))
    ));
    assert_eq!(gate.state().acked_through, 0);
    link.lie.store(false, Ordering::SeqCst);
    // The honest Ledger already holds both (the lying answer came after a real intake).
    assert_eq!(gate.ship(&link).await.unwrap(), 2);
    assert_eq!(gate.uncovered().unwrap(), 2);

    // An acknowledgement for a receipt this gate never wrote, or with another hash.
    struct Forged(Envelope);
    #[async_trait]
    impl LedgerLink for Forged {
        async fn ship(&self, _: &[Envelope]) -> Result<Envelope, String> {
            Ok(self.0.clone())
        }
    }
    let b = action(2);
    let n = gate.begin(&observed(&agent, &b), T0 + 1).unwrap();
    gate.finish(n, OutcomeStatus::Ok, None, T0 + 1).unwrap();
    let wrong = IntakeAck {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("ledger"),
        gate: id("gate-1"),
        seq: 4,
        hash: gov_core::Digest::of(&[b"not what the gate wrote"]),
        at: T0 + 2,
    };
    let forged = Forged(seal(&wrong, &ledger_key()).unwrap());
    assert!(matches!(
        gate.ship(&forged).await,
        Err(GateError::LedgerDisagrees(4))
    ));
    assert_eq!(gate.state().acked_through, 2);

    // A checkpoint, validly signed by the Ledger, that names another hash for receipt 2.
    let lying = Checkpoint {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("ledger"),
        seq: 1,
        at: T0 + 60,
        heads: vec![gov_core::lease::GateHead {
            gate: id("gate-1"),
            seq: 2,
            hash: gov_core::Digest::of(&[b"another history"]),
        }],
    };
    let r = gate.covered_by(&seal(&lying, &ledger_key()).unwrap());
    assert!(matches!(r, Err(GateError::LedgerDisagrees(2))));
    assert_eq!(gate.uncovered().unwrap(), 4);
    // And one not signed by the Ledger at all.
    let unsigned = seal(&lying, &gate_key()).unwrap();
    assert!(gate.covered_by(&unsigned).is_err());
    assert_eq!(gate.uncovered().unwrap(), 4);
}

#[tokio::test]
async fn a_full_spool_says_so_and_an_open_intent_is_never_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let spool = FileSpool::open(dir.path().join("spool")).unwrap();
    let mut gate = Gate::open(config(4), gate_key(), spool, T0).unwrap();
    let a = action(1);
    let n = gate.begin(&observed(&agent, &a), T0).unwrap();
    gate.finish(n, OutcomeStatus::Ok, None, T0).unwrap();
    // Room for one more call: its intent and its outcome.
    let in_flight = gate.begin(&observed(&agent, &a), T0).unwrap();
    // The next call does not fit, and the gate says so before the call, never after it.
    assert!(matches!(
        gate.begin(&observed(&agent, &a), T0),
        Err(GateError::SpoolFull)
    ));

    // A checkpoint covers all three; the open intent and what follows it stay in the spool.
    gate.ship(&link).await.unwrap();
    let checkpoint = link
        .ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    assert_eq!(gate.covered_by(&checkpoint).unwrap(), 2);
    assert_eq!(gate.uncovered().unwrap(), 1);
    // The outcome of the call in flight always fits.
    assert_eq!(
        gate.finish(in_flight, OutcomeStatus::Ok, None, T0 + 61)
            .unwrap(),
        4
    );
    assert!(gate.begin(&observed(&agent, &a), T0 + 62).is_ok());
}
