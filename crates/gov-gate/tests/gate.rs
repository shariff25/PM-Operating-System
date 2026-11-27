//! Slice G3a: the gate's chain, spool and shipping, in observe mode, against a real Ledger
//! (in memory) and a real witness directory.

use async_trait::async_trait;
use gov_core::action::{CanonicalAction, Kind, Reversal};
use gov_core::lease::{Checkpoint, IntakeAck};
use gov_core::receipt::{Coverage, Mode, OutcomeStatus, Phase, Proof, Receipt};
use gov_core::{open, seal, Envelope, Id, KeySet, ResourcePath, Scope, SigningKey};
use gov_gate::{
    DirWitnessReader, FileSpool, Gate, GateConfig, GateError, LedgerLink, LinkAnswer, Observed,
    Shipped, Spool, Who, WitnessReader,
};
use gov_ledger::verify::{verify, Check};
use gov_ledger::{DirWitness, Ledger, MemoryStore, Witness};
use serde_json::json;
use std::path::Path;
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
        policy_version: None,
    }
}
fn gate_at(dir: &Path, limit: usize, now: i64) -> Gate<FileSpool> {
    Gate::open(
        config(limit),
        gate_key(),
        FileSpool::open(dir).unwrap(),
        now,
    )
    .unwrap()
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
        who: Who::claimed(agent.clone()),
        action,
        reversal: Reversal::Restore,
        coverage: Coverage::InLine,
    }
}
/// One whole observed call.
fn call(gate: &mut Gate<FileSpool>, n: u64, now: i64) -> (u64, u64) {
    let agent = id("triage-bot");
    let a = action(n);
    let intent = gate.begin(&observed(&agent, &a), now).unwrap();
    let outcome = gate
        .finish(intent, OutcomeStatus::Ok, Some(id("PAY-12")), now)
        .unwrap();
    (intent, outcome)
}

/// The Ledger, reached in process. `lie` makes it answer with someone else's signature;
/// `down` makes it unreachable; `stale` makes it answer with an old acknowledgement.
struct InProcess {
    ledger: Ledger<MemoryStore>,
    lie: AtomicBool,
    down: AtomicBool,
}

#[async_trait]
impl LedgerLink for InProcess {
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("connection refused".into());
        }
        let key = if self.lie.load(Ordering::SeqCst) {
            SigningKey::from_seed(id("ledger-1"), &[51; 32]).unwrap()
        } else {
            ledger_key()
        };
        let (ack, refused) = self
            .ledger
            .take_shipment(request, batch, &key, &id("ledger"), T0 + 1)
            .await
            .map_err(|e| e.to_string())?;
        Ok(LinkAnswer {
            ack,
            refused: refused.map(str::to_string),
        })
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

/// The nonce in a gate's signed request, read without checking the signature: the fake
/// Ledgers below answer whatever they are sent.
fn nonce_of(request: &Envelope) -> Id {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.payload)
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    id(doc["nonce"].as_str().unwrap())
}

/// A link that answers with whatever it was given, whatever was sent.
struct Canned(Envelope);
#[async_trait]
impl LedgerLink for Canned {
    async fn ship(&self, _: &Envelope, _: &[Envelope]) -> Result<LinkAnswer, String> {
        Ok(LinkAnswer {
            ack: self.0.clone(),
            refused: None,
        })
    }
}
/// A link that signs an acknowledgement of its choosing, with the right nonce.
#[derive(Default)]
struct Says {
    seq: u64,
    hash: Option<gov_core::Digest>,
    refused: Option<String>,
}
#[async_trait]
impl LedgerLink for Says {
    async fn ship(&self, request: &Envelope, _: &[Envelope]) -> Result<LinkAnswer, String> {
        let ack = IntakeAck {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            issuer: id("ledger"),
            gate: id("gate-1"),
            nonce: nonce_of(request),
            seq: self.seq,
            hash: self.hash.clone(),
            at: T0 + 2,
        };
        Ok(LinkAnswer {
            ack: seal(&ack, &ledger_key()).unwrap(),
            refused: self.refused.clone(),
        })
    }
}

fn docs(dir: &Path) -> Vec<Receipt> {
    let keys = KeySet::new([gate_key().public()]);
    FileSpool::open(dir)
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .map(|r| open::<Receipt>(&r.envelope, &keys, &scope()).unwrap().doc)
        .collect()
}

