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
    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path());
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
    assert!(report.ok(), "{:?}", report.findings);
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
    // A receipt dated after its gate was disabled, by more than the clocks may differ:
    // gate-1's second receipt is one second later than the record that disabled it.
    assert_eq!(
        problems(&keys, &checkpoints, 0),
        vec![(Problem::ReceiptAfterRetirement, Some(id("gate-1")))]
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
            verified: false,
        },
        action: AdminAction::GateDisabled,
        subject: Subject {
            kind: SubjectKind::Gate,
            id: id("authority"),
        },
        about: None,
        value: None,
    };
    let envelope = seal(&by_hand, &admin_key()).unwrap();
    ledger.ingest(&envelope, NOW).await.unwrap();
    assert!(!ledger
        .store()
        .key_retired(&scope(), &a.admin_public().kid)
        .await
        .unwrap());
}
