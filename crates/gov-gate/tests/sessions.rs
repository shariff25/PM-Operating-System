//! Sessions with the shim (spec §5.3.1): who may open one, what makes a call proven, and
//! what someone holding a copy of a mandate can and cannot do.

use gov_core::action::{CanonicalAction, Grant, Kind};
use gov_core::mandate::{AgentRef, IssuedBy, IssuerKind, Limits, Mandate, Requires};
use gov_core::receipt::ProofFailure;
use gov_core::request::{RequestProof, SessionOpen};
use gov_core::{
    seal, Digest, Envelope, Id, KeySet, ResourcePath, ResourcePattern, Scope, SigningKey,
};
use gov_gate::sessions::{OpenRefused, Proved, Sessions, SessionsConfig};
use serde_json::json;

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
fn issuer() -> SigningKey {
    key("issuer-1", 1)
}
fn agent() -> SigningKey {
    key("agent-key-1", 7)
}

fn mandate(agent: &SigningKey, expires_at: i64) -> Envelope {
    let m = Mandate {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        id: id("m-1"),
        issuer: id("authority"),
        issued_at: NOW - 60,
        expires_at,
        agent: AgentRef {
            id: id("triage-bot"),
            key: agent.public(),
        },
        issued_by: IssuedBy {
            kind: IssuerKind::Person,
            id: id("ana"),
            session: Some(id("ses-1")),
        },
        template: id("triage@7"),
        grants: vec![Grant {
            system: id("jira"),
            operations: [id("issue.update")].into(),
            resources: vec![ResourcePattern::new("jira/project/pay/*").unwrap()],
        }],
        limits: Limits::default(),
        requires: Requires::default(),
        parent: None,
    };
    seal(&m, &issuer()).unwrap()
}

fn sessions() -> Sessions {
    Sessions::new(SessionsConfig::new(
        scope(),
        id("gate-1"),
        KeySet::new([issuer().public()]),
    ))
}

fn ask(signer: &SigningKey, gate: &str, nonce: &str, at: i64) -> Envelope {
    seal(
        &SessionOpen {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            gate: id(gate),
            mandate: id("m-1"),
            nonce: id(nonce),
            at,
        },
        signer,
    )
    .unwrap()
}

fn action(n: u64, salt: u8) -> Digest {
    CanonicalAction::new(
        id("jira"),
        id("issue.update"),
        Kind::Write,
        ResourcePath::new("jira/project/pay/issue/pay-12").unwrap(),
        None,
        &json!({"n": n}),
        &[salt; 32],
    )
    .unwrap()
    .digest()
}

fn proof(
    signer: &SigningKey,
    session: &Id,
    challenge: &Id,
    counter: u64,
    digest: &Digest,
) -> Envelope {
    seal(
        &RequestProof {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            gate: id("gate-1"),
            session: session.clone(),
            challenge: challenge.clone(),
            counter,
            mandate: id("m-1"),
            action_digest: digest.clone(),
        },
        signer,
    )
    .unwrap()
}

fn why(p: Proved) -> ProofFailure {
    match p {
        Proved::Failed { why, .. } => why,
        Proved::Proven(_) => panic!("proven"),
    }
}

/// A session is opened only by the holder of the key the mandate names (review finding
/// GR-97): a copy of the mandate alone opens nothing.
#[test]
fn only_the_keys_holder_opens_a_session() {
    let mut s = sessions();
    let m = mandate(&agent(), NOW + 3600);
    let stranger = key("stranger", 9);
    for (request, what) in [
        (
            ask(&stranger, "gate-1", "n-1", NOW),
            "signed by another key",
        ),
        (ask(&agent(), "gate-2", "n-2", NOW), "for another gate"),
        (ask(&agent(), "gate-1", "n-3", NOW - 600), "signed long ago"),
        (ask(&agent(), "gate-1", "n-4", NOW + 600), "dated ahead"),
    ] {
        assert_eq!(
            s.open(&m, &request, NOW),
            Err(OpenRefused::Invalid),
            "{what}"
        );
    }
    // A mandate no trusted issuer signed, and one that has expired.
    let forged = seal(
        &gov_core::open::<Mandate>(&m, &KeySet::new([issuer().public()]), &scope())
            .unwrap()
            .doc,
        &stranger,
    )
    .unwrap();
    assert_eq!(
        s.open(&forged, &ask(&agent(), "gate-1", "n-5", NOW), NOW),
        Err(OpenRefused::Invalid)
    );
    assert_eq!(
        s.open(
            &mandate(&agent(), NOW - 1),
            &ask(&agent(), "gate-1", "n-6", NOW),
            NOW
        ),
        Err(OpenRefused::Invalid)
    );
    assert!(s.is_empty());

    // The real one opens; the same signed request played again does not.
    let request = ask(&agent(), "gate-1", "n-7", NOW);
    s.open(&m, &request, NOW).unwrap();
    assert_eq!(s.open(&m, &request, NOW + 1), Err(OpenRefused::Invalid));
    assert_eq!(s.len(), 1);

    // A time at either end of the range is refused like any other; it does not stop the
    // gate (review finding GR-102).
    for (n, at) in [i64::MIN, i64::MAX, i64::MIN + 1].into_iter().enumerate() {
        let extreme = ask(&agent(), "gate-1", &format!("x-{n}"), at);
        assert_eq!(s.open(&m, &extreme, NOW), Err(OpenRefused::Invalid));
    }
}

