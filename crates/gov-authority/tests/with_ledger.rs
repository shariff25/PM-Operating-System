//! Slice G4a, second part: the Authority's chain goes to the Ledger and is witnessed there,
//! and a gate's key reaches the Ledger only on the Authority's word, backed by the record of
//! its enrolment (spec §7.6).

use std::collections::BTreeSet;

// The Ledger's own test helpers, for a gate's receipts.
#[path = "../../gov-ledger/tests/common/mod.rs"]
mod common;

use gov_authority::{Authority, AuthorityError, MemoryStore, Role};
use gov_core::admin::AdminAction;
use gov_core::enrol::{code_digest, EnrolKind, EnrolRequest};
use gov_core::lease::IntakeAck;
use gov_core::{seal, Digest, Envelope, Id, KeySet, Scope, SigningKey};
use gov_ledger::verify::{verify, Check, Problem};
use gov_ledger::{DirWitness, Ledger, LedgerError, Store as LedgerStore, TrustedKeys, Witness};

const NOW: i64 = 1_800_000_000;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}
fn admin_key() -> SigningKey {
    key("authority-admin-1", 60)
}
fn ledger_key() -> SigningKey {
    key("ledger-1", 50)
}
fn ledger_keys() -> KeySet {
    KeySet::new([ledger_key().public()])
}

type TheAuthority = Authority<MemoryStore>;
type TheLedger = Ledger<gov_ledger::MemoryStore>;

/// Enrol `gate`'s key at the Authority with a fresh code.
async fn enrol_gate(a: &TheAuthority, gate: &str, gate_key: &SigningKey) {
    let code = a
        .offer_enrolment(&id("ana"), EnrolKind::Gate, &id(gate), NOW)
        .await
        .unwrap();
    let request = seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind: EnrolKind::Gate,
            name: id(gate),
            key: gate_key.public(),
            code: code_digest(&code),
        },
        gate_key,
    )
    .unwrap();
    a.enrol(&code, &request, NOW).await.unwrap();
}