#[tokio::test]
async fn an_observed_call_leaves_two_receipts_that_verify_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let reader = DirWitnessReader::new(dir.path().join("witness"));
    let mut gate = gate_at(&spool, 100, T0);
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
        assert!(!r.at_clamped);
        if r.phase == Phase::Outcome {
            assert_eq!(r.intent, Some(r.seq - 1));
        }
    }
    assert_eq!(
        written[3].outcome.as_ref().unwrap().status,
        OutcomeStatus::Failed
    );
    // No argument value is anywhere on disk in the spool, only its salted digest.
    for entry in std::fs::read_dir(&spool).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains("never-in-a-receipt"));
    }

    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(6));
    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::NothingToSend);
    // Acknowledged is not covered: the spool keeps everything until the witness has it.
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 0);
    assert_eq!(gate.uncovered().unwrap(), 6);
    link.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 6);
    assert_eq!(gate.uncovered().unwrap(), 0);
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 0);
    // The gate keeps the checkpoint it trimmed on, as it read it.
    let kept = FileSpool::open(&spool)
        .unwrap()
        .kept_checkpoint()
        .unwrap()
        .unwrap();
    assert_eq!(kept, witness.all().unwrap()[0].1);

    // The chain goes on after the trim, and an auditor's check passes on the whole of it.
    assert_eq!(call(&mut gate, 4, T0 + 61), (7, 8));
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
        clock_skew_secs: 60,
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
        let mut gate = gate_at(dir.path(), 100, T0);
        call(&mut gate, 1, T0);
        let b = action(2);
        gate.begin(&observed(&agent, &b), T0 + 1).unwrap();
        // The process dies here, with the call to the system in flight.
    }
    let mut gate = gate_at(dir.path(), 100, T0 + 30);
    let written = docs(dir.path());
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
    let mut gate = gate_at(dir.path(), 100, T0 + 60);
    assert_eq!(gate.state().head.as_ref().unwrap().seq, 4);
    // The chain is whole: the Ledger takes all four.
    let link = ledger().await;
    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(4));
}

/// Review finding GR-48: a crash after the outcome was written and before the intent was
/// marked answered must not produce a second, contradicting outcome.
#[tokio::test]
async fn a_crash_after_the_outcome_writes_no_second_outcome() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut gate = gate_at(dir.path(), 100, T0);
        call(&mut gate, 1, T0);
    }
    // Put the intent's file back to how it was before `finish` marked it answered.
    let path = dir.path().join(format!("receipt-{:020}.json", 1));
    let mut held: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(held["open_intent"], json!(false));
    held["open_intent"] = json!(true);
    std::fs::write(&path, held.to_string()).unwrap();

    let gate = gate_at(dir.path(), 100, T0 + 30);
    let written = docs(dir.path());
    assert_eq!(written.len(), 2, "a second outcome was written");
    assert_eq!(
        written[1].outcome.as_ref().unwrap().status,
        OutcomeStatus::Ok
    );
    assert_eq!(gate.state().head.as_ref().unwrap().seq, 2);
    // And the intent is marked answered now.
    assert!(FileSpool::open(dir.path())
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .all(|r| !r.open_intent));
}

/// A receipt on disk that the state file never learnt of (a crash between the two writes) is
/// adopted, so the next receipt does not reuse its number.
#[tokio::test]
async fn a_receipt_written_just_before_a_crash_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut gate = gate_at(dir.path(), 100, T0);
        call(&mut gate, 1, T0);
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

    let mut gate = gate_at(dir.path(), 100, T0);
    assert_eq!(gate.state().head.as_ref().unwrap().seq, 2);
    assert_eq!(call(&mut gate, 2, T0).0, 3);
}