/// Proven is said only for a proof that holds for the action the gate computed, and each
/// way a proof can fail has its own reason.
#[test]
fn a_call_is_proven_only_by_a_proof_that_holds() {
    let mut s = sessions();
    let m = mandate(&agent(), NOW + 3600);
    let (sid, chal) = s
        .open(&m, &ask(&agent(), "gate-1", "n-1", NOW), NOW)
        .unwrap();
    let d = action(1, 1);

    // The proof holds: the agent is the one the mandate names, never one a header names.
    match s.prove(
        &sid,
        Some(&proof(&agent(), &sid, &chal, 1, &d)),
        Some(&[1; 32]),
        &d,
        true,
        NOW,
    ) {
        Proved::Proven(call) => {
            assert_eq!(call.who().agent, id("triage-bot"));
            assert_eq!(call.who().mandates, vec![id("m-1")]);
        }
        other => panic!("{other:?}"),
    }

    let d2 = action(2, 2);
    let good = |counter| proof(&agent(), &sid, &chal, counter, &d2);
    let stranger = key("stranger", 9);
    // The same proof again; a new proof with the old salt; a proof for another action; one
    // signed by another key; one for another session; none at all; no salt.
    assert_eq!(
        why(s.prove(
            &sid,
            Some(&proof(&agent(), &sid, &chal, 1, &d)),
            Some(&[3; 32]),
            &d,
            true,
            NOW
        )),
        ProofFailure::Replay
    );
    assert_eq!(
        why(s.prove(&sid, Some(&good(2)), Some(&[1; 32]), &d2, true, NOW)),
        ProofFailure::RepeatedSalt
    );
    assert_eq!(
        why(s.prove(
            &sid,
            Some(&good(2)),
            Some(&[2; 32]),
            &action(3, 2),
            true,
            NOW
        )),
        ProofFailure::WrongDigest
    );
    // Arguments with no canonical form: the two sides are not expected to agree, and the
    // receipt says that, not that someone tried something.
    assert_eq!(
        why(s.prove(
            &sid,
            Some(&good(2)),
            Some(&[2; 32]),
            &action(3, 2),
            false,
            NOW
        )),
        ProofFailure::NoCanonicalForm
    );
    assert_eq!(
        why(s.prove(
            &sid,
            Some(&proof(&stranger, &sid, &chal, 2, &d2)),
            Some(&[2; 32]),
            &d2,
            true,
            NOW
        )),
        ProofFailure::BadSignature
    );
    assert_eq!(
        why(s.prove(
            &sid,
            Some(&proof(&agent(), &id("sess-other"), &chal, 2, &d2)),
            Some(&[2; 32]),
            &d2,
            true,
            NOW
        )),
        ProofFailure::WrongSession
    );
    assert_eq!(
        why(s.prove(&sid, None, Some(&[2; 32]), &d2, true, NOW)),
        ProofFailure::Unreadable
    );
    assert_eq!(
        why(s.prove(&sid, Some(&good(2)), None, &d2, true, NOW)),
        ProofFailure::Unreadable
    );
    // None of those failures used up the counter or the salt: the honest call still holds.
    assert!(matches!(
        s.prove(&sid, Some(&good(2)), Some(&[2; 32]), &d2, true, NOW),
        Proved::Proven(_)
    ));

    // A session the gate does not know names nobody.
    assert_eq!(
        s.prove(
            &id("sess-gone"),
            Some(&good(3)),
            Some(&[4; 32]),
            &d2,
            true,
            NOW
        ),
        Proved::Failed {
            who: None,
            why: ProofFailure::UnknownSession
        }
    );
}