/// Ship everything the Ledger has not acknowledged, and return what it now acknowledges.
async fn ship(a: &TheAuthority, ledger: &TheLedger, acked: u64) -> u64 {
    let shipment = a.shipment(acked, 100).await.unwrap().unwrap();
    let (ack, refused) = ledger
        .take_shipment(
            &shipment.request,
            &shipment.batch,
            &ledger_key(),
            &id("ledger"),
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(refused, None);
    a.check_answer(&shipment, &ack, &ledger_keys())
        .await
        .unwrap()
}

fn refused(r: Result<(), LedgerError>) {
    assert!(
        matches!(r, Err(LedgerError::Refused("key_not_enrolled"))),
        "{r:?}"
    );
}

async fn installed() -> (TheAuthority, TheLedger) {
    let a = Authority::new(
        MemoryStore::default(),
        scope(),
        id("authority"),
        admin_key(),
        key("authority-root-1", 61).public(),
    )
    .with_online_keys(
        key("authority:mandates-1", 71),
        key("authority:leases-1", 72),
    );
    let ana = id("ana");
    a.bootstrap(&ana, NOW).await.unwrap();
    a.add_person(&ana, &id("ben"), Role::Owner, NOW)
        .await
        .unwrap();
    a.register_agent(&ana, &id("refund-bot"), &id("ben"), BTreeSet::new(), NOW)
        .await
        .unwrap();
    for gate in ["gate-1", "gate-2"] {
        a.register_gate(&ana, &id(gate), NOW).await.unwrap();
    }
    // The Ledger is given the Authority's admin-record key at installation, and nothing else.
    let ledger = Ledger::new(gov_ledger::MemoryStore::default(), scope());
    ledger
        .enrol_admin_key(&id("authority"), &a.admin_public())
        .await
        .unwrap();
    (a, ledger)
}

#[tokio::test]
async fn the_authoritys_chain_is_witnessed_and_a_gates_key_follows_from_it() {
    let (a, ledger) = installed().await;
    let g1 = key("gate-1:k1", 20);
    enrol_gate(&a, "gate-1", &g1).await;
    assert_eq!(a.gate_keys().await.unwrap(), vec![g1.public()]);

    // The key alone enrols nothing: the Ledger does not yet hold the record of its
    // enrolment, so it is not a key the witness could have seen.
    refused(ledger.supply_key(&g1.public()).await);
    assert!(ledger
        .store()
        .gate_key(&scope(), &id("gate-1:k1"))
        .await
        .unwrap()
        .is_none());
    let receipts = common::chain("gate-1", &g1, 4, NOW, 1);
    assert!(
        ledger.ingest(&receipts[0], NOW).await.is_err(),
        "no receipt before the record"
    );

    // The chain is shipped, and the Ledger's signed answer agrees with the Authority's own.
    let acked = ship(&a, &ledger, 0).await;
    assert_eq!(acked, 7);
    // Another key offered under the enrolled key's id, before the real one has been: the
    // record names a key by its digest, and this is not that key.
    let impostor = SigningKey::from_seed(id("gate-1:k1"), &[21; 32]).unwrap();
    refused(ledger.supply_key(&impostor.public()).await);
    assert!(ledger
        .store()
        .gate_key(&scope(), &id("gate-1:k1"))
        .await
        .unwrap()
        .is_none());
    // Now the key is backed by a witnessed record: it is enrolled for the gate's receipts,
    // for the gate the record names, whoever offers it and however often.
    ledger.supply_key(&g1.public()).await.unwrap();
    ledger.supply_key(&g1.public()).await.unwrap();
    let (gate, held) = ledger
        .store()
        .gate_key(&scope(), &id("gate-1:k1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((gate, held), (id("gate-1"), g1.public()));
    assert_eq!(
        ledger
            .store()
            .chain_kind(&scope(), &id("gate-1"))
            .await
            .unwrap(),
        Some(gov_ledger::ChainKind::Receipt)
    );
    for env in &receipts[..2] {
        ledger.ingest(env, NOW).await.unwrap();
    }

    // What does not enrol a key at the Ledger: a key nobody enrolled; another key under an
    // enrolled key's id; an enrolled key's bytes under another id.
    let stranger = key("gate-2:k1", 99);
    refused(ledger.supply_key(&stranger.public()).await);
    let same_id = SigningKey::from_seed(id("gate-1:k1"), &[21; 32]).unwrap();
    refused(ledger.supply_key(&same_id.public()).await);
    let same_key = SigningKey::from_seed(id("gate-1:k7"), &[20; 32]).unwrap();
    refused(ledger.supply_key(&same_key.public()).await);
    // An agent's key is in the Authority's chain too, and is no gate's: agents send no
    // receipts, and the Ledger takes none under an agent's key.
    let bot = key("refund-bot:k1", 9);
    let code = a
        .offer_enrolment(&id("ana"), EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .unwrap();
    let asked = seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind: EnrolKind::Agent,
            name: id("refund-bot"),
            key: bot.public(),
            code: code_digest(&code),
        },
        &bot,
    )
    .unwrap();
    a.enrol(&code, &asked, NOW).await.unwrap();
    let acked = ship(&a, &ledger, acked).await;
    refused(ledger.supply_key(&bot.public()).await);
    assert_eq!(a.gate_keys().await.unwrap(), vec![g1.public()]);

    // A gate enrolled and then disabled, before the Ledger ever saw its key: once the Ledger
    // holds both records the key is not taken.
    let g2 = key("gate-2:k1", 22);
    enrol_gate(&a, "gate-2", &g2).await;
    a.disable_gate(&id("ana"), &id("gate-2"), NOW)
        .await
        .unwrap();
    let acked = ship(&a, &ledger, acked).await;
    refused(ledger.supply_key(&g2.public()).await);

    // The witness sees gate-1's receipts before anything is said of disabling it.
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 30, &witness)
        .await
        .unwrap();

    // A gate disabled while in use: when the Ledger takes in that record, the gate's key is
    // retired. What it sent before stays and still verifies; nothing more is taken, and it
    // gets no answer to a shipment (review finding GR-20).
    a.disable_gate(&id("ana"), &id("gate-1"), NOW)
        .await
        .unwrap();
    assert!(a.gate_keys().await.unwrap().is_empty());
    let acked = ship(&a, &ledger, acked).await;
    assert_eq!(acked, 13);
    assert!(ledger
        .store()
        .key_retired(&scope(), &id("gate-1:k1"))
        .await
        .unwrap());
    assert!(matches!(
        ledger.ingest(&receipts[2], NOW).await,
        Err(LedgerError::Refused("retired_key"))
    ));
    refused(ledger.supply_key(&g1.public()).await);
    a.register_gate(&id("ana"), &id("gate-9"), NOW)
        .await
        .unwrap();
    assert_eq!(ship(&a, &ledger, acked).await, 14);

    // The Authority's chain is under the same witness as the gates', and an auditor's check
    // of the export covers both, the retired gate's receipts included.
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &witness)
        .await
        .unwrap();
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    let report = verify(&Check {
        scope: &scope(),
        keys: &keys,
        export: &export,
        checkpoints: &witness.all().unwrap(),
        interval_secs: 60,
        now: Some(NOW + 61),
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: None,
    });
    // Every signature, chain and key verifies. The one finding is that gate-1's receipts,
    // made by the test helper, name a mandate (m-1) this Authority never issued; the test
    // below, of a mandate it did issue, has none.
    assert_eq!(
        report
            .findings
            .iter()
            .map(|f| (f.problem.clone(), f.gate.clone(), f.seq))
            .collect::<Vec<_>>(),
        vec![(Problem::MandateNotIssued, Some(id("gate-1")), Some(1))]
    );
    assert_eq!(report.receipts_verified, 14 + 2);

    // The auditor's check believes a gate's key only as far as a witnessed record of its
    // enrolment bears it out (GR-84). The keys file comes from the Ledger being audited.
    let checkpoints = witness.all().unwrap();
    let problems = |keys: &TrustedKeys, checkpoints: &[(u64, Vec<u8>)], skew: u32| {
        verify(&Check {
            scope: &scope(),
            keys,
            export: &export,
            checkpoints,
            interval_secs: 60,
            now: Some(NOW + 61),
            gate_quiet_secs: None,
            clock_skew_secs: skew,
            retention_secs: None,
        })
        .findings
        .into_iter()
        // The finding about the helper's mandate, checked above, is in every answer.
        .filter(|f| f.problem != Problem::MandateNotIssued)
        .map(|f| (f.problem, f.gate))
        .collect::<Vec<_>>()
    };
    let not_enrolled = |gate: &str| (Problem::KeyNotEnrolled, Some(id(gate)));
    // A key the Ledger added for a gate on its own, which no record names.
    let mut padded = keys.clone();
    let listed = |keys: &mut TrustedKeys, key: gov_core::PublicKey| {
        keys.gates.get_mut(&id("gate-1")).unwrap().push(key);
    };
    listed(&mut padded, key("gate-1:k9", 99).public());
    assert_eq!(
        problems(&padded, &checkpoints, 60),
        vec![not_enrolled("gate-1")]
    );
    // A key enrolled for one gate (gate-2's, by a witnessed record), listed for another.
    let mut moved = keys.clone();
    listed(&mut moved, g2.public());
    assert_eq!(
        problems(&moved, &checkpoints, 60),
        vec![not_enrolled("gate-1")]
    );
    // Another key under an enrolled key's id: the record names the key by its digest.
    let mut swapped = keys.clone();
    swapped
        .gates
        .insert(id("gate-1"), vec![key("gate-1:k1", 98).public()]);
    assert!(problems(&swapped, &checkpoints, 60).contains(&not_enrolled("gate-1")));
    // An agent's enrolled key, listed as the key of a gate of the agent's name: an agent's
    // enrolment makes no gate key.
    let mut agent = keys.clone();
    agent.gates.insert(id("refund-bot"), vec![bot.public()]);
    assert!(problems(&agent, &checkpoints, 60).contains(&not_enrolled("refund-bot")));
    // A record of enrolment no checkpoint covers is the Ledger's word alone.
    assert!(problems(&keys, &[], 60).contains(&not_enrolled("gate-1")));
    // With no Authority named there is nothing to check a key against: the gates' chains
    // verify as before, and the only findings are that the Authority's chain is one the
    // auditor holds no key for.
    let mut alone = keys.clone();
    alone.authorities.clear();
    let found = problems(&alone, &checkpoints, 60);
    assert!(!found.is_empty());
    assert!(found
        .iter()
        .all(|(p, gate)| *p != Problem::KeyNotEnrolled && *gate == Some(id("authority"))));
    // The witness's order decides whether a receipt is earlier than its gate's disabling,
    // whatever dates either carries (GR-130). Above, a checkpoint covered gate-1's receipts
    // before the one that covered the record. Without that earlier checkpoint nothing
    // shows them to be earlier, and the last of them is reported.
    assert!(problems(&keys, &checkpoints, 0).is_empty());
    let late_only = &checkpoints[1..];
    assert!(problems(&keys, late_only, 60)
        .contains(&(Problem::ReceiptAfterRetirement, Some(id("gate-1")))));
    // The order is by the checkpoints' own numbers, not by where each stands among those
    // that name a chain (GR-186): the Authority's chain is witnessed first, alone, and then
    // one checkpoint covers the receipts and the disabling together.
    let open_cp = |bytes: &[u8]| {
        let envelope: Envelope = serde_json::from_slice(bytes).unwrap();
        gov_core::open::<gov_core::lease::Checkpoint>(&envelope, &ledger_keys(), &scope())
            .unwrap()
            .doc
    };
    let sealed = |cp: &gov_core::lease::Checkpoint| {
        (
            cp.seq,
            serde_json::to_vec(&seal(cp, &ledger_key()).unwrap()).unwrap(),
        )
    };
    let mut alone_first = open_cp(&checkpoints[0].1);
    alone_first.heads.retain(|h| h.gate == id("authority"));
    let mut together = open_cp(&checkpoints[1].1);
    together.seq = 2;
    assert!(
        problems(&keys, &[sealed(&alone_first), sealed(&together)], 60)
            .contains(&(Problem::ReceiptAfterRetirement, Some(id("gate-1"))))
    );
    // What was witnessed says which key was enrolled for which gate or agent, in order.
    let enrolled: Vec<Id> = a
        .records()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == AdminAction::KeyEnrolled)
        .map(|r| r.subject.id)
        .collect();
    assert_eq!(
        enrolled,
        vec![id("gate-1:k1"), id("refund-bot:k1"), id("gate-2:k1")]
    );
}