/// Review finding GR-51: a spool file that is not what this gate wrote stops the gate.
#[tokio::test]
async fn a_spool_file_the_gate_did_not_write_is_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut gate = gate_at(dir.path(), 100, T0);
        call(&mut gate, 1, T0);
    }
    let reopen = || {
        Gate::open(
            config(100),
            gate_key(),
            FileSpool::open(dir.path()).unwrap(),
            T0,
        )
    };
    assert!(reopen().is_ok());
    // A receipt signed by another key, planted under the next number.
    let path = dir.path().join(format!("receipt-{:020}.json", 2));
    let original = std::fs::read_to_string(&path).unwrap();
    let mut planted: serde_json::Value = serde_json::from_str(&original).unwrap();
    planted["seq"] = json!(3);
    std::fs::write(
        dir.path().join(format!("receipt-{:020}.json", 3)),
        planted.to_string(),
    )
    .unwrap();
    assert!(matches!(reopen(), Err(GateError::Spool(_))));
    std::fs::remove_file(dir.path().join(format!("receipt-{:020}.json", 3))).unwrap();
    // The right receipt with a hash that is not its own.
    let mut wrong: serde_json::Value = serde_json::from_str(&original).unwrap();
    wrong["hash"] = json!("0".repeat(64));
    std::fs::write(&path, wrong.to_string()).unwrap();
    assert!(matches!(reopen(), Err(GateError::Spool(_))));
}

/// Review findings GR-16, GR-42 and GR-53: a gate never writes a time earlier than its last
/// receipt's, and says so on the receipt when it had to.
#[tokio::test]
async fn a_clock_that_steps_back_does_not_reach_the_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let mut gate = gate_at(dir.path(), 100, T0);
    let a = action(1);
    let first = gate.begin(&observed(&agent, &a), T0 + 100).unwrap();
    // The clock steps back two seconds before the outcome, and stays behind.
    gate.finish(first, OutcomeStatus::Ok, None, T0 + 98)
        .unwrap();
    let second = gate.begin(&observed(&agent, &a), T0 + 99).unwrap();
    gate.finish(second, OutcomeStatus::Ok, None, T0 + 101)
        .unwrap();
    let written = docs(dir.path());
    let times: Vec<(i64, bool)> = written.iter().map(|r| (r.at, r.at_clamped)).collect();
    assert_eq!(
        times,
        vec![
            (T0 + 100, false),
            (T0 + 100, true),
            (T0 + 100, true),
            (T0 + 101, false)
        ]
    );
    // The Ledger takes them without recording a step back.
    let link = ledger().await;
    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(4));
    assert!(gov_ledger::Store::refusals(link.ledger.store(), &scope())
        .await
        .unwrap()
        .is_empty());
}

/// A receipt's time says where it came from, receipt by receipt: the mark is given with
/// the time, so neither can be set without the other (review finding GR-177). A gate that
/// gets its first lease between a call's intent and its outcome writes each as it was.
#[tokio::test]
async fn each_receipt_says_whether_its_own_time_is_the_authoritys() {
    use gov_gate::When;
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let mut gate = gate_at(dir.path(), 100, T0);
    let a = action(1);
    let leased = |at: i64| When {
        at,
        from_lease: true,
    };
    // Begun on the machine's clock; the lease arrives; finished on the Authority's.
    let first = gate
        .begin_under(&observed(&agent, &a), When::here(T0 + 1), None)
        .unwrap();
    gate.finish_at(first, OutcomeStatus::Ok, None, leased(T0 + 60))
        .unwrap();
    // Both on the Authority's.
    let second = gate
        .begin_under(&observed(&agent, &a), leased(T0 + 61), None)
        .unwrap();
    gate.finish_at(second, OutcomeStatus::Ok, None, leased(T0 + 62))
        .unwrap();
    // The calls that take a bare time are the machine's clock, as before there were leases.
    let third = gate.begin(&observed(&agent, &a), T0 + 63).unwrap();
    gate.finish(third, OutcomeStatus::Ok, None, T0 + 64)
        .unwrap();
    let said: Vec<(i64, bool)> = docs(dir.path())
        .iter()
        .map(|r| (r.at, r.at_from_lease))
        .collect();
    assert_eq!(
        said,
        vec![
            (T0 + 1, false),
            (T0 + 60, true),
            (T0 + 61, true),
            (T0 + 62, true),
            (T0 + 63, false),
            (T0 + 64, false),
        ]
    );
}

