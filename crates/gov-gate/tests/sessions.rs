//! Sessions with the shim (spec §5.3.1): who may open one, what makes a call proven, and
//! what someone holding a copy of a mandate can and cannot do.

use gov_core::action::{CanonicalAction, Grant, Kind};
use gov_core::mandate::{AgentRef, IssuedBy, IssuerKind, Limits, Mandate, Requires};
use gov_core::receipt::ProofFailure;
use gov_core::request::{RequestProof, SessionOpen};
use gov_core::{
    seal, Digest, Envelope, Id, KeySet, ResourcePath, ResourcePattern, Scope, SigningKey,
};
use gov_gate::sessions::{why, OpenRefused, Proved, Sessions, SessionsConfig};
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
    // The asker is told nothing of why; the operator's count says (finding GR-155).
    let count = |s: &Sessions, why: &str| s.refusals().get(why).copied().unwrap_or(0);
    assert_eq!((count(&s, why::REQUEST), count(&s, why::MANDATE)), (4, 2));

    // The real one opens; the same signed request played again does not.
    let request = ask(&agent(), "gate-1", "n-7", NOW);
    s.open(&m, &request, NOW).unwrap();
    assert_eq!(s.open(&m, &request, NOW + 1), Err(OpenRefused::Invalid));
    assert_eq!(s.len(), 1);
    assert_eq!(count(&s, why::REPLAY), 1);
    assert_eq!(s.refusals().values().sum::<u64>(), 7);

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
    assert_eq!(s.refusals().get(why::BUSY), Some(&8));
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

/// A mandate that a trusted key signed and that names no template of the bundle held, or
/// goes wider than the one it names, is refused, and counted apart from every other
/// refusal: signed with the real key, it is what a taken mandates key looks like (review
/// findings GR-138 and GR-155).
#[test]
fn a_mandate_outside_its_template_is_counted_as_that() {
    use gov_core::mandate::Template;
    let template = |resource: &str| Template {
        id: id("triage"),
        grants: vec![Grant {
            system: id("jira"),
            operations: [id("issue.update")].into(),
            resources: vec![ResourcePattern::new(resource).unwrap()],
        }],
        max_life_secs: 3600,
        limits: Limits::default(),
        delegable: false,
        people: Default::default(),
        receivers: Default::default(),
    };
    let with = |templates: Vec<Template>| {
        let mut config =
            SessionsConfig::new(scope(), id("gate-1"), KeySet::new([issuer().public()]));
        config.templates = Some(templates);
        Sessions::new(config)
    };
    let m = mandate(&agent(), NOW + 600);

    // Within its template: opened, and nothing counted.
    let mut s = with(vec![template("jira/project/pay/*")]);
    s.open(&m, &ask(&agent(), "gate-1", "n-1", NOW), NOW)
        .unwrap();
    assert!(s.refusals().is_empty());

    // Wider than its template, and a template the bundle does not hold.
    for held in [vec![template("jira/project/pay/issue/pay-1")], Vec::new()] {
        let mut s = with(held);
        assert_eq!(
            s.open(&m, &ask(&agent(), "gate-1", "n-2", NOW), NOW),
            Err(OpenRefused::Invalid)
        );
        assert_eq!(s.refusals().get(why::TEMPLATE), Some(&1));
        assert_eq!(s.refusals().len(), 1);
        assert!(s.is_empty());
    }
}

// ----- a gate with an Authority: the lease and revocation (spec §7.6.3, points 5 and 6) --

mod with_an_authority {
    use super::*;
    use gov_core::revocation::{Revocable, RevocationSet};
    use gov_gate::sessions::{Call, Trust};

    fn revoked(what: &[(Revocable, &str)]) -> RevocationSet {
        let mut set = RevocationSet::default();
        for (kind, name) in what {
            set.revoke(*kind, id(name));
        }
        set
    }

    /// One call under `sid`, with a proof that holds, numbered `n`.
    fn call(
        s: &mut Sessions,
        sid: &Id,
        chal: &Id,
        n: u64,
        fresh: bool,
        revoked: &RevocationSet,
    ) -> Proved {
        let d = action(n, u8::try_from(n).unwrap());
        let salt = [u8::try_from(n).unwrap(); 32];
        s.prove_with(
            sid,
            &Call {
                proof: Some(&proof(&agent(), sid, chal, n, &d)),
                salt: Some(&salt),
                digest: &d,
                canonical: true,
            },
            NOW,
            Some(&Trust { fresh, revoked }),
        )
    }

    fn who(p: &Proved) -> Option<Id> {
        match p {
            Proved::Failed { who, .. } => who.as_ref().map(|w| w.agent.clone()),
            Proved::Proven(call) => Some(call.who().agent.clone()),
        }
    }