#[tokio::test]
async fn the_authority_checks_the_ledgers_answer_against_its_own_chain() {
    let (a, ledger) = installed().await;
    let first = a.shipment(0, 2).await.unwrap().unwrap();
    assert_eq!(first.batch.len(), 2);
    let (ack, _) = ledger
        .take_shipment(
            &first.request,
            &first.batch,
            &ledger_key(),
            &id("ledger"),
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(
        a.check_answer(&first, &ack, &ledger_keys()).await.unwrap(),
        2
    );

    // An answer to another shipment, played back.
    let second = a.shipment(2, 100).await.unwrap().unwrap();
    assert!(a.check_answer(&second, &ack, &ledger_keys()).await.is_err());
    // An answer signed by someone else.
    let other = KeySet::new([key("another-ledger", 51).public()]);
    assert!(a.check_answer(&first, &ack, &other).await.is_err());

    // A Ledger that says it holds a record this Authority did not write under that number.
    let says = |seq: u64, hash: Option<Digest>, gate: &str, nonce: &Envelope| {
        let asked = gov_core::open::<gov_core::lease::ShipRequest>(
            nonce,
            &KeySet::new([admin_key().public()]),
            &scope(),
        )
        .unwrap()
        .doc;
        seal(
            &IntakeAck {
                v: 1,
                tenant: id("acme"),
                env: id("prod"),
                issuer: id("ledger"),
                gate: id(gate),
                nonce: asked.nonce,
                seq,
                hash,
                at: NOW,
            },
            &ledger_key(),
        )
        .unwrap()
    };
    let lie = says(
        2,
        Some(Digest::of(&[b"another record 2"])),
        "authority",
        &second.request,
    );
    assert!(matches!(
        a.check_answer(&second, &lie, &ledger_keys()).await,
        Err(AuthorityError::LedgerDisagrees(2))
    ));
    // One that names a record beyond the chain, and one that answers for another chain.
    let beyond = says(99, Some(Digest::of(&[b"x"])), "authority", &second.request);
    assert!(matches!(
        a.check_answer(&second, &beyond, &ledger_keys()).await,
        Err(AuthorityError::LedgerDisagrees(99))
    ));
    let elsewhere = says(0, None, "gate-1", &second.request);
    assert!(a
        .check_answer(&second, &elsewhere, &ledger_keys())
        .await
        .is_err());
    // An honest "I hold nothing" is believed: a Ledger restored from an older copy says so.
    let nothing = says(0, None, "authority", &second.request);
    assert_eq!(
        a.check_answer(&second, &nothing, &ledger_keys())
            .await
            .unwrap(),
        0
    );
    // Nothing left to send once everything is acknowledged.
    let (ack, _) = ledger
        .take_shipment(
            &second.request,
            &second.batch,
            &ledger_key(),
            &id("ledger"),
            NOW,
        )
        .await
        .unwrap();
    let acked = a.check_answer(&second, &ack, &ledger_keys()).await.unwrap();
    assert!(a.shipment(acked, 100).await.unwrap().is_none());
}

/// Put a record of the Authority's straight into the Ledger's store, as the Ledger would
/// have left it had it stopped after keeping the record and before acting on it.
async fn kept_but_not_acted_on(ledger: &TheLedger, envelope: &Envelope, admin_keys: &KeySet) {
    let opened =
        gov_core::open::<gov_core::admin::AdminRecord>(envelope, admin_keys, &scope()).unwrap();
    let stored = gov_ledger::store::StoredReceipt {
        seq: opened.doc.seq,
        hash: opened.hash,
        at: opened.doc.at,
        received_at: NOW,
        envelope: envelope.clone(),
    };
    ledger
        .store()
        .append(&scope(), &id("authority"), opened.doc.seq - 1, &stored)
        .await
        .unwrap();
}

/// A Ledger that stopped between keeping a record and acting on it acts on it when the
/// Authority sends it again (review finding GR-123), an Authority's chain is not pruned
/// (GR-122), and a key offered to the Ledger is looked up, not searched for (GR-124).
#[tokio::test]
async fn a_record_sent_again_is_acted_on_and_the_authoritys_chain_is_kept() {
    let (a, ledger) = installed().await;
    let admin_keys = KeySet::new([a.admin_public()]);
    let g1 = key("gate-1:k1", 20);
    let acked = ship(&a, &ledger, 0).await;

    // The record of an enrolment, kept and not acted on: the key is not yet one the Ledger
    // will take. Sent again, it is noted, and the key is taken.
    enrol_gate(&a, "gate-1", &g1).await;
    let batch = a.shipment(acked, 100).await.unwrap().unwrap().batch;
    for envelope in &batch {
        kept_but_not_acted_on(&ledger, envelope, &admin_keys).await;
    }
    refused(ledger.supply_key(&g1.public()).await);
    let acked = ship(&a, &ledger, acked).await;
    ledger.supply_key(&g1.public()).await.unwrap();
    let receipts = common::chain("gate-1", &g1, 2, NOW, 1);
    ledger.ingest(&receipts[0], NOW).await.unwrap();

    // The record that disables the gate, kept and not acted on: the key is still in use.
    // Sent again, the key is retired and can no longer be supplied.
    a.disable_gate(&id("ana"), &id("gate-1"), NOW)
        .await
        .unwrap();
    let batch = a.shipment(acked, 100).await.unwrap().unwrap().batch;
    for envelope in &batch {
        kept_but_not_acted_on(&ledger, envelope, &admin_keys).await;
    }
    let retired = || async {
        ledger
            .store()
            .key_retired(&scope(), &id("gate-1:k1"))
            .await
            .unwrap()
    };
    assert!(!retired().await);
    let acked = ship(&a, &ledger, acked).await;
    assert!(retired().await);
    assert!(matches!(
        ledger.ingest(&receipts[1], NOW).await,
        Err(LedgerError::Refused("retired_key"))
    ));
    refused(ledger.supply_key(&g1.public()).await);

    // A gate disabled before its key was ever supplied: the enrolment is ended in what
    // the Ledger noted, so the key is not taken afterwards.
    let g2 = key("gate-2:k1", 22);
    enrol_gate(&a, "gate-2", &g2).await;
    let acked = ship(&a, &ledger, acked).await;
    a.disable_gate(&id("ana"), &id("gate-2"), NOW)
        .await
        .unwrap();
    let acked = ship(&a, &ledger, acked).await;
    refused(ledger.supply_key(&g2.public()).await);

    // What the Ledger noted is what the records said, by key id.
    let noted = ledger
        .store()
        .enrolment(&scope(), &id("gate-1:k1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (&noted.gate, noted.ended, &noted.authority),
        (&id("gate-1"), true, &id("authority"))
    );

    // A Ledger restored from another copy can hold notes that say more, or less, than its
    // chain (GR-131). At start the notes are written again from the chain: a note with no
    // record behind it goes, a missing one comes back, and an ended one is ended again.
    let stray = gov_ledger::Enrolment {
        authority: id("authority"),
        gate: id("gate-7"),
        digest: gov_core::enrol::key_digest(&key("gate-7:k1", 77).public()).unwrap(),
        ended: false,
    };
    ledger.store().clear_enrolments(&scope()).await.unwrap();
    ledger
        .store()
        .note_enrolment(&scope(), &id("gate-7:k1"), &stray)
        .await
        .unwrap();
    // A live note for a gate the chain says is disabled.
    let revived = gov_ledger::Enrolment {
        ended: false,
        ..noted.clone()
    };
    ledger
        .store()
        .note_enrolment(
            &scope(),
            &id("gate-2:k1"),
            &gov_ledger::Enrolment {
                gate: id("gate-2"),
                digest: gov_core::enrol::key_digest(&g2.public()).unwrap(),
                ..revived
            },
        )
        .await
        .unwrap();
    assert_eq!(ledger.rebuild_notes().await.unwrap(), acked);
    let note = |kid: &'static str| {
        let ledger = &ledger;
        async move { ledger.store().enrolment(&scope(), &id(kid)).await.unwrap() }
    };
    assert!(note("gate-7:k1").await.is_none());
    assert!(note("gate-1:k1").await.unwrap().ended);
    assert!(note("gate-2:k1").await.unwrap().ended);
    refused(ledger.supply_key(&g2.public()).await);
    refused(ledger.supply_key(&key("gate-7:k1", 77).public()).await);
    // Done twice it says the same.
    ledger.rebuild_notes().await.unwrap();
    assert!(note("gate-1:k1").await.unwrap().ended);

    // The Authority's chain is not pruned, however old: it is what says which keys are
    // trusted. A gate's chain of the same age is.
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &witness)
        .await
        .unwrap();
    let far = NOW + 1_000_000;
    let prune = |chain: &'static str| {
        let (ledger, witness) = (&ledger, &witness);
        async move {
            ledger
                .prune(&id(chain), far, &ledger_key(), &id("ledger"), far, witness)
                .await
                .unwrap()
        }
    };
    assert_eq!(prune("authority").await, None);
    assert_eq!(prune("gate-1").await, Some(1));
    let held = ledger
        .store()
        .receipts(&scope(), &id("authority"))
        .await
        .unwrap();
    assert_eq!(held.len() as u64, acked);
    assert_eq!(held[0].seq, 1);
}