/// Review finding GR-10: an answer the Ledger did not sign, or one that names another hash,
/// moves nothing.
#[tokio::test]
async fn the_gate_checks_every_answer_against_its_own_chain() {
    let dir = tempfile::tempdir().unwrap();
    let link = ledger().await;
    let mut gate = gate_at(dir.path(), 100, T0);
    call(&mut gate, 1, T0);

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
    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(2));
    assert_eq!(gate.uncovered().unwrap(), 2);

    call(&mut gate, 2, T0 + 1);
    // An acknowledgement, rightly signed and with the right nonce, for a receipt this gate
    // wrote but with another hash.
    let wrong = Says {
        seq: 4,
        hash: Some(gov_core::Digest::of(&[b"not what the gate wrote"])),
        ..Says::default()
    };
    assert!(matches!(
        gate.ship(&wrong).await,
        Err(GateError::LedgerDisagrees(4))
    ));
    // And one for a receipt this gate never wrote.
    let beyond = Says {
        seq: 9,
        hash: Some(gov_core::Digest::of(&[b"x"])),
        ..Says::default()
    };
    assert!(matches!(
        gate.ship(&beyond).await,
        Err(GateError::LedgerDisagrees(9))
    ));
    assert_eq!(gate.state().acked_through, 2);

    // Review finding GR-55: an old answer, rightly signed, played back for a new batch.
    let old = IntakeAck {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("ledger"),
        gate: id("gate-1"),
        nonce: id("ship-00000000000000000000000000000000"),
        seq: 4,
        hash: Some(
            FileSpool::open(dir.path()).unwrap().held().unwrap()[3]
                .hash
                .clone(),
        ),
        at: T0,
    };
    let replayed = Canned(seal(&old, &ledger_key()).unwrap());
    assert!(matches!(
        gate.ship(&replayed).await,
        Err(GateError::Core(gov_core::Error::Replay))
    ));
    assert_eq!(gate.state().acked_through, 2);
}

/// Review finding GR-50: the gate tells a Ledger that takes nothing from one with nothing to
/// take, sends again to a Ledger restored from an older copy, and is not confused by an
/// honest Ledger that is ahead of one batch.
#[tokio::test]
async fn the_gate_notices_a_ledger_that_is_stuck_behind_or_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let link = ledger().await;
    let mut gate = gate_at(dir.path(), 100, T0);
    call(&mut gate, 1, T0);
    call(&mut gate, 2, T0 + 1);
    assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(4));
    let hashes: Vec<gov_core::Digest> = FileSpool::open(dir.path())
        .unwrap()
        .held()
        .unwrap()
        .iter()
        .map(|r| r.hash.clone())
        .collect();

    // Stuck: new receipts are sent and the Ledger still holds only four.
    call(&mut gate, 3, T0 + 2);
    let stuck = Says {
        seq: 4,
        hash: Some(hashes[3].clone()),
        ..Says::default()
    };
    assert_eq!(
        gate.ship(&stuck).await.unwrap(),
        Shipped::NoProgress { refused: None }
    );
    assert_eq!(gate.state().acked_through, 4);

    // Behind: the Ledger was restored from a copy that held two, then from an empty one.
    let behind = Says {
        seq: 2,
        hash: Some(hashes[1].clone()),
        ..Says::default()
    };
    assert_eq!(gate.ship(&behind).await.unwrap(), Shipped::LedgerBehind(2));
    assert_eq!(gate.state().acked_through, 2);
    let empty = Says {
        seq: 0,
        hash: None,
        ..Says::default()
    };
    assert_eq!(gate.ship(&empty).await.unwrap(), Shipped::LedgerBehind(0));
    // A fresh, empty Ledger then receives the whole chain again, from receipt 1.
    let fresh = ledger().await;
    assert_eq!(gate.ship(&fresh).await.unwrap(), Shipped::Advanced(6));

    // Ahead: with batches of two, the Ledger (which already has all six) answers "six".
    let dir2 = tempfile::tempdir().unwrap();
    let mut small = Gate::open(
        GateConfig {
            batch: 2,
            policy_version: None,
            ..config(100)
        },
        gate_key(),
        FileSpool::open(dir2.path()).unwrap(),
        T0,
    )
    .unwrap();
    call(&mut small, 1, T0);
    call(&mut small, 2, T0 + 1);
    call(&mut small, 3, T0 + 2);
    // `fresh` holds exactly these six receipts already: same key, same calls, same times.
    assert_eq!(small.ship(&fresh).await.unwrap(), Shipped::Advanced(6));
}