    /// Without a complete, fresh lease the gate cannot say that nothing is revoked, so it
    /// opens no session; and it opens none for a chain any part of which is revoked.
    #[test]
    fn no_session_is_opened_without_a_fresh_lease_or_for_anything_revoked() {
        let m = mandate(&agent(), NOW + 3600);
        let none = RevocationSet::default();
        let open = |s: &mut Sessions, n: &str, fresh: bool, revoked: &RevocationSet| {
            let request = ask(&agent(), "gate-1", n, NOW);
            s.open_with(&m, &request, NOW, Some(&Trust { fresh, revoked }))
        };
        let mut s = sessions();
        assert_eq!(open(&mut s, "n-1", false, &none), Err(OpenRefused::Invalid));
        assert_eq!(s.refusals().get(why::NO_LEASE), Some(&1));
        assert!(s.is_empty());
        // The mandate, its agent, the agent's key, the key the mandate was signed with.
        for (n, what) in [
            (Revocable::Mandate, "m-1"),
            (Revocable::Agent, "triage-bot"),
            (Revocable::Key, "agent-key-1"),
            (Revocable::Key, "issuer-1"),
        ]
        .iter()
        .enumerate()
        {
            let set = revoked(&[*what]);
            assert_eq!(
                open(&mut s, &format!("r-{n}"), true, &set),
                Err(OpenRefused::Invalid),
                "{what:?}"
            );
        }
        assert_eq!(s.refusals().get(why::REVOKED), Some(&4));
        // Something else revoked is no reason to refuse this one.
        let other = revoked(&[(Revocable::Mandate, "m-2"), (Revocable::Agent, "other-bot")]);
        open(&mut s, "n-2", true, &other).unwrap();
        assert_eq!(s.len(), 1);
        // A gate with no Authority asks none of this.
        let mut bare = sessions();
        bare.open(&m, &ask(&agent(), "gate-1", "n-3", NOW), NOW)
            .unwrap();
    }

    /// A proof that holds is recorded as proven only under a fresh lease. Without one it is
    /// a failed proof with a reason of its own, the agent is still named, and the session
    /// is not counted as proven.
    #[test]
    fn a_proof_that_holds_is_proven_only_under_a_fresh_lease() {
        let none = RevocationSet::default();
        let mut s = sessions();
        let m = mandate(&agent(), NOW + 3600);
        let request = ask(&agent(), "gate-1", "n-1", NOW);
        let trust = Trust {
            fresh: true,
            revoked: &none,
        };
        let (sid, chal) = s.open_with(&m, &request, NOW, Some(&trust)).unwrap();

        // The lease has run out by the first call.
        let stale = call(&mut s, &sid, &chal, 1, false, &none);
        assert_eq!(who(&stale), Some(id("triage-bot")));
        assert_eq!(why(stale), ProofFailure::NoLease);
        // A proof that does not hold keeps its own reason, lease or no lease.
        let d = action(2, 2);
        let forged = s.prove_with(
            &sid,
            &Call {
                proof: Some(&proof(&key("agent-key-1", 99), &sid, &chal, 2, &d)),
                salt: Some(&[2; 32]),
                digest: &d,
                canonical: true,
            },
            NOW,
            Some(&Trust {
                fresh: false,
                revoked: &none,
            }),
        );
        assert_eq!(why(forged), ProofFailure::BadSignature);
        // The proof under no lease was used up all the same: it is not played again later.
        let d1 = action(1, 1);
        let replayed = s.prove_with(
            &sid,
            &Call {
                proof: Some(&proof(&agent(), &sid, &chal, 1, &d1)),
                salt: Some(&[9; 32]),
                digest: &d1,
                canonical: true,
            },
            NOW,
            Some(&trust),
        );
        assert_eq!(why(replayed), ProofFailure::Replay);

        // The lease is renewed: the next call is proven.
        assert!(matches!(
            call(&mut s, &sid, &chal, 3, true, &none),
            Proved::Proven(_)
        ));
        // And stale again: failed again. Proven is never remembered from one call to the next.
        assert_eq!(
            why(call(&mut s, &sid, &chal, 4, false, &none)),
            ProofFailure::NoLease
        );
    }