/// A record that disables "a gate" of the Authority's own name retires a gate's keys only,
/// never those of the Authority's chain. The Authority no longer registers such a gate
/// (GR-127), so the record here is made by hand, as a faulty or taken-over Authority's
/// would be: the Ledger's rule does not lean on the Authority's.
#[tokio::test]
async fn a_record_about_a_gate_never_retires_the_authoritys_own_key() {
    use gov_core::admin::{Actor, AdminRecord, Subject, SubjectKind};
    let (a, ledger) = installed().await;
    let acked = ship(&a, &ledger, 0).await;
    let batch = a.shipment(0, 100).await.unwrap().unwrap().batch;
    let admin_keys = KeySet::new([a.admin_public()]);
    let last = gov_core::open::<AdminRecord>(batch.last().unwrap(), &admin_keys, &scope()).unwrap();
    let by_hand = AdminRecord {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        authority: id("authority"),
        seq: acked + 1,
        prev: Some(last.hash),
        at: NOW,
        actor: Actor {
            id: id("ana"),
            kind: Default::default(),
            verified: false,
            session: None,
        },
        action: AdminAction::GateDisabled,
        subject: Subject {
            kind: SubjectKind::Gate,
            id: id("authority"),
        },
        about: None,
        value: None,
        until: None,
    };
    let envelope = seal(&by_hand, &admin_key()).unwrap();
    ledger.ingest(&envelope, NOW).await.unwrap();
    assert!(!ledger
        .store()
        .key_retired(&scope(), &a.admin_public().kid)
        .await
        .unwrap());
}