/// Review finding GR-49: the spool is trimmed only on a checkpoint the gate reads from the
/// witness itself, never past what the Ledger acknowledged, and never on one that names
/// another hash.
#[tokio::test]
async fn the_spool_is_trimmed_only_on_what_the_witness_holds() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    let witness_dir = dir.path().join("witness");
    let link = ledger().await;
    let mut gate = gate_at(&spool, 100, T0);
    call(&mut gate, 1, T0);
    call(&mut gate, 2, T0 + 1);
    let held = FileSpool::open(&spool).unwrap().held().unwrap();

    // A checkpoint the Ledger signed and handed over, but never published: the gate has no
    // way to be handed one. It reads the witness, which is empty.
    let reader = DirWitnessReader::new(&witness_dir);
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 0);
    assert_eq!(gate.uncovered().unwrap(), 4);

    // The Ledger publishes a checkpoint covering receipts it was never sent (it knows the
    // hashes because it colludes, or guesses). Nothing acknowledged, so nothing is trimmed.
    struct Planted(Vec<(u64, Vec<u8>)>);
    impl WitnessReader for Planted {
        fn checkpoints(&self) -> Result<Vec<(u64, Vec<u8>)>, String> {
            Ok(self.0.clone())
        }
    }
    let covering = |seq: u64, hash: gov_core::Digest, number: u64, key: &SigningKey| {
        let cp = Checkpoint {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            issuer: id("ledger"),
            seq: number,
            at: T0 + 60,
            heads: vec![gov_core::lease::GateHead {
                gate: id("gate-1"),
                seq,
                hash,
            }],
        };
        (
            number,
            serde_json::to_vec(&seal(&cp, key).unwrap()).unwrap(),
        )
    };
    let unsent = Planted(vec![covering(4, held[3].hash.clone(), 1, &ledger_key())]);
    assert_eq!(gate.trim_to_witness(&unsent).unwrap().receipts, 0);
    assert_eq!(gate.uncovered().unwrap(), 4);

    // After a real acknowledgement of the first two only, that same checkpoint trims two.
    let two = Says {
        seq: 2,
        hash: Some(held[1].hash.clone()),
        ..Says::default()
    };
    assert_eq!(gate.ship(&two).await.unwrap(), Shipped::Advanced(2));
    // A checkpoint that names another hash for receipt 4: an alarm, nothing trimmed.
    let lying = Planted(vec![covering(
        4,
        gov_core::Digest::of(&[b"another history"]),
        1,
        &ledger_key(),
    )]);
    assert!(matches!(
        gate.trim_to_witness(&lying),
        Err(GateError::LedgerDisagrees(4))
    ));
    // One the Ledger's key did not sign, and one filed under the wrong number.
    let unsigned = Planted(vec![covering(4, held[3].hash.clone(), 1, &gate_key())]);
    // Neither is believed; each is reported, and neither is an error that stops the gate.
    let t = gate.trim_to_witness(&unsigned).unwrap();
    assert_eq!((t.receipts, t.bad_checkpoints), (0, vec![1]));
    let misfiled = Planted(vec![(
        7,
        covering(4, held[3].hash.clone(), 1, &ledger_key()).1,
    )]);
    let t = gate.trim_to_witness(&misfiled).unwrap();
    assert_eq!((t.receipts, t.bad_checkpoints), (0, vec![7]));
    assert_eq!(gate.uncovered().unwrap(), 4);
    assert_eq!(gate.trim_to_witness(&unsent).unwrap().receipts, 2);
    assert_eq!(gate.uncovered().unwrap(), 2);

    // Review finding GR-52: a crash after the state moved and before the files were removed
    // leaves covered receipts behind; opening the gate removes them.
    drop(gate);
    let leftover = spool.join(format!("receipt-{:020}.json", 1));
    std::fs::write(&leftover, serde_json::to_vec(&held[0]).unwrap()).unwrap();
    let gate = gate_at(&spool, 100, T0 + 90);
    assert!(!leftover.exists());
    assert_eq!(gate.uncovered().unwrap(), 2);
    let _ = link;
}

