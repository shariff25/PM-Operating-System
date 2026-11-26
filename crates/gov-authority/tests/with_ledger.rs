//! Slice G4a, second part: the Authority's chain goes to the Ledger and is witnessed there,
//! and a gate's key reaches the Ledger only on the Authority's word, backed by the record of
//! its enrolment (spec §7.6).

use std::collections::BTreeSet;

use gov_authority::{Authority, AuthorityError, MemoryStore, Role};
use gov_core::admin::AdminAction;
use gov_core::enrol::{code_digest, EnrolKind, EnrolRequest, KeyGrant};
use gov_core::lease::IntakeAck;
use gov_core::{seal, Digest, Envelope, Id, KeySet, PublicKey, Scope, SigningKey};
use gov_ledger::verify::{verify, Check};
use gov_ledger::{DirWitness, Ledger, LedgerError, Store as LedgerStore, Witness};

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

fn grant(signer: &SigningKey, gate: &str, key: PublicKey) -> Envelope {
    seal(
        &KeyGrant {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            gate: id(gate),
            key,
        },
        signer,
    )
    .unwrap()
}

fn refused(r: Result<(), LedgerError>) {
    assert!(
        matches!(r, Err(LedgerError::Refused("grant_refused"))),
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
    let g1 = key("gate-1-k1", 20);
    enrol_gate(&a, "gate-1", &g1).await;
    let word = a.grant(&id("gate-1-k1")).await.unwrap();

    // The Authority's word alone enrols nothing: the Ledger does not yet hold the record of
    // the enrolment, so the key is not one the witness could have seen.
    refused(ledger.grant_key(&word).await);
    assert!(ledger
        .store()
        .gate_key(&scope(), &id("gate-1-k1"))
        .await
        .unwrap()
        .is_none());

    // The chain is shipped, and the Ledger's signed answer agrees with the Authority's own.
    let acked = ship(&a, &ledger, 0).await;
    assert_eq!(acked, 7);
    // Now the word is backed by a record, and the key is enrolled for the gate's receipts.
    ledger.grant_key(&word).await.unwrap();
    let (gate, held) = ledger
        .store()
        .gate_key(&scope(), &id("gate-1-k1"))
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
    // Asked twice, enrolled once.
    ledger.grant_key(&word).await.unwrap();

    // What does not enrol a key at the Ledger.
    let stranger = key("stranger-k1", 99);
    // A grant signed by a gate's own key, or by a key the Ledger does not know.
    refused(
        ledger
            .grant_key(&grant(&g1, "gate-2", stranger.public()))
            .await,
    );
    refused(
        ledger
            .grant_key(&grant(&stranger, "gate-2", stranger.public()))
            .await,
    );
    // The Authority's own signature on a key that was never enrolled, on an enrolled key for
    // another gate, and on another key under an enrolled key's id.
    refused(
        ledger
            .grant_key(&grant(&admin_key(), "gate-2", stranger.public()))
            .await,
    );
    refused(
        ledger
            .grant_key(&grant(&admin_key(), "gate-2", g1.public()))
            .await,
    );
    let same_id = SigningKey::from_seed(id("gate-1-k1"), &[21; 32]).unwrap();
    refused(
        ledger
            .grant_key(&grant(&admin_key(), "gate-1", same_id.public()))
            .await,
    );
    // An agent's key is not a gate's: the Authority gives no word for it, and a forged word
    // finds no record of a gate's enrolment.
    assert!(matches!(
        a.grant(&id("no-such-key")).await,
        Err(AuthorityError::Unknown(_))
    ));

    // A gate enrolled and then disabled: once the Ledger holds both records, the key is not
    // taken, whoever signs for it.
    let g2 = key("gate-2-k1", 22);
    enrol_gate(&a, "gate-2", &g2).await;
    let word2 = a.grant(&id("gate-2-k1")).await.unwrap();
    a.disable_gate(&id("ana"), &id("gate-2"), NOW)
        .await
        .unwrap();
    assert!(a.grant(&id("gate-2-k1")).await.is_err());
    let acked = ship(&a, &ledger, acked).await;
    assert_eq!(acked, 10);
    refused(ledger.grant_key(&word2).await);

    // The Authority's chain is under the same witness as the gates', and an auditor's check
    // of the export covers it.
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
    // gate-1 has a key and no receipt yet; nothing else is amiss.
    let other: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.gate.as_ref() != Some(&id("gate-1")))
        .collect();
    assert!(other.is_empty(), "{other:?}");
    assert_eq!(report.receipts_verified, 10);
    // What was witnessed says who enrolled which key for which gate.
    let enrolled: Vec<Id> = a
        .store_records()
        .await
        .into_iter()
        .filter(|r| r.action == AdminAction::KeyEnrolled)
        .map(|r| r.subject.id)
        .collect();
    assert_eq!(enrolled, vec![id("gate-1-k1"), id("gate-2-k1")]);
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