/// A record made by hand under `signer`, following `prev`: what a taken key would write.
fn by_hand(
    seq: u64,
    prev: Digest,
    action: AdminAction,
    subject: (gov_core::admin::SubjectKind, &str),
    about: Option<(gov_core::admin::SubjectKind, &str)>,
    value: Option<Digest>,
    signer: &SigningKey,
) -> Envelope {
    use gov_core::admin::{Actor, AdminRecord, Subject};
    let record = AdminRecord {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        authority: id("authority"),
        seq,
        prev: Some(prev),
        at: NOW,
        actor: Actor {
            id: id("ana"),
            kind: Default::default(),
            verified: false,
            session: None,
        },
        action,
        subject: Subject {
            kind: subject.0,
            id: id(subject.1),
        },
        about: about.map(|(kind, name)| Subject { kind, id: id(name) }),
        value,
        until: None,
    };
    seal(&record, signer).unwrap()
}

/// The Authority's admin-record key is replaced (spec §7.6.3, point 9; GR-166): the old key
/// signs the record of it and nothing after, at the Ledger and in the auditor's check.
#[tokio::test]
async fn the_authoritys_own_key_is_replaced_by_a_record_in_its_chain() {
    use gov_core::admin::SubjectKind;
    use gov_core::enrol::key_digest;
    let (mut a, ledger) = installed().await;
    let (ana, ben) = (id("ana"), id("ben"));
    let (old, next) = (admin_key(), key("authority-admin-2", 62));
    let acked = ship(&a, &ledger, 0).await;

    assert!(matches!(
        a.replace_admin_key(&ben, key("authority-admin-2", 62), NOW)
            .await,
        Err(AuthorityError::NotAllowed)
    ));
    assert!(a.replace_admin_key(&ana, admin_key(), NOW).await.is_err());
    // Nor a key, or a key id, that a gate or an agent holds (GR-189).
    let g1 = key("gate-1:k1", 20);
    enrol_gate(&a, "gate-1", &g1).await;
    for taken in [key("gate-1:k1", 62), key("authority-admin-2", 20)] {
        assert!(matches!(
            a.replace_admin_key(&ana, taken, NOW).await,
            Err(AuthorityError::Exists("key"))
        ));
    }
    let acked = ship(&a, &ledger, acked).await;
    let at = a
        .replace_admin_key(&ana, key("authority-admin-2", 62), NOW)
        .await
        .unwrap();
    assert_eq!(a.admin_public(), next.public());
    a.register_gate(&ana, &id("gate-3"), NOW).await.unwrap();
    let records = a.records().await.unwrap();
    let replaced = &records[at as usize - 1];
    assert_eq!(replaced.action, AdminAction::AdminKeyReplaced);
    assert_eq!(replaced.subject.id, next.public().kid);
    assert_eq!(replaced.value, Some(key_digest(&next.public()).unwrap()));

    // The Ledger knows the new key only from the record, so it is not taken before.
    refused(ledger.supply_key(&next.public()).await);
    let sent = a.shipment(acked, 100).await.unwrap().unwrap();
    assert_eq!(sent.request.kid, old.public().kid);
    assert_eq!(sent.batch.len() as u64, at - acked);
    assert_eq!(sent.batch.last().unwrap().kid, old.public().kid);
    assert_eq!(ship(&a, &ledger, acked).await, at);
    // Another key under the new key's id is not the one the record named.
    refused(
        ledger
            .supply_key(&key("authority-admin-2", 63).public())
            .await,
    );
    ledger.supply_key(&next.public()).await.unwrap();
    let after = a.shipment(at, 100).await.unwrap().unwrap();
    assert_eq!(after.request.kid, next.public().kid);
    assert_eq!(ship(&a, &ledger, at).await, at + 1);

    // The old key is answered nothing and signs nothing more; what it signed comes again.
    let all = a.shipment(0, 100).await.unwrap().unwrap();
    assert!(matches!(
        ledger
            .take_shipment(&all.request, &all.batch, &ledger_key(), &id("ledger"), NOW)
            .await,
        Err(LedgerError::Refused("unknown_sender"))
    ));
    a.settle_admin_key();
    assert_eq!(ship(&a, &ledger, 0).await, at + 1);
    let both = KeySet::new([old.public(), next.public()]);
    let last = a.shipment(at, 100).await.unwrap().unwrap().batch;
    let last = gov_core::open::<gov_core::admin::AdminRecord>(&last[0], &both, &scope()).unwrap();
    let taken = by_hand(
        at + 2,
        last.hash.clone(),
        AdminAction::GateDisabled,
        (SubjectKind::Gate, "gate-2"),
        None,
        None,
        &old,
    );
    assert!(matches!(
        ledger.ingest(&taken, NOW).await,
        Err(LedgerError::Refused("retired_key"))
    ));

    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &witness)
        .await
        .unwrap();
    assert_eq!(findings_of(&ledger, &witness, NOW + 61).await, vec![]);

    // An export in which the old key went on signing is reported, at that record.
    let mut export = ledger.export().await.unwrap();
    let chain = export
        .gates
        .iter_mut()
        .find(|g| g.gate == id("authority"))
        .unwrap();
    chain.receipts.push(taken);
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    let report = verify(&Check {
        scope: &scope(),
        keys: &keys,
        export: &export,
        checkpoints: &witness.all().unwrap(),
        interval_secs: 60,
        now: Some(NOW + 61),
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: None,
    });
    assert!(report.findings.iter().any(|f| {
        f.problem == Problem::RecordUnderReplacedKey
            && f.gate == Some(id("authority"))
            && f.seq == Some(at + 2)
    }));

    // A key enrolled for a gate that bears the Authority's name is no key of the
    // Authority's: only a replacement names one.
    let stray = key("authority:k9", 64);
    let named = by_hand(
        at + 2,
        last.hash,
        AdminAction::KeyEnrolled,
        (SubjectKind::Key, "authority:k9"),
        Some((SubjectKind::Gate, "authority")),
        Some(key_digest(&stray.public()).unwrap()),
        &next,
    );
    ledger.ingest(&named, NOW).await.unwrap();
    refused(ledger.supply_key(&stray.public()).await);
}