#[tokio::test]
async fn a_full_spool_says_so_and_an_open_intent_is_never_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let reader = DirWitnessReader::new(dir.path().join("witness"));
    let mut gate = gate_at(&dir.path().join("spool"), 4, T0);
    call(&mut gate, 1, T0);
    let a = action(2);
    // Room for one more call: its intent and its outcome.
    let in_flight = gate.begin(&observed(&agent, &a), T0).unwrap();
    // The next call does not fit, and the gate says so before the call, never after it.
    assert!(matches!(
        gate.begin(&observed(&agent, &a), T0),
        Err(GateError::SpoolFull)
    ));

    // A checkpoint covers all three; the open intent and what follows it stay in the spool.
    gate.ship(&link).await.unwrap();
    link.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 2);
    assert_eq!(gate.uncovered().unwrap(), 1);
    // The outcome of the call in flight always fits.
    assert_eq!(
        gate.finish(in_flight, OutcomeStatus::Ok, None, T0 + 61)
            .unwrap(),
        4
    );
    assert!(gate.begin(&observed(&agent, &a), T0 + 62).is_ok());
}

/// Review finding GR-54: custody means the gate holds the only credential, which it does
/// only when it enforces. An observed call cannot be recorded as custody.
#[tokio::test]
async fn an_observed_call_cannot_claim_custody() {
    let dir = tempfile::tempdir().unwrap();
    let agent = id("triage-bot");
    let mut gate = gate_at(dir.path(), 100, T0);
    let a = action(1);
    let claim = Observed {
        coverage: Coverage::Custody,
        ..observed(&agent, &a)
    };
    assert!(matches!(gate.begin(&claim, T0), Err(GateError::Core(_))));
    assert_eq!(gate.uncovered().unwrap(), 0);
}

/// Review finding GR-57: one unreadable checkpoint with the highest number does not stop the
/// gate trimming on the newest one that verifies. GR-56: once covered, the gate raises the
/// alarm itself when the witness shows its chain shorter or gone.
#[tokio::test]
async fn the_gate_reads_past_a_bad_checkpoint_and_notices_its_chain_going_back() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    let witness_dir = dir.path().join("witness");
    let link = ledger().await;
    let witness = DirWitness::new(&witness_dir);
    let reader = DirWitnessReader::new(&witness_dir);
    let mut gate = gate_at(&spool, 100, T0);
    call(&mut gate, 1, T0);
    gate.ship(&link).await.unwrap();
    link.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    // Someone drops a file that is not a checkpoint under the next number.
    std::fs::write(
        witness_dir.join(format!("checkpoint-{:020}.json", 2)),
        b"{not a checkpoint",
    )
    .unwrap();
    let t = gate.trim_to_witness(&reader).unwrap();
    assert_eq!((t.receipts, t.bad_checkpoints.clone()), (2, vec![2]));

    // A later checkpoint, rightly signed, that leaves this gate out; and one that shows its
    // chain at receipt 1 after the witness had covered receipt 2.
    struct Planted(Vec<(u64, Vec<u8>)>);
    impl WitnessReader for Planted {
        fn checkpoints(&self) -> Result<Vec<(u64, Vec<u8>)>, String> {
            Ok(self.0.clone())
        }
    }
    let signed = |heads: Vec<gov_core::lease::GateHead>| {
        let cp = Checkpoint {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            issuer: id("ledger"),
            seq: 3,
            at: T0 + 120,
            heads,
        };
        Planted(vec![(
            3,
            serde_json::to_vec(&seal(&cp, &ledger_key()).unwrap()).unwrap(),
        )])
    };
    assert!(matches!(
        gate.trim_to_witness(&signed(vec![])),
        Err(GateError::LedgerDisagrees(2))
    ));
    let shorter = signed(vec![gov_core::lease::GateHead {
        gate: id("gate-1"),
        seq: 1,
        hash: gov_core::Digest::of(&[b"x"]),
    }]);
    assert!(matches!(
        gate.trim_to_witness(&shorter),
        Err(GateError::LedgerDisagrees(2))
    ));
}