    /// A session whose chain is revoked is closed to proof and kept: a call that still
    /// proves the key is recorded as that agent's, failed, with what was revoked.
    #[test]
    fn a_revoked_session_is_kept_as_a_marker_and_says_what_was_revoked() {
        let none = RevocationSet::default();
        for (what, expected) in [
            (
                vec![(Revocable::Mandate, "m-1")],
                ProofFailure::MandateRevoked,
            ),
            (
                vec![(Revocable::Key, "issuer-1")],
                ProofFailure::MandateRevoked,
            ),
            (
                vec![(Revocable::Agent, "triage-bot")],
                ProofFailure::AgentRevoked,
            ),
            (
                vec![(Revocable::Key, "agent-key-1")],
                ProofFailure::KeyRevoked,
            ),
            // The nearest thing revoked is the one named: the key, then the agent.
            (
                vec![
                    (Revocable::Mandate, "m-1"),
                    (Revocable::Agent, "triage-bot"),
                ],
                ProofFailure::AgentRevoked,
            ),
            (
                vec![
                    (Revocable::Agent, "triage-bot"),
                    (Revocable::Key, "agent-key-1"),
                ],
                ProofFailure::KeyRevoked,
            ),
        ] {
            let mut s = sessions();
            let m = mandate(&agent(), NOW + 3600);
            let (sid, chal) = s
                .open(&m, &ask(&agent(), "gate-1", "n-1", NOW), NOW)
                .unwrap();
            assert!(matches!(
                call(&mut s, &sid, &chal, 1, true, &none),
                Proved::Proven(_)
            ));
            // Something else is revoked: this session is untouched.
            let other = revoked(&[(Revocable::Mandate, "m-2")]);
            assert_eq!(s.revoke(&other, NOW), 0);
            assert!(matches!(
                call(&mut s, &sid, &chal, 2, true, &other),
                Proved::Proven(_)
            ));

            let set = revoked(&what);
            assert_eq!(s.revoke(&set, NOW), 1, "{what:?}");
            // Marked once: telling it again closes nothing more.
            assert_eq!(s.revoke(&set, NOW), 0);
            assert_eq!(s.len(), 1, "the session is kept");
            let after = call(&mut s, &sid, &chal, 3, true, &set);
            assert_eq!(who(&after), Some(id("triage-bot")), "{what:?}");
            assert_eq!(why(after), expected, "{what:?}");
            // A proof that does not hold keeps its own reason.
            let d = action(4, 4);
            let forged = s.prove_with(
                &sid,
                &Call {
                    proof: Some(&proof(&key("agent-key-1", 99), &sid, &chal, 4, &d)),
                    salt: Some(&[4; 32]),
                    digest: &d,
                    canonical: true,
                },
                NOW,
                Some(&Trust {
                    fresh: true,
                    revoked: &set,
                }),
            );
            assert_eq!(why(forged), ProofFailure::BadSignature);
            // Revoked beats a stale lease: the reason that will not go away is the one said.
            assert_eq!(why(call(&mut s, &sid, &chal, 5, false, &set)), expected);
            // No new session is opened for it.
            let again = ask(&agent(), "gate-1", "n-2", NOW);
            let trust = Trust {
                fresh: true,
                revoked: &set,
            };
            assert_eq!(
                s.open_with(&m, &again, NOW, Some(&trust)),
                Err(OpenRefused::Invalid)
            );
            assert_eq!(s.refusals().get(why::REVOKED), Some(&1));
        }
    }

    /// A marker lives only as long as its session would have, and takes no proven
    /// session's place.
    #[test]
    fn a_marker_does_not_outlive_its_session_or_push_one_out() {
        let none = RevocationSet::default();
        let set = revoked(&[(Revocable::Mandate, "m-1")]);
        // Never proven before it was revoked: gone when an unproven session would be.
        let mut s = sessions();
        let m = mandate(&agent(), NOW + 3600);
        let (sid, chal) = s
            .open(&m, &ask(&agent(), "gate-1", "n-1", NOW), NOW)
            .unwrap();
        assert_eq!(s.revoke(&set, NOW), 1);
        assert_eq!(
            why(call(&mut s, &sid, &chal, 1, true, &set)),
            ProofFailure::MandateRevoked
        );
        // Its call did not make it a proven session.
        assert_eq!(s.revoke(&none, NOW + 31), 0);
        assert!(
            s.is_empty(),
            "an unproven marker is swept with the unproven"
        );

        // Proven sessions of the same key are not closed to make room for a marker's calls.
        let mut s = sessions();
        let mut proven = Vec::new();
        for n in 1..=4u64 {
            let request = ask(&agent(), "gate-1", &format!("p-{n}"), NOW);
            let (sid, chal) = s.open(&m, &request, NOW).unwrap();
            assert!(matches!(
                call(&mut s, &sid, &chal, n, true, &none),
                Proved::Proven(_)
            ));
            proven.push((sid, chal));
        }
        assert_eq!(s.len(), 4);
        let request = ask(&agent(), "gate-1", "late", NOW);
        let (late, chal) = s.open(&m, &request, NOW).unwrap();
        assert_eq!(s.revoke(&set, NOW), 5);
        for n in 10..=14u64 {
            assert_eq!(
                why(call(&mut s, &late, &chal, n, true, &set)),
                ProofFailure::MandateRevoked
            );
        }
        assert_eq!(s.len(), 5, "nothing was pushed out");
        // Each of the four still answers as the agent's, revoked.
        for (n, (sid, chal)) in proven.iter().enumerate() {
            let n = 20 + u64::try_from(n).unwrap();
            assert_eq!(
                why(call(&mut s, sid, chal, n, true, &set)),
                ProofFailure::MandateRevoked
            );
        }
    }
}