/// A replacement that names a key id the Ledger already holds a note for cannot be
/// followed, so the key that signed it stays in use: the chain is not left with no key
/// (GR-189). The record is made by hand; the Authority refuses to write it.
#[tokio::test]
async fn a_replacement_the_ledger_cannot_follow_retires_nothing() {
    use gov_core::admin::{AdminRecord, SubjectKind};
    use gov_core::enrol::key_digest;
    let (a, ledger) = installed().await;
    enrol_gate(&a, "gate-1", &key("gate-1:k1", 20)).await;
    let acked = ship(&a, &ledger, 0).await;
    let batch = a.shipment(0, 100).await.unwrap().unwrap().batch;
    let admin_keys = KeySet::new([a.admin_public()]);
    let last = gov_core::open::<AdminRecord>(batch.last().unwrap(), &admin_keys, &scope()).unwrap();
    let colliding = by_hand(
        acked + 1,
        last.hash,
        AdminAction::AdminKeyReplaced,
        (SubjectKind::Key, "gate-1:k1"),
        None,
        Some(key_digest(&key("gate-1:k1", 62).public()).unwrap()),
        &admin_key(),
    );
    ledger.ingest(&colliding, NOW).await.unwrap();
    assert!(!ledger
        .store()
        .key_retired(&scope(), &a.admin_public().kid)
        .await
        .unwrap());
}