/// Review finding GR-58: an acknowledgement for a receipt already trimmed is checked against
/// the checkpoint the spool was trimmed on, and the signed answer that raised an alarm is
/// kept. GR-60: when the Ledger refuses a receipt, the gate passes on its reason.
#[tokio::test]
async fn an_alarming_answer_is_kept_and_a_refusal_is_passed_on() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let reader = DirWitnessReader::new(dir.path().join("witness"));
    let mut gate = gate_at(&spool, 100, T0);
    call(&mut gate, 1, T0);
    gate.ship(&link).await.unwrap();
    link.ledger
        .checkpoint(&ledger_key(), &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();
    assert_eq!(gate.trim_to_witness(&reader).unwrap().receipts, 2);
    assert!(FileSpool::open(&spool)
        .unwrap()
        .kept_alarms()
        .unwrap()
        .is_empty());
    call(&mut gate, 2, T0 + 61);

    // The Ledger now says it holds the chain only through receipt 2, which is trimmed, and
    // names a hash that is not the one in the checkpoint the gate kept.
    let rewritten = Says {
        seq: 2,
        hash: Some(gov_core::Digest::of(&[b"another receipt 2"])),
        ..Says::default()
    };
    assert!(matches!(
        gate.ship(&rewritten).await,
        Err(GateError::LedgerDisagrees(2))
    ));
    // The Ledger's own signed statement is kept as evidence.
    let kept = FileSpool::open(&spool).unwrap().kept_alarms().unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, 2);
    let env: Envelope = serde_json::from_slice(&kept[0].1).unwrap();
    let ack = open::<IntakeAck>(&env, &KeySet::new([ledger_key().public()]), &scope())
        .unwrap()
        .doc;
    assert_eq!(ack.seq, 2);

    // It lost receipts this gate can no longer send.
    let lost = Says {
        seq: 1,
        hash: Some(gov_core::Digest::of(&[b"x"])),
        ..Says::default()
    };
    assert!(matches!(
        gate.ship(&lost).await,
        Err(GateError::LedgerLost(1))
    ));
    // Each alarming answer is kept, under the receipt it speaks of (review finding GR-80):
    // the second did not replace the first.
    let seqs: Vec<u64> = FileSpool::open(&spool)
        .unwrap()
        .kept_alarms()
        .unwrap()
        .iter()
        .map(|(seq, _)| *seq)
        .collect();
    assert_eq!(seqs, vec![1, 2]);
    assert!(matches!(
        gate.ship(&lost).await,
        Err(GateError::LedgerLost(1))
    ));

    // It takes nothing and says why.
    let cp_hash = {
        let bytes = FileSpool::open(&spool)
            .unwrap()
            .kept_checkpoint()
            .unwrap()
            .unwrap();
        let env: Envelope = serde_json::from_slice(&bytes).unwrap();
        let cp = open::<Checkpoint>(&env, &KeySet::new([ledger_key().public()]), &scope())
            .unwrap()
            .doc;
        cp.heads[0].hash.clone()
    };
    let refusing = Says {
        seq: 2,
        hash: Some(cp_hash),
        refused: Some("outcome_without_intent".to_string()),
    };
    assert_eq!(
        gate.ship(&refusing).await.unwrap(),
        Shipped::NoProgress {
            refused: Some("outcome_without_intent".to_string())
        }
    );
}

/// A Ledger gone wrong cannot fill the gate's disk with alarms: the earliest are the ones
/// that stay.
#[test]
fn alarms_are_kept_up_to_a_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = FileSpool::open(dir.path()).unwrap();
    for seq in 1..=(gov_gate::spool::MAX_ALARMS as u64 + 10) {
        spool.keep_alarm(seq, b"{}").unwrap();
    }
    let kept = spool.kept_alarms().unwrap();
    assert_eq!(kept.len(), gov_gate::spool::MAX_ALARMS);
    assert_eq!(kept.first().unwrap().0, 1);
    // One already kept can still be written again.
    spool.keep_alarm(1, b"{\"again\":true}").unwrap();
    assert_eq!(spool.kept_alarms().unwrap()[0].1, b"{\"again\":true}");
}