/// A session does not outlive its mandate, nor its number of calls.
#[test]
fn a_session_ends_with_its_mandate_and_its_calls() {
    let mut config = SessionsConfig::new(scope(), id("gate-1"), KeySet::new([issuer().public()]));
    config.max_calls = 2;
    let mut s = Sessions::new(config);
    let m = mandate(&agent(), NOW + 100);
    let (sid, chal) = s
        .open(&m, &ask(&agent(), "gate-1", "n-1", NOW), NOW)
        .unwrap();
    for n in 1..=2u64 {
        let d = action(n, n as u8);
        assert!(matches!(
            s.prove(
                &sid,
                Some(&proof(&agent(), &sid, &chal, n, &d)),
                Some(&[n as u8; 32]),
                &d,
                true,
                NOW
            ),
            Proved::Proven(_)
        ));
    }
    let d = action(3, 3);
    let third = proof(&agent(), &sid, &chal, 3, &d);
    assert_eq!(
        why(s.prove(&sid, Some(&third), Some(&[3; 32]), &d, true, NOW)),
        ProofFailure::SessionSpent
    );

    // The mandate is checked on every call, not only at opening (review finding GR-93).
    let mut s = sessions();
    let (sid, chal) = s
        .open(&m, &ask(&agent(), "gate-1", "n-2", NOW), NOW)
        .unwrap();
    let d = action(1, 1);
    let p = proof(&agent(), &sid, &chal, 1, &d);
    assert!(matches!(
        s.prove(&sid, Some(&p), Some(&[1; 32]), &d, true, NOW + 1),
        Proved::Proven(_)
    ));
    let d = action(2, 2);
    let p = proof(&agent(), &sid, &chal, 2, &d);
    let after = s.prove(&sid, Some(&p), Some(&[2; 32]), &d, true, NOW + 100);
    assert!(matches!(
        after,
        Proved::Failed {
            who: None,
            why: ProofFailure::UnknownSession
        } | Proved::Failed {
            why: ProofFailure::MandateExpired,
            ..
        }
    ));
    assert!(s.is_empty());
}

/// Someone holding a copy of the mandate cannot close the real shim's session (review
/// finding GR-88). Sessions that have proven nothing are few, short-lived and counted apart;
/// a proven session is never closed to make room for one that is not.
#[test]
fn unproven_sessions_never_push_out_a_proven_one() {
    let mut s = sessions();
    let m = mandate(&agent(), NOW + 3600);
    let (real, chal) = s
        .open(&m, &ask(&agent(), "gate-1", "n-0", NOW), NOW)
        .unwrap();
    let d = action(1, 1);
    assert!(matches!(
        s.prove(
            &real,
            Some(&proof(&agent(), &real, &chal, 1, &d)),
            Some(&[1; 32]),
            &d,
            true,
            NOW
        ),
        Proved::Proven(_)
    ));

    // The unproven sessions fill, and the next is refused rather than anything closed.
    let mut opened = 0;
    for n in 1..=10 {
        match s.open(&m, &ask(&agent(), "gate-1", &format!("n-{n}"), NOW), NOW) {
            Ok(_) => opened += 1,
            Err(e) => assert_eq!(e, OpenRefused::Busy),
        }
    }
    assert_eq!(opened, 2);
    let d = action(2, 2);
    assert!(matches!(
        s.prove(
            &real,
            Some(&proof(&agent(), &real, &chal, 2, &d)),
            Some(&[2; 32]),
            &d,
            true,
            NOW + 1
        ),
        Proved::Proven(_)
    ));

    // Seconds later the unproven ones are gone, and one can be opened again.
    assert_eq!(s.len(), 3);
    s.open(&m, &ask(&agent(), "gate-1", "n-20", NOW + 31), NOW + 31)
        .unwrap();
    assert_eq!(s.len(), 2);

    // Proven sessions are capped too: a fifth closes the one unused longest, and no other.
    let mut s = sessions();
    let mut held = Vec::new();
    for n in 0..5u64 {
        let at = NOW + n as i64;
        let (sid, chal) = s
            .open(&m, &ask(&agent(), "gate-1", &format!("p-{n}"), at), at)
            .unwrap();
        let d = action(n, n as u8 + 1);
        assert!(matches!(
            s.prove(
                &sid,
                Some(&proof(&agent(), &sid, &chal, 1, &d)),
                Some(&[n as u8 + 1; 32]),
                &d,
                true,
                at
            ),
            Proved::Proven(_)
        ));
        held.push((sid, chal));
    }
    assert_eq!(s.len(), 4);
    let d = action(9, 9);
    let (first, chal) = &held[0];
    assert_eq!(
        why(s.prove(
            first,
            Some(&proof(&agent(), first, chal, 2, &d)),
            Some(&[9; 32]),
            &d,
            true,
            NOW + 6
        )),
        ProofFailure::UnknownSession
    );
    let (second, chal) = &held[1];
    assert!(matches!(
        s.prove(
            second,
            Some(&proof(&agent(), second, chal, 2, &d)),
            Some(&[9; 32]),
            &d,
            true,
            NOW + 6
        ),
        Proved::Proven(_)
    ));
}