/// A receipt under a revoked key, as distinct from a disabled gate's: earlier than the
/// revocation only where the witness shows it. The late one is put into the Ledger's
/// store directly, as a Ledger at fault would hold it.
#[tokio::test]
async fn a_receipt_under_a_revoked_key_is_judged_by_the_witness() {
    let (a, ledger) = installed().await;
    let (g1, g2) = (key("gate-1:k1", 20), key("gate-1:k2", 21));
    enrol_gate(&a, "gate-1", &g1).await;
    enrol_gate(&a, "gate-1", &g2).await;
    let acked = ship(&a, &ledger, 0).await;
    ledger.supply_key(&g1.public()).await.unwrap();
    ledger.supply_key(&g2.public()).await.unwrap();
    let mut prev = None;
    let mut receipts = Vec::new();
    for seq in 1..=3u64 {
        let r = common::receipt("gate-1", seq, prev.clone(), NOW + seq as i64);
        let env = seal(&r, &g1).unwrap();
        prev = Some(common::hash_of(&env, &g1));
        receipts.push((env, prev.clone().unwrap()));
    }
    for (env, _) in &receipts[..2] {
        ledger.ingest(env, NOW).await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    let (lk, issuer) = (ledger_key(), id("ledger"));
    let checkpoint = |at: i64| ledger.checkpoint(&lk, &issuer, at, &witness);
    checkpoint(NOW + 30).await.unwrap();
    a.revoke_key(&id("ana"), &id("gate-1:k1"), NOW)
        .await
        .unwrap();
    ship(&a, &ledger, acked).await;
    checkpoint(NOW + 60).await.unwrap();
    let after_retirement = |found: Vec<(Problem, Option<Id>, Option<u64>)>| {
        found
            .into_iter()
            .filter(|(p, _, _)| *p == Problem::ReceiptAfterRetirement)
            .map(|(_, gate, seq)| (gate, seq))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        after_retirement(findings_of(&ledger, &witness, NOW + 61).await),
        vec![]
    );
    let (late, hash) = receipts[2].clone();
    ledger
        .store()
        .append(
            &scope(),
            &id("gate-1"),
            2,
            &gov_ledger::store::StoredReceipt {
                seq: 3,
                hash,
                at: NOW + 3,
                received_at: NOW,
                envelope: late,
            },
        )
        .await
        .unwrap();
    checkpoint(NOW + 90).await.unwrap();
    assert_eq!(
        after_retirement(findings_of(&ledger, &witness, NOW + 91).await),
        vec![(Some(id("gate-1")), Some(3))]
    );
}

/// The auditor's check holds an act said to be made under a sign-in to the record of that
/// sign-in (spec §7.6.4, point 4; GR-198).
#[tokio::test]
async fn a_signed_in_act_is_held_to_the_record_of_its_sign_in() {
    use gov_core::admin::{Actor, AdminRecord, Subject, SubjectKind};
    let (a, ledger) = installed().await;
    let (ana, ben) = (id("ana"), id("ben"));
    let account = |oid: &str| gov_authority::Account {
        tid: "t".into(),
        oid: oid.into(),
    };
    a.link_person(&ana, &ana, &account("a"), NOW).await.unwrap();
    a.link_person(&ana, &ben, &account("b"), NOW).await.unwrap();
    let out = a.sign_in(&account("a"), NOW, NOW).await.unwrap();
    a.sign_out(&out.value, NOW + 1).await.unwrap();
    let bens = a.sign_in(&account("b"), NOW, NOW + 1).await.unwrap();
    a.offboard_person(&ana, &ben, NOW + 2).await.unwrap();
    let live = a.sign_in(&account("a"), NOW, NOW + 2).await.unwrap();
    let session = a.session(&live.value, NOW + 3).await.unwrap();
    a.register_gate(&session, &id("gate-7"), NOW + 3)
        .await
        .unwrap();
    let reference = |signed: &gov_authority::SignedIn| signed.reference().clone();
    let acked = ship(&a, &ledger, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &witness)
        .await
        .unwrap();
    assert_eq!(findings_of(&ledger, &witness, NOW + 61).await, vec![]);

    // One more record, by hand, as a faulty or taken-over Authority would write it.
    let batch = a.shipment(0, 100).await.unwrap().unwrap().batch;
    let admin_keys = KeySet::new([a.admin_public()]);
    let last = gov_core::open::<AdminRecord>(batch.last().unwrap(), &admin_keys, &scope()).unwrap();
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    let checkpoints = witness.all().unwrap();
    let reported = |who: &Id, session: &str, at: i64| {
        let record = AdminRecord {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            seq: acked + 1,
            prev: Some(last.hash.clone()),
            at,
            actor: Actor {
                id: who.clone(),
                kind: Default::default(),
                verified: true,
                session: Some(id(session)),
            },
            action: AdminAction::GateRegistered,
            subject: Subject {
                kind: SubjectKind::Gate,
                id: id("gate-8"),
            },
            about: None,
            value: None,
            until: None,
        };
        let mut export = export.clone();
        let chain = export
            .gates
            .iter_mut()
            .find(|g| g.gate == id("authority"))
            .unwrap();
        chain.receipts.push(seal(&record, &admin_key()).unwrap());
        verify(&Check {
            scope: &scope(),
            keys: &keys,
            export: &export,
            checkpoints: &checkpoints,
            interval_secs: 60,
            now: Some(NOW + 61),
            gate_quiet_secs: None,
            clock_skew_secs: 60,
            retention_secs: None,
        })
        .findings
        .iter()
        .any(|f| f.problem == Problem::SessionNotBorneOut && f.seq == Some(acked + 1))
    };
    let (signed_out, offboarded, in_force) = (reference(&out), reference(&bens), reference(&live));
    let until = NOW + 8 * 3600;
    assert!(!reported(&ana, in_force.as_str(), until - 1));
    assert!(reported(&ana, in_force.as_str(), until));
    assert!(reported(&ana, "s-nobody-signed-this-in", NOW + 4));
    assert!(reported(&ben, in_force.as_str(), NOW + 4));
    assert!(reported(&ana, signed_out.as_str(), NOW + 4));
    assert!(reported(&ben, offboarded.as_str(), NOW + 4));

    // A second sign-in under a reference already used does not reopen it (GR-202).
    let mut again = AdminRecord {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        authority: id("authority"),
        seq: acked + 1,
        prev: Some(last.hash.clone()),
        at: NOW + 4,
        actor: Actor {
            id: ana.clone(),
            kind: Default::default(),
            verified: true,
            session: Some(signed_out.clone()),
        },
        action: AdminAction::SignedIn,
        subject: Subject {
            kind: SubjectKind::Person,
            id: ana.clone(),
        },
        about: None,
        value: None,
        until: Some(NOW + 9 * 3600),
    };
    let findings = |record: &AdminRecord| {
        let mut export = export.clone();
        let chain = export
            .gates
            .iter_mut()
            .find(|g| g.gate == id("authority"))
            .unwrap();
        chain.receipts.push(seal(record, &admin_key()).unwrap());
        verify(&Check {
            scope: &scope(),
            keys: &keys,
            export: &export,
            checkpoints: &checkpoints,
            interval_secs: 60,
            now: Some(NOW + 61),
            gate_quiet_secs: None,
            clock_skew_secs: 60,
            retention_secs: None,
        })
        .findings
        .iter()
        .any(|f| f.problem == Problem::SessionNotBorneOut && f.seq == Some(acked + 1))
    };
    assert!(findings(&again));
    again.actor.session = Some(id("s-a-new-one"));
    assert!(!findings(&again));
}

/// The auditor's check holds a proven receipt to a mandate the Authority recorded issuing
/// (spec §7.6.2, point 8): one it did issue passes, one it never issued is reported, and so
/// is one whose record no checkpoint covers yet.
#[tokio::test]
async fn a_proven_receipt_names_a_mandate_the_authority_issued() {
    use gov_core::action::Grant;
    use gov_core::mandate::{Limits, Mandate, Template};
    use gov_core::policy::{OnlineKeys, PolicyBundle};
    let (a, ledger) = installed().await;
    let (ana, ben) = (id("ana"), id("ben"));
    let g1 = key("gate-1:k1", 20);
    enrol_gate(&a, "gate-1", &g1).await;

    // A bundle with one template, published; the agent's key enrolled; a mandate issued.
    let grants = vec![Grant {
        system: id("jira"),
        operations: [id("issue.update")].into(),
        resources: vec![gov_core::ResourcePattern::new("jira/project/pay/*").unwrap()],
    }];
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id("v"),
        number: 1,
        issued_at: NOW,
        keys: OnlineKeys {
            mandates: vec![key("authority:mandates-1", 71).public()],
            leases: vec![key("authority:leases-1", 72).public()],
        },
        freshness: Default::default(),
        rules: Vec::new(),
        connectors: Vec::new(),
        templates: vec![Template {
            id: id("triage"),
            grants: grants.clone(),
            max_life_secs: 3600,
            limits: Limits::default(),
            delegable: false,
            people: BTreeSet::new(),
            receivers: BTreeSet::new(),
        }],
    };
    let signed = seal(&bundle, &key("authority-root-1", 61)).unwrap();
    a.publish_bundle(&ana, &signed, NOW).await.unwrap();
    let bot = key("refund-bot:k1", 9);
    let code = a
        .offer_enrolment(&ana, EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .unwrap();
    let asked = seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind: EnrolKind::Agent,
            name: id("refund-bot"),
            key: bot.public(),
            code: code_digest(&code),
        },
        &bot,
    )
    .unwrap();
    a.enrol(&code, &asked, NOW).await.unwrap();
    let ask = gov_authority::MandateAsk {
        agent: id("refund-bot"),
        key: id("refund-bot:k1"),
        template: id("triage"),
        grants,
        life_secs: 1800,
        limits: Limits::default(),
    };
    let issued = a.issue_mandate(&ben, &ask, NOW).await.unwrap();
    let issuers = KeySet::new([key("authority:mandates-1", 71).public()]);
    let mandate = gov_core::open::<Mandate>(&issued, &issuers, &scope())
        .unwrap()
        .doc
        .id;

    let acked = ship(&a, &ledger, 0).await;
    ledger.supply_key(&g1.public()).await.unwrap();
    // Two receipts that name the issued mandate, and then two that name another.
    let receipts = |mandates: [&Id; 2]| {
        let mut out: Vec<Envelope> = Vec::new();
        let mut prev = None;
        for seq in 1..=4u64 {
            let mut r = common::receipt("gate-1", seq, prev.clone(), NOW + seq as i64);
            r.mandate = vec![mandates[((seq - 1) / 2) as usize].clone()];
            let env = seal(&r, &g1).unwrap();
            prev = Some(common::hash_of(&env, &g1));
            out.push(env);
        }
        out
    };
    // Not a made-up name: one the Authority's chain does hold a record about, as an agent
    // it registered. Only a record of a mandate's issue bears a mandate out.
    let forged = id("refund-bot");
    let chain = receipts([&mandate, &forged]);
    for env in &chain[..2] {
        ledger.ingest(env, NOW).await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &witness)
        .await
        .unwrap();
    // The mandate was issued, and the record of it is witnessed: nothing to report.
    assert_eq!(findings_of(&ledger, &witness, NOW + 61).await, vec![]);

    // A mandate the Authority never issued, in a receipt marked proven: reported, at the
    // first receipt that names it.
    for env in &chain[2..] {
        ledger.ingest(env, NOW).await.unwrap();
    }
    ledger
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 120, &witness)
        .await
        .unwrap();
    assert_eq!(
        findings_of(&ledger, &witness, NOW + 121).await,
        vec![(Problem::MandateNotIssued, Some(id("gate-1")), Some(3))]
    );

    // A record of issue that no checkpoint covers is the Ledger's word alone: with only the
    // checkpoints from before it, the mandate it issued is not yet borne out.
    let early = tempfile::tempdir().unwrap();
    let early_witness = DirWitness::new(early.path());
    let (b, ledger_b) = installed().await;
    enrol_gate(&b, "gate-1", &g1).await;
    let _ = ship(&b, &ledger_b, 0).await;
    ledger_b.supply_key(&g1.public()).await.unwrap();
    ledger_b
        .checkpoint(&ledger_key(), &id("ledger"), NOW + 60, &early_witness)
        .await
        .unwrap();
    b.publish_bundle(&ana, &signed, NOW).await.unwrap();
    let code = b
        .offer_enrolment(&ana, EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .unwrap();
    let asked = seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind: EnrolKind::Agent,
            name: id("refund-bot"),
            key: bot.public(),
            code: code_digest(&code),
        },
        &bot,
    )
    .unwrap();
    b.enrol(&code, &asked, NOW).await.unwrap();
    let issued_b = b.issue_mandate(&ben, &ask, NOW).await.unwrap();
    let mandate_b = gov_core::open::<Mandate>(&issued_b, &issuers, &scope())
        .unwrap()
        .doc
        .id;
    let head = ledger_b
        .store()
        .head(&scope(), &id("authority"))
        .await
        .unwrap()
        .unwrap()
        .seq;
    let _ = ship(&b, &ledger_b, head).await;
    let chain_b = receipts([&mandate_b, &mandate_b]);
    for env in &chain_b[..2] {
        ledger_b.ingest(env, NOW).await.unwrap();
    }
    let unwitnessed = findings_of(&ledger_b, &early_witness, NOW + 61).await;
    assert!(
        unwitnessed.contains(&(Problem::MandateNotIssued, Some(id("gate-1")), Some(1))),
        "{unwitnessed:?}"
    );
    let _ = acked;
}

/// What the auditor's check finds in this Ledger's export against this witness.
async fn findings_of(
    ledger: &TheLedger,
    witness: &DirWitness,
    now: i64,
) -> Vec<(Problem, Option<Id>, Option<u64>)> {
    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key()).await.unwrap();
    verify(&Check {
        scope: &scope(),
        keys: &keys,
        export: &export,
        checkpoints: &witness.all().unwrap(),
        interval_secs: 60,
        now: Some(now),
        gate_quiet_secs: None,
        clock_skew_secs: 60,
        retention_secs: None,
    })
    .findings
    .into_iter()
    .map(|f| (f.problem, f.gate, f.seq))
    .collect()
}