/// A spool that counts how often it is read whole.
struct Counting {
    inner: FileSpool,
    scans: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
impl Spool for Counting {
    fn state(&self) -> Result<gov_gate::SpoolState, gov_gate::SpoolError> {
        self.inner.state()
    }
    fn append(
        &mut self,
        receipt: &gov_gate::Spooled,
        state: &gov_gate::SpoolState,
    ) -> Result<(), gov_gate::SpoolError> {
        self.inner.append(receipt, state)
    }
    fn close_intent(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.inner.close_intent(seq)
    }
    fn set_state(&mut self, state: &gov_gate::SpoolState) -> Result<(), gov_gate::SpoolError> {
        self.inner.set_state(state)
    }
    fn held(&self) -> Result<Vec<gov_gate::Spooled>, gov_gate::SpoolError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        self.inner.held()
    }
    fn get(&self, seq: u64) -> Result<Option<gov_gate::Spooled>, gov_gate::SpoolError> {
        self.inner.get(seq)
    }
    fn remove(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.inner.remove(seq)
    }
    fn trim(&mut self, seq: u64) -> Result<(), gov_gate::SpoolError> {
        self.inner.trim(seq)
    }
    fn keep_checkpoint(&mut self, bytes: &[u8]) -> Result<(), gov_gate::SpoolError> {
        self.inner.keep_checkpoint(bytes)
    }
    fn kept_checkpoint(&self) -> Result<Option<Vec<u8>>, gov_gate::SpoolError> {
        self.inner.kept_checkpoint()
    }
    fn keep_alarm(&mut self, seq: u64, bytes: &[u8]) -> Result<(), gov_gate::SpoolError> {
        self.inner.keep_alarm(seq, bytes)
    }
    fn kept_alarms(&self) -> Result<Vec<(u64, Vec<u8>)>, gov_gate::SpoolError> {
        self.inner.kept_alarms()
    }
}

/// A witness reader that counts how often the whole witness is read.
struct NewestOnly {
    inner: DirWitnessReader,
    whole: std::sync::atomic::AtomicU64,
}
impl gov_gate::WitnessReader for NewestOnly {
    fn checkpoints(&self) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.whole.fetch_add(1, Ordering::SeqCst);
        self.inner.checkpoints()
    }
    fn newest(&self, limit: usize) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.inner.newest(limit)
    }
}

/// The spool is read whole once, when the gate opens, and never again for a call, a
/// shipment or a trim; and a trim reads the newest end of the witness, not all of it (review
/// finding GR-82). A mode that blocks nothing must not slow to a stop as the spool fills.
#[tokio::test]
async fn a_call_never_reads_the_whole_spool_and_a_trim_never_the_whole_witness() {
    let dir = tempfile::tempdir().unwrap();
    let scans = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let spool = Counting {
        inner: FileSpool::open(dir.path().join("spool")).unwrap(),
        scans: scans.clone(),
    };
    let mut gate = Gate::open(config(1000), gate_key(), spool, T0).unwrap();
    assert_eq!(scans.load(Ordering::SeqCst), 1);

    let link = ledger().await;
    let witness = DirWitness::new(dir.path().join("witness"));
    let reader = NewestOnly {
        inner: DirWitnessReader::new(dir.path().join("witness")),
        whole: Default::default(),
    };
    let agent = id("triage-bot");
    let mut trimmed = 0;
    for round in 0..3u64 {
        for n in 0..4u64 {
            let a = action(round * 10 + n);
            let now = T0 + (round * 10 + n) as i64;
            let seq = gate.begin(&observed(&agent, &a), now).unwrap();
            gate.finish(seq, OutcomeStatus::Ok, None, now).unwrap();
        }
        assert_eq!(gate.ship(&link).await.unwrap(), Shipped::Advanced(8));
        link.ledger
            .checkpoint(
                &ledger_key(),
                &id("ledger"),
                T0 + 100 + round as i64 * 60,
                &witness,
            )
            .await
            .unwrap();
        trimmed += gate.trim_to_witness(&reader).unwrap().receipts;
    }
    assert_eq!((trimmed, gate.uncovered().unwrap()), (24, 0));
    assert_eq!(
        scans.load(Ordering::SeqCst),
        1,
        "the spool was read whole again"
    );
    assert_eq!(reader.whole.load(Ordering::SeqCst), 0);

    // The newest end of a long witness is what is read: the last three of these, in order.
    let numbers: Vec<u64> = DirWitnessReader::new(dir.path().join("witness"))
        .newest(2)
        .unwrap()
        .iter()
        .map(|(seq, _)| *seq)
        .collect();
    assert_eq!(numbers, vec![2, 3]);
}
