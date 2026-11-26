//! The attacks of spec §3.3 that slice G1 answers: T1 to T5, T7, T10, T11, T17, T19, T20.
//! Each test makes the attack and expects the named refusal.

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use gov_core::action::{self, CanonicalAction, Grant, Kind, Reversal};
use gov_core::countersign::{
    Approver, Class, Countersignature, Evidence, NoPasskeys, PasskeyVerifier, Requirement,
};
use gov_core::lease::{
    Checkpoint, Freshness, FreshnessLimits, GateHead, HeldLease, Lease, LeaseRequest,
};
use gov_core::mandate::{
    verify_chain, AgentRef, ChainCheck, IssuedBy, IssuerKind, Limits, Mandate, Requires, MAX_CHAIN,
};
use gov_core::policy::{Effect, Match, Needed, PolicyBundle, Rule, Subject, Verdict};
use gov_core::receipt::{
    follow, ChainHead, Coverage, Decision, DecisionKind, Mode, Outcome, OutcomeStatus, Phase,
    Proof, Reason, Receipt, ReceiptAction, ReceiptAgent,
};
use gov_core::request::{RequestProof, Session};
use gov_core::revocation::{Revocable, RevocationSet};
use gov_core::{
    open, seal, Digest, Envelope, Error, Id, KeySet, ResourcePath, ResourcePattern, Scope,
    SigningKey,
};
use serde_json::{json, Value};

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
fn authority() -> SigningKey {
    key("authority-1", 1)
}
fn authority_keys() -> KeySet {
    KeySet::new([authority().public()])
}
fn grant(ops: &[&str], resources: &[&str]) -> Grant {
    Grant {
        system: id("jira"),
        operations: ops.iter().map(|o| id(o)).collect(),
        resources: resources
            .iter()
            .map(|r| ResourcePattern::new(r).unwrap())
            .collect(),
    }
}
fn action_on(resource: &str, op: &str, kind: Kind, args: Value) -> CanonicalAction {
    CanonicalAction::new(
        id("jira"),
        id(op),
        kind,
        ResourcePath::new(resource).unwrap(),
        Some(id("7")),
        &args,
        &[9; 32],
    )
    .unwrap()
}
fn update() -> CanonicalAction {
    action_on(
        "jira/project/PAY/issue/PAY-12",
        "issue.update",
        Kind::Write,
        json!({"summary": "Refund flow"}),
    )
}

fn root(agent: &SigningKey, agent_id: &str) -> Mandate {
    Mandate {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        id: id("m-root"),
        issuer: id("authority"),
        issued_at: NOW - 60,
        expires_at: NOW + 3600,
        agent: AgentRef {
            id: id(agent_id),
            key: agent.public(),
        },
        issued_by: IssuedBy {
            kind: IssuerKind::Person,
            id: id("ana"),
            session: Some(id("ses-1")),
        },
        template: id("triage@7"),
        grants: vec![grant(
            &["issue.read", "issue.update"],
            &["jira/project/PAY/*"],
        )],
        limits: Limits {
            max_actions: Some(100),
            max_writes: Some(10),
        },
        requires: Requires::default(),
        parent: None,
    }
}

/// A child of `parent`, delegated by the parent's agent, narrowed to one issue.
fn child(parent: &Mandate, parent_env: &Envelope, agent: &SigningKey, n: usize) -> Mandate {
    Mandate {
        id: id(&format!("m-child-{n}")),
        issued_at: parent.issued_at,
        expires_at: parent.expires_at - 60,
        agent: AgentRef {
            id: id(&format!("agent-{n}")),
            key: agent.public(),
        },
        issued_by: IssuedBy {
            kind: IssuerKind::Agent,
            id: parent.agent.id.clone(),
            session: None,
        },
        grants: vec![grant(&["issue.update"], &["jira/project/PAY/issue/*"])],
        limits: Limits {
            max_actions: Some(5),
            max_writes: Some(5),
        },
        parent: Some(Box::new(parent_env.clone())),
        ..parent.clone()
    }
}

fn check<'a>(scope: &'a Scope, keys: &'a KeySet, revoked: &'a RevocationSet) -> ChainCheck<'a> {
    ChainCheck {
        scope,
        authority: keys,
        now: NOW,
        revoked,
    }
}

fn verify(env: &Envelope) -> Result<gov_core::mandate::VerifiedChain, Error> {
    verify_chain(
        env,
        &check(&scope(), &authority_keys(), &RevocationSet::default()),
    )
}

/// A two-link chain: `agent-0` holds the root, `agent-1` the child.
fn chain() -> (Envelope, Mandate, SigningKey) {
    let a0 = key("agent-0-key", 10);
    let a1 = key("agent-1-key", 11);
    let r = root(&a0, "agent-0");
    let r_env = seal(&r, &authority()).unwrap();
    let c = child(&r, &r_env, &a1, 1);
    (seal(&c, &authority()).unwrap(), c, a1)
}

// ----- the envelope ---------------------------------------------------------------------

#[test]
fn any_changed_byte_fails() {
    let agent = key("agent-0-key", 10);
    let env = seal(&root(&agent, "agent-0"), &authority()).unwrap();
    assert!(verify(&env).is_ok());

    let payload = URL_SAFE_NO_PAD.decode(&env.payload).unwrap();
    for i in 0..payload.len() {
        let mut changed = payload.clone();
        changed[i] ^= 0x01;
        let forged = Envelope {
            payload: URL_SAFE_NO_PAD.encode(&changed),
            ..env.clone()
        };
        assert_eq!(
            verify(&forged).unwrap_err(),
            Error::BadSignature,
            "payload byte {i}"
        );
    }
    let sig = URL_SAFE_NO_PAD.decode(&env.sig).unwrap();
    for i in 0..sig.len() {
        let mut changed = sig.clone();
        changed[i] ^= 0x80;
        let forged = Envelope {
            sig: URL_SAFE_NO_PAD.encode(&changed),
            ..env.clone()
        };
        assert_eq!(
            verify(&forged).unwrap_err(),
            Error::BadSignature,
            "signature byte {i}"
        );
    }
}

#[test]
fn an_envelope_is_strict_about_its_own_shape() {
    let agent = key("agent-0-key", 10);
    let env = seal(&root(&agent, "agent-0"), &authority()).unwrap();
    let v2 = Envelope {
        v: 2,
        ..env.clone()
    };
    assert_eq!(verify(&v2).unwrap_err(), Error::UnknownVersion);
    let padded = Envelope {
        sig: format!("{}==", env.sig),
        ..env.clone()
    };
    assert_eq!(verify(&padded).unwrap_err(), Error::BadEnvelope);
    let other_kid = Envelope {
        kid: id("authority-2"),
        ..env.clone()
    };
    assert_eq!(verify(&other_kid).unwrap_err(), Error::UnknownKey);
    let extra: Result<Envelope, _> = serde_json::from_value(json!({
        "v": 1, "kid": "k", "payload": "", "sig": "", "alg": "none"
    }));
    assert!(extra.is_err());
}

#[test]
fn a_document_signed_by_the_wrong_party_fails() {
    // An agent signs itself a mandate with its own key, named like the Authority's.
    let mallory = key("authority-1", 66);
    let env = seal(&root(&mallory, "mallory"), &mallory).unwrap();
    assert_eq!(verify(&env).unwrap_err(), Error::BadSignature);
}

/// T19, and "one kind of document never verifies as another".
#[test]
fn cross_type_cross_tenant_and_cross_environment_documents_fail() {
    let agent = key("agent-0-key", 10);
    let m = root(&agent, "agent-0");
    let env = seal(&m, &authority()).unwrap();

    // The same bytes, presented as a lease: the tag is part of what was signed.
    let as_lease = open::<Lease>(&env, &authority_keys(), &scope());
    assert_eq!(as_lease.unwrap_err(), Error::BadSignature);

    let other_tenant = Scope::new("globex", "prod").unwrap();
    let r = verify_chain(
        &env,
        &check(&other_tenant, &authority_keys(), &RevocationSet::default()),
    );
    assert_eq!(r.unwrap_err(), Error::WrongTenant);

    let test_env = Scope::new("acme", "test").unwrap();
    let r = verify_chain(
        &env,
        &check(&test_env, &authority_keys(), &RevocationSet::default()),
    );
    assert_eq!(r.unwrap_err(), Error::WrongEnv);

    let future = Mandate { v: 2, ..m };
    let env = seal(&future, &authority()).unwrap();
    assert_eq!(verify(&env).unwrap_err(), Error::UnknownVersion);
}

#[test]
fn an_unknown_field_in_a_payload_is_refused() {
    let agent = key("agent-0-key", 10);
    let mut v = serde_json::to_value(root(&agent, "agent-0")).unwrap();
    v["note"] = json!("ignore the grants above");
    assert!(serde_json::from_value::<Mandate>(v).is_err());
}

// ----- the mandate chain (T4, T11) ------------------------------------------------------

#[test]
fn a_narrowed_chain_verifies() {
    let (env, child, _) = chain();
    let verified = verify(&env).unwrap();
    assert_eq!(verified.leaf(), &child);
    assert_eq!(verified.ids(), vec![id("m-child-1"), id("m-root")]);
    assert!(action::carries(&verified.leaf().grants, &update()));
}

/// Rebuild the two-link chain with one change to the child, signed by the Authority as if its
/// own check had failed. The gate must still refuse.
fn widened(change: impl FnOnce(&mut Mandate)) -> Error {
    let a0 = key("agent-0-key", 10);
    let a1 = key("agent-1-key", 11);
    let r = root(&a0, "agent-0");
    let r_env = seal(&r, &authority()).unwrap();
    let mut c = child(&r, &r_env, &a1, 1);
    change(&mut c);
    verify(&seal(&c, &authority()).unwrap()).unwrap_err()
}

#[test]
fn a_child_never_verifies_wider_than_its_parent() {
    assert_eq!(
        widened(|c| c.grants = vec![grant(&["issue.update"], &["jira/project/*"])]),
        Error::WiderThanParent("grants")
    );
    assert_eq!(
        widened(|c| c.grants = vec![grant(&["issue.delete"], &["jira/project/PAY/issue/*"])]),
        Error::WiderThanParent("grants")
    );
    assert_eq!(
        widened(|c| c
            .grants
            .push(grant(&["issue.update"], &["jira/project/HR/*"]))),
        Error::WiderThanParent("grants")
    );
    assert_eq!(
        widened(|c| c.expires_at += 120),
        Error::WiderThanParent("time")
    );
    assert_eq!(
        widened(|c| c.issued_at -= 3600),
        Error::WiderThanParent("time")
    );
    assert_eq!(
        widened(|c| c.limits.max_writes = Some(11)),
        Error::WiderThanParent("limits")
    );
    assert_eq!(
        widened(|c| c.limits.max_actions = None),
        Error::WiderThanParent("limits")
    );
}

#[test]
fn a_child_must_come_from_its_parents_agent() {
    assert_eq!(
        widened(|c| c.issued_by.id = id("agent-9")),
        Error::NotDelegatedByParent
    );
    assert_eq!(
        widened(|c| c.issued_by.kind = IssuerKind::Person),
        Error::NotDelegatedByParent
    );
    // A root that claims an agent asked for it, with no parent to show.
    let a0 = key("agent-0-key", 10);
    let mut r = root(&a0, "agent-0");
    r.issued_by.kind = IssuerKind::Agent;
    assert_eq!(
        verify(&seal(&r, &authority()).unwrap()).unwrap_err(),
        Error::NotDelegatedByParent
    );
}

#[test]
fn a_parent_not_signed_by_the_authority_fails_the_whole_chain() {
    let a0 = key("agent-0-key", 10);
    let a1 = key("agent-1-key", 11);
    // The parent agent writes itself a wide root and signs it with its own key.
    let mut r = root(&a0, "agent-0");
    r.grants = vec![grant(&["issue.update", "issue.delete"], &["jira/*"])];
    let forged_parent = seal(&r, &a0).unwrap();
    let c = child(&r, &forged_parent, &a1, 1);
    let env = seal(&c, &authority()).unwrap();
    assert_eq!(verify(&env).unwrap_err(), Error::UnknownKey);
}

#[test]
fn time_and_revocation_stop_every_link() {
    let (env, _, _) = chain();
    let keys = authority_keys();
    let sc = scope();
    let none = RevocationSet::default();

    let mut late = check(&sc, &keys, &none);
    late.now = NOW + 3600 - 60;
    assert_eq!(verify_chain(&env, &late).unwrap_err(), Error::Expired);
    let mut early = check(&sc, &keys, &none);
    early.now = NOW - 61;
    assert_eq!(verify_chain(&env, &early).unwrap_err(), Error::NotYetValid);

    for (what, name) in [
        (Revocable::Mandate, "m-root"),
        (Revocable::Mandate, "m-child-1"),
        (Revocable::Agent, "agent-0"),
        (Revocable::Agent, "agent-1"),
        (Revocable::Key, "authority-1"),
    ] {
        let mut revoked = RevocationSet::default();
        revoked.revoke(what, id(name));
        let r = verify_chain(&env, &check(&sc, &keys, &revoked));
        assert_eq!(r.unwrap_err(), Error::Revoked, "{name}");
    }
}

#[test]
fn a_chain_has_a_longest_length() {
    let mut keys_used = vec![key("agent-0-key", 10)];
    let mut mandate = root(&keys_used[0], "agent-0");
    let mut env = seal(&mandate, &authority()).unwrap();
    for n in 1..=MAX_CHAIN {
        let k = key(&format!("agent-{n}-key"), 10 + n as u8);
        let mut c = child(&mandate, &env, &k, n);
        // Each link may only shrink; keep the same grants and step the expiry down.
        c.grants = if n == 1 {
            c.grants
        } else {
            mandate.grants.clone()
        };
        c.limits = Limits {
            max_actions: Some(5),
            max_writes: Some(5),
        };
        mandate = c;
        env = seal(&mandate, &authority()).unwrap();
        keys_used.push(k);
        if n < MAX_CHAIN {
            assert_eq!(verify(&env).unwrap().links().len(), n + 1);
        }
    }
    assert_eq!(verify(&env).unwrap_err(), Error::ChainTooDeep);
}

#[test]
fn a_sandbox_condition_is_refused_until_something_can_confirm_it() {
    let a0 = key("agent-0-key", 10);
    let mut r = root(&a0, "agent-0");
    r.requires.sandbox = true;
    assert_eq!(
        verify(&seal(&r, &authority()).unwrap()).unwrap_err(),
        Error::SandboxUnverifiable
    );
}

// ----- scope containment, at random -----------------------------------------------------

/// A small fixed-seed generator: the tests are the same on every run.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn path(&mut self) -> String {
        let names = ["a", "b", "ab", "c"];
        let len = 1 + self.below(4);
        let mut segs = vec!["jira".to_string()];
        for _ in 0..len {
            segs.push(names[self.below(4) as usize].to_string());
        }
        segs.join("/")
    }
    fn pattern(&mut self) -> ResourcePattern {
        let p = self.path();
        let s = if self.below(2) == 0 {
            format!("{p}/*")
        } else {
            p
        };
        ResourcePattern::new(&s).unwrap()
    }
    /// `parent` with operations dropped and patterns narrowed: never wider.
    fn narrowed(&mut self, parent: &[Grant]) -> Vec<Grant> {
        parent
            .iter()
            .map(|g| Grant {
                system: g.system.clone(),
                operations: g.operations.iter().take(1).cloned().collect(),
                resources: g
                    .resources
                    .iter()
                    .map(|r| {
                        let text = String::from(r.clone());
                        let narrower = match (text.strip_suffix("/*"), self.below(3)) {
                            (Some(prefix), 0) => format!("{prefix}/a/*"),
                            (Some(prefix), 1) => format!("{prefix}/b"),
                            _ => text,
                        };
                        ResourcePattern::new(&narrower).unwrap()
                    })
                    .collect(),
            })
            .collect()
    }
    fn grants(&mut self) -> Vec<Grant> {
        let ops = ["read", "update", "delete"];
        (0..1 + self.below(3))
            .map(|_| Grant {
                system: id("jira"),
                operations: (0..1 + self.below(2))
                    .map(|_| id(ops[self.below(3) as usize]))
                    .collect::<BTreeSet<_>>(),
                resources: (0..1 + self.below(2)).map(|_| self.pattern()).collect(),
            })
            .collect()
    }
}

/// If the check says a child is inside its parent, then nothing the child matches escapes
/// the parent. Tried on 20,000 random pairs, with a count of how many were "inside" so the
/// test cannot pass by never saying yes.
#[test]
fn inside_means_nothing_escapes() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut inside = 0;
    for _ in 0..20_000 {
        let (c, p) = (rng.pattern(), rng.pattern());
        if !c.within(&p) {
            continue;
        }
        inside += 1;
        for _ in 0..20 {
            let path = ResourcePath::new(&rng.path()).unwrap();
            assert!(
                !c.covers(&path) || p.covers(&path),
                "{c:?} in {p:?} but {path:?} escapes"
            );
        }
    }
    assert!(inside > 500, "only {inside} pairs were inside");

    let mut inside = 0;
    for round in 0..20_000 {
        // Half the children are random; half are the parent, narrowed, so that "inside" is
        // common enough to test.
        let p = rng.grants();
        let c = if round % 2 == 0 {
            rng.grants()
        } else {
            rng.narrowed(&p)
        };
        if !action::within(&c, &p) {
            assert!(
                round % 2 == 0,
                "a narrowed child was called wider: {c:?} of {p:?}"
            );
            continue;
        }
        inside += 1;
        for _ in 0..20 {
            let op = ["read", "update", "delete"][rng.below(3) as usize];
            let a = action_on(&rng.path(), op, Kind::Write, json!({}));
            assert!(
                !action::carries(&c, &a) || action::carries(&p, &a),
                "{c:?} in {p:?} but {a:?} escapes"
            );
        }
    }
    assert!(inside > 5_000, "only {inside} grant sets were inside");
}

// ----- the request (T1, T2, T3, T5) -----------------------------------------------------

fn proof(counter: u64, digest: &Digest, mandate: &str) -> RequestProof {
    RequestProof {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        gate: id("gate-1"),
        session: id("s-1"),
        challenge: id("c-4f9a"),
        counter,
        mandate: id(mandate),
        action_digest: digest.clone(),
    }
}
fn session() -> Session {
    Session::new(
        id("gate-1"),
        id("s-1"),
        id("c-4f9a"),
        key("agent-1-key", 11).public(),
    )
}

#[test]
fn a_request_is_good_once_and_only_for_its_action() {
    let (_, leaf, agent) = chain();
    let digest = update().digest();
    let sc = scope();
    let mut s = session();

    let first = seal(&proof(1, &digest, "m-child-1"), &agent).unwrap();
    s.accept(&first, &leaf, &digest, &sc).unwrap();
    // T3: the same request again, and an older counter.
    assert_eq!(
        s.accept(&first, &leaf, &digest, &sc).unwrap_err(),
        Error::Replay
    );
    let second = seal(&proof(2, &digest, "m-child-1"), &agent).unwrap();
    s.accept(&second, &leaf, &digest, &sc).unwrap();
    assert_eq!(
        s.accept(&first, &leaf, &digest, &sc).unwrap_err(),
        Error::Replay
    );

    // T5: the arguments changed after the agent signed.
    let other = action_on(
        "jira/project/PAY/issue/PAY-12",
        "issue.update",
        Kind::Write,
        json!({"summary": "Refund flow", "assignee": "mallory"}),
    )
    .digest();
    let third = seal(&proof(3, &digest, "m-child-1"), &agent).unwrap();
    assert_eq!(
        s.accept(&third, &leaf, &other, &sc).unwrap_err(),
        Error::WrongDigest
    );
    // A refused request does not use up its counter.
    s.accept(&third, &leaf, &digest, &sc).unwrap();
}

#[test]
fn a_stolen_mandate_is_useless_without_its_key() {
    let (_, leaf, _) = chain();
    let digest = update().digest();
    // T2: the thief signs with a key of their own under the agent's key id.
    let thief = key("agent-1-key", 99);
    let forged = seal(&proof(1, &digest, "m-child-1"), &thief).unwrap();
    assert_eq!(
        session()
            .accept(&forged, &leaf, &digest, &scope())
            .unwrap_err(),
        Error::BadSignature
    );
    // T1: or under another key id entirely.
    let other = key("agent-7-key", 98);
    let forged = seal(&proof(1, &digest, "m-child-1"), &other).unwrap();
    assert_eq!(
        session()
            .accept(&forged, &leaf, &digest, &scope())
            .unwrap_err(),
        Error::UnknownKey
    );
}

#[test]
fn a_request_belongs_to_one_gate_session_and_mandate() {
    let (_, leaf, agent) = chain();
    let digest = update().digest();
    let sc = scope();
    let good = seal(&proof(1, &digest, "m-child-1"), &agent).unwrap();

    let mut other_gate = Session::new(id("gate-2"), id("s-1"), id("c-4f9a"), agent.public());
    assert_eq!(
        other_gate.accept(&good, &leaf, &digest, &sc).unwrap_err(),
        Error::WrongGate
    );
    let mut other_challenge = Session::new(id("gate-1"), id("s-1"), id("c-0000"), agent.public());
    assert_eq!(
        other_challenge
            .accept(&good, &leaf, &digest, &sc)
            .unwrap_err(),
        Error::WrongSession
    );
    let for_root = seal(&proof(1, &digest, "m-root"), &agent).unwrap();
    assert_eq!(
        session()
            .accept(&for_root, &leaf, &digest, &sc)
            .unwrap_err(),
        Error::WrongMandate
    );
}

// ----- the countersignature (T5, T7, T8) ------------------------------------------------

fn countersignature(class: Class) -> Countersignature {
    Countersignature {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        id: id("cs-1"),
        issuer: id("authority"),
        issued_at: NOW - 5,
        expires_at: NOW + 300,
        gate: id("gate-1"),
        mandate: id("m-child-1"),
        action_digest: update().digest(),
        approver: Approver {
            id: id("bo"),
            groups: vec![id("pay-leads")],
        },
        class,
        evidence: match class {
            Class::Attested => Evidence::Attested {
                by: id("authority"),
                session: id("ses-9"),
            },
            Class::Signed => Evidence::Passkey {
                credential: id("cred-1"),
                assertion: "AAAA".to_string(),
            },
        },
    }
}

struct Want {
    gate: Id,
    mandate: Id,
    digest: Digest,
    group: Id,
    signed_by: Id,
    not_approver: Option<Id>,
}
impl Want {
    fn new() -> Self {
        Want {
            gate: id("gate-1"),
            mandate: id("m-child-1"),
            digest: update().digest(),
            group: id("pay-leads"),
            signed_by: id("authority-1"),
            not_approver: None,
        }
    }
    fn of(&self, class: Class, now: i64) -> Requirement<'_> {
        Requirement {
            gate: &self.gate,
            mandate: &self.mandate,
            action_digest: &self.digest,
            class,
            group: &self.group,
            not_approver: self.not_approver.as_ref(),
            signed_by: &self.signed_by,
            now,
        }
    }
}

/// Accepts one known assertion for one challenge: a stand-in for the passkey check of G6.
struct OnePasskey(Digest);
impl PasskeyVerifier for OnePasskey {
    fn verify(&self, approver: &Id, credential: &Id, assertion: &str, challenge: &Digest) -> bool {
        approver.as_str() == "bo"
            && credential.as_str() == "cred-1"
            && assertion == "AAAA"
            && challenge == &self.0
    }
}

#[test]
fn an_approval_fits_one_action_one_gate_one_mandate_for_a_while() {
    let none = RevocationSet::default();
    let cs = countersignature(Class::Attested);
    let w = Want::new();
    cs.satisfies(&w.of(Class::Attested, NOW), &NoPasskeys, &none)
        .unwrap();

    let mut other = Want::new();
    other.digest = action_on(
        "jira/project/PAY/issue/PAY-12",
        "issue.update",
        Kind::Write,
        json!({"summary": "Refund flow!"}),
    )
    .digest();
    assert_eq!(
        cs.satisfies(&other.of(Class::Attested, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::WrongDigest
    );
    let mut other = Want::new();
    other.gate = id("gate-2");
    assert_eq!(
        cs.satisfies(&other.of(Class::Attested, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::WrongGate
    );
    let mut other = Want::new();
    other.mandate = id("m-root");
    assert_eq!(
        cs.satisfies(&other.of(Class::Attested, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::WrongMandate
    );
    let mut other = Want::new();
    other.group = id("admins");
    assert_eq!(
        cs.satisfies(&other.of(Class::Attested, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::WrongGroup
    );
    assert_eq!(
        cs.satisfies(&w.of(Class::Attested, NOW + 300), &NoPasskeys, &none)
            .unwrap_err(),
        Error::Expired
    );
    let mut revoked = RevocationSet::default();
    revoked.revoke(Revocable::Countersignature, id("cs-1"));
    assert_eq!(
        cs.satisfies(&w.of(Class::Attested, NOW), &NoPasskeys, &revoked)
            .unwrap_err(),
        Error::Revoked
    );
}

/// T8: where policy asks for the approver's own signature, the Authority's word is not enough.
#[test]
fn an_attested_approval_never_passes_for_a_signed_one() {
    let none = RevocationSet::default();
    let w = Want::new();
    let attested = countersignature(Class::Attested);
    assert_eq!(
        attested
            .satisfies(&w.of(Class::Signed, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::WrongClass
    );
    // Calling it "signed" without a passkey's evidence does not help.
    let mut relabelled = attested.clone();
    relabelled.class = Class::Signed;
    assert!(matches!(
        relabelled
            .satisfies(&w.of(Class::Signed, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::Malformed(_)
    ));
    // Nor does passkey evidence nothing has verified.
    let signed = countersignature(Class::Signed);
    assert_eq!(
        signed
            .satisfies(&w.of(Class::Signed, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::PasskeyUnverified
    );
    // With a verifier that knows the assertion for this exact challenge, it passes; and the
    // challenge moves with the action.
    let verifier = OnePasskey(signed.challenge());
    signed
        .satisfies(&w.of(Class::Signed, NOW), &verifier, &none)
        .unwrap();
    signed
        .satisfies(&w.of(Class::Attested, NOW), &verifier, &none)
        .unwrap();
    let mut moved = signed.clone();
    moved.action_digest = args_only_digest();
    assert_ne!(moved.challenge(), signed.challenge());
}

fn args_only_digest() -> Digest {
    action_on(
        "jira/project/PAY/issue/PAY-13",
        "issue.update",
        Kind::Write,
        json!({}),
    )
    .digest()
}

// ----- receipts and their chain (T10, T17) ----------------------------------------------

fn receipt(seq: u64, prev: Option<Digest>, phase: Phase) -> Receipt {
    Receipt {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        gate: id("gate-1"),
        seq,
        prev,
        at: NOW,
        phase,
        mode: Mode::Enforce,
        coverage: Coverage::Custody,
        agent: ReceiptAgent {
            id: id("agent-1"),
            proof: Proof::Proven,
        },
        mandate: vec![id("m-child-1"), id("m-root")],
        policy_version: Some(id("7")),
        action: ReceiptAction::from(&update()),
        reversal: Reversal::Restore,
        before_digest: None,
        reverses: None,
        decision: Decision {
            kind: DecisionKind::Allow,
            reason: Reason::Ok,
        },
        would_decide: None,
        countersign: None,
        outcome: (phase == Phase::Outcome).then(|| Outcome {
            status: OutcomeStatus::Ok,
            upstream_ref: Some(id("PAY-12")),
            response_digest: None,
        }),
        intent: (phase == Phase::Outcome).then(|| seq - 1),
    }
}

#[test]
fn a_chain_shows_a_removed_reordered_or_altered_receipt() {
    let gate = key("gate-1-key", 20);
    let keys = KeySet::new([gate.public()]);
    let sc = scope();
    let mut head: Option<ChainHead> = None;
    let mut sealed = Vec::new();
    for seq in 1..=4u64 {
        let phase = if seq % 2 == 1 {
            Phase::Intent
        } else {
            Phase::Outcome
        };
        let r = receipt(seq, head.as_ref().map(|h| h.hash.clone()), phase);
        let env = seal(&r, &gate).unwrap();
        let opened = open::<Receipt>(&env, &keys, &sc).unwrap();
        head = Some(follow(head.as_ref(), &opened).unwrap());
        sealed.push(env);
    }
    let opened: Vec<_> = sealed
        .iter()
        .map(|e| open::<Receipt>(e, &keys, &sc).unwrap())
        .collect();
    let h1 = follow(None, &opened[0]).unwrap();
    // Removed: 3 does not follow 1.
    assert_eq!(
        follow(Some(&h1), &opened[2]).unwrap_err(),
        Error::BrokenChain
    );
    // A number skipped, with the right link: the count is part of the chain.
    let skipped = receipt(3, Some(h1.hash.clone()), Phase::Outcome);
    let skipped = open::<Receipt>(&seal(&skipped, &gate).unwrap(), &keys, &sc).unwrap();
    assert_eq!(follow(Some(&h1), &skipped).unwrap_err(), Error::BrokenChain);
    // Repeated.
    assert_eq!(
        follow(Some(&h1), &opened[0]).unwrap_err(),
        Error::BrokenChain
    );
    // A chain cannot start in the middle.
    assert_eq!(follow(None, &opened[1]).unwrap_err(), Error::BrokenChain);
    // Replaced: a different receipt 2, validly signed by the gate, does not link to 3.
    let mut other = receipt(2, Some(h1.hash.clone()), Phase::Outcome);
    other.outcome.as_mut().unwrap().status = OutcomeStatus::Failed;
    let other = open::<Receipt>(&seal(&other, &gate).unwrap(), &keys, &sc).unwrap();
    let forked = follow(Some(&h1), &other).unwrap();
    assert_eq!(
        follow(Some(&forked), &opened[2]).unwrap_err(),
        Error::BrokenChain
    );
    // Signed by anything but the gate's enrolled key.
    let stranger = key("gate-1-key", 21);
    let forged = seal(&receipt(1, None, Phase::Intent), &stranger).unwrap();
    assert_eq!(
        open::<Receipt>(&forged, &keys, &sc).unwrap_err(),
        Error::BadSignature
    );
}

#[test]
fn a_receipt_keeps_its_own_rules() {
    let mut r = receipt(1, None, Phase::Intent);
    r.validate().unwrap();
    r.action.args_digest = Digest::of(&[b"other arguments"]);
    assert_eq!(r.validate().unwrap_err(), Error::WrongDigest);

    let mut r = receipt(1, None, Phase::Intent);
    r.outcome = Some(Outcome {
        status: OutcomeStatus::Ok,
        upstream_ref: None,
        response_digest: None,
    });
    assert!(r.validate().is_err());
    let mut r = receipt(1, None, Phase::Intent);
    r.would_decide = Some(r.decision);
    assert!(r.validate().is_err(), "would_decide outside shadow mode");
    let mut r = receipt(1, None, Phase::Intent);
    r.agent.proof = Proof::Claimed;
    assert!(r.validate().is_err(), "a claimed identity in enforce mode");
    let mut r = receipt(1, None, Phase::Intent);
    r.decision.kind = DecisionKind::Refuse;
    assert!(r.validate().is_err(), "a refused action with an intent");
    assert!(receipt(2, None, Phase::Intent).validate().is_err());
    assert!(!Mode::Observe.enforced() && !Mode::Shadow.enforced() && Mode::Enforce.enforced());
}

/// T17: there is nowhere in a receipt to put a sentence.
#[test]
fn a_receipt_cannot_carry_free_text() {
    fn strings(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            Value::Object(o) => o.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }
    let mut full = receipt(2, Some(Digest::of(&[b"x"])), Phase::Outcome);
    full.countersign = Some(gov_core::receipt::CountersignRef {
        id: id("cs-1"),
        class: Class::Signed,
    });
    full.outcome.as_mut().unwrap().response_digest = Some(Digest::of(&[b"body"]));
    let value = serde_json::to_value(&full).unwrap();

    // Every string field refuses a sentence: put one in each string in turn.
    let mut paths = Vec::new();
    fn walk(v: &Value, at: String, out: &mut Vec<String>) {
        match v {
            Value::String(_) => out.push(at),
            Value::Array(a) => a
                .iter()
                .enumerate()
                .for_each(|(i, x)| walk(x, format!("{at}/{i}"), out)),
            Value::Object(o) => o
                .iter()
                .for_each(|(k, x)| walk(x, format!("{at}/{k}"), out)),
            _ => {}
        }
    }
    walk(&value, String::new(), &mut paths);
    assert!(paths.len() >= 20, "{paths:?}");
    for path in &paths {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = json!("Ana asked to refund customer 4411");
        assert!(
            serde_json::from_value::<Receipt>(changed).is_err(),
            "{path} took free text"
        );
    }
    let mut extra = value.clone();
    extra["note"] = json!("x");
    assert!(serde_json::from_value::<Receipt>(extra).is_err());

    let mut all = Vec::new();
    strings(&value, &mut all);
    assert!(all.iter().all(|s| !s.contains(char::is_whitespace)));
}

// ----- the lease (T11, T20) -------------------------------------------------------------

fn lease() -> Lease {
    Lease {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        gate: id("gate-1"),
        nonce: id("n-1"),
        authority_time: NOW,
        policy_version: id("7"),
        revocation_seq: 41,
        valid_for_secs: 300,
    }
}

fn asked(mono_ms: u64) -> LeaseRequest {
    LeaseRequest {
        gate: id("gate-1"),
        nonce: id("n-1"),
        sent_mono_ms: mono_ms,
    }
}
/// A first lease, asked for at `mono_ms`.
fn held_at(lease: Lease, mono_ms: u64) -> HeldLease {
    HeldLease::accept(None, lease, asked(mono_ms)).unwrap()
}

/// Review finding GR-1: a lease is the answer to one request, and never moves a gate back.
#[test]
fn a_replayed_or_older_lease_is_refused() {
    let first = held_at(lease(), 5_000);
    // A newer lease arrives in answer to the gate's next request.
    let mut newer = lease();
    newer.nonce = id("n-2");
    newer.authority_time = NOW + 10_000;
    newer.revocation_seq = 57;
    let next = LeaseRequest {
        gate: id("gate-1"),
        nonce: id("n-2"),
        sent_mono_ms: 10_005_000,
    };
    let held = HeldLease::accept(Some(&first), newer.clone(), next).unwrap();
    assert_eq!(held.now(10_005_000).unwrap(), NOW + 10_000);

    // The old lease, kept by someone and played back: it answers no request the gate made.
    let third = || LeaseRequest {
        gate: id("gate-1"),
        nonce: id("n-3"),
        sent_mono_ms: 10_006_000,
    };
    assert_eq!(
        HeldLease::accept(Some(&held), lease(), third()).unwrap_err(),
        Error::LeaseReplayed
    );
    // Even with the right nonce (a hostile network replaying into a guessed request), a lease
    // that would turn the clock or the revocations back is refused.
    let mut back_in_time = lease();
    back_in_time.nonce = id("n-3");
    assert_eq!(
        HeldLease::accept(Some(&held), back_in_time, third()).unwrap_err(),
        Error::LeaseReplayed
    );
    let mut fewer_revocations = newer.clone();
    fewer_revocations.nonce = id("n-3");
    fewer_revocations.authority_time = NOW + 10_001;
    fewer_revocations.revocation_seq = 41;
    assert_eq!(
        HeldLease::accept(Some(&held), fewer_revocations, third()).unwrap_err(),
        Error::LeaseReplayed
    );
    // Each check alone: a lease that is newer in every way but answers another request...
    let mut wrong_nonce = newer.clone();
    wrong_nonce.nonce = id("n-9");
    wrong_nonce.authority_time = NOW + 20_000;
    wrong_nonce.revocation_seq = 60;
    assert_eq!(
        HeldLease::accept(Some(&held), wrong_nonce, third()).unwrap_err(),
        Error::LeaseReplayed
    );
    // ...and one that answers this request, with every revocation, but an older clock.
    let mut older_clock = newer.clone();
    older_clock.nonce = id("n-3");
    older_clock.authority_time = NOW + 9_999;
    older_clock.revocation_seq = 60;
    assert_eq!(
        HeldLease::accept(Some(&held), older_clock, third()).unwrap_err(),
        Error::LeaseReplayed
    );
    // Another gate's lease.
    let mut other_gate = newer.clone();
    other_gate.nonce = id("n-3");
    other_gate.gate = id("gate-2");
    assert_eq!(
        HeldLease::accept(Some(&held), other_gate, third()).unwrap_err(),
        Error::LeaseReplayed
    );
    // A lease held up on the way is as old as its request, not as young as its arrival: asked
    // for at 10_006_000, it is already past the strict class by 10_016_001 however late it came.
    let mut delayed = newer;
    delayed.nonce = id("n-3");
    let held = HeldLease::accept(Some(&held), delayed, third()).unwrap();
    let limits = FreshnessLimits::default();
    assert_eq!(
        held.permits(10_016_001, Freshness::Strict, &limits, &id("7"), 57)
            .unwrap_err(),
        Error::LeaseStale
    );
}

#[test]
fn a_stale_or_incomplete_lease_refuses() {
    let limits = FreshnessLimits::default();
    let held = held_at(lease(), 5_000);
    let v7 = id("7");

    held.permits(15_000, Freshness::Strict, &limits, &v7, 41)
        .unwrap();
    assert_eq!(
        held.permits(15_001, Freshness::Strict, &limits, &v7, 41)
            .unwrap_err(),
        Error::LeaseStale
    );
    // A read is not exempt: it lasts longer, and then it stops too.
    held.permits(125_000, Freshness::Standard, &limits, &v7, 41)
        .unwrap();
    assert_eq!(
        held.permits(125_001, Freshness::Standard, &limits, &v7, 41)
            .unwrap_err(),
        Error::LeaseStale
    );
    // The lease's own life caps every class.
    let mut short = lease();
    short.valid_for_secs = 3;
    let short = held_at(short, 0);
    assert_eq!(
        short
            .permits(3_001, Freshness::Standard, &limits, &v7, 41)
            .unwrap_err(),
        Error::LeaseStale
    );
    // T11: a gate that missed a revocation, or holds another bundle, may not decide at all.
    assert_eq!(
        held.permits(6_000, Freshness::Standard, &limits, &v7, 40)
            .unwrap_err(),
        Error::LeaseIncomplete
    );
    assert_eq!(
        held.permits(6_000, Freshness::Standard, &limits, &id("6"), 41)
            .unwrap_err(),
        Error::LeaseIncomplete
    );
    // A monotonic clock that ran backwards.
    assert_eq!(
        held.permits(4_999, Freshness::Standard, &limits, &v7, 41)
            .unwrap_err(),
        Error::LeaseStale
    );
}

/// T20: "now" is the Authority's time plus elapsed monotonic time, so a mandate expires on the
/// Authority's clock whatever the gate's wall clock says.
#[test]
fn expiry_runs_on_the_authoritys_clock() {
    let held = held_at(lease(), 1_000_000);
    assert_eq!(held.now(1_000_000).unwrap(), NOW);
    assert_eq!(held.now(1_059_999).unwrap(), NOW + 59);
    let (env, _, _) = chain();
    let keys = authority_keys();
    let sc = scope();
    let none = RevocationSet::default();
    let mut c = check(&sc, &keys, &none);
    // The child expires at NOW + 3540.
    c.now = held.now(1_000_000 + 3_539_000).unwrap();
    assert!(verify_chain(&env, &c).is_ok());
    c.now = held.now(1_000_000 + 3_540_000).unwrap();
    assert_eq!(verify_chain(&env, &c).unwrap_err(), Error::Expired);
}

#[test]
fn a_checkpoint_lists_each_gate_once_in_order() {
    let head = |g: &str| GateHead {
        gate: id(g),
        seq: 3,
        hash: Digest::of(&[g.as_bytes()]),
    };
    let mut c = Checkpoint {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("ledger"),
        seq: 1,
        at: NOW,
        heads: vec![head("gate-1"), head("gate-2")],
    };
    c.validate().unwrap();
    c.heads.push(head("gate-2"));
    assert!(c.validate().is_err());
    c.heads = vec![head("gate-2"), head("gate-1")];
    assert!(c.validate().is_err());
}

// ----- policy ---------------------------------------------------------------------------

fn rule(name: &str, matches: Match, then: Effect) -> Rule {
    Rule {
        id: id(name),
        matches,
        then,
        freshness: None,
    }
}
fn jira(ops: &[&str], resources: &[&str]) -> Match {
    Match {
        system: Some(id("jira")),
        operations: (!ops.is_empty()).then(|| ops.iter().map(|o| id(o)).collect()),
        resources: (!resources.is_empty()).then(|| {
            resources
                .iter()
                .map(|r| ResourcePattern::new(r).unwrap())
                .collect()
        }),
        ..Match::default()
    }
}
fn bundle(rules: Vec<Rule>) -> PolicyBundle {
    PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id("7"),
        issued_at: NOW,
        freshness: FreshnessLimits::default(),
        rules,
    }
}

#[test]
fn refusals_win_and_no_match_refuses() {
    let labels = BTreeSet::from([id("triage")]);
    let agent = id("agent-1");
    let subject = Subject {
        agent: &agent,
        labels: &labels,
    };
    let lead = Effect::Countersign {
        class: Class::Attested,
        group: id("pay-leads"),
        issuer_may_approve: false,
    };
    let sec = Effect::Countersign {
        class: Class::Signed,
        group: id("security"),
        issuer_may_approve: true,
    };

    let policy = bundle(vec![
        rule("read", jira(&["issue.read"], &[]), Effect::Allow),
        rule(
            "update-pay",
            jira(&["issue.update"], &["jira/project/PAY/*"]),
            Effect::Allow,
        ),
        rule(
            "frozen",
            jira(&[], &["jira/project/PAY/issue/PAY-99"]),
            Effect::Refuse,
        ),
        rule(
            "lead-signs",
            jira(&["issue.update"], &["jira/project/PAY/issue/PAY-50"]),
            lead,
        ),
        rule(
            "security-signs",
            Match {
                kinds: Some(BTreeSet::from([Kind::Destructive])),
                ..jira(&[], &[])
            },
            sec,
        ),
        rule(
            "not-this-agent",
            Match {
                agent: Some(id("agent-2")),
                ..Match::default()
            },
            Effect::Refuse,
        ),
    ]);
    policy.validate().unwrap();

    let upd = |issue: &str| {
        action_on(
            &format!("jira/project/PAY/issue/{issue}"),
            "issue.update",
            Kind::Write,
            json!({}),
        )
    };
    assert_eq!(
        policy
            .evaluate(&subject, &upd("PAY-12"), Reversal::Restore)
            .verdict,
        Verdict::Allow
    );
    assert_eq!(
        policy
            .evaluate(&subject, &upd("PAY-99"), Reversal::Restore)
            .verdict,
        Verdict::Refuse
    );
    assert_eq!(
        policy
            .evaluate(&subject, &upd("PAY-50"), Reversal::Restore)
            .verdict,
        Verdict::Countersign(vec![Needed {
            class: Class::Attested,
            group: id("pay-leads"),
            issuer_may_approve: false,
        }])
    );
    // Another project: nothing matches, which refuses.
    let hr = action_on(
        "jira/project/HR/issue/HR-1",
        "issue.update",
        Kind::Write,
        json!({}),
    );
    assert_eq!(
        policy.evaluate(&subject, &hr, Reversal::Restore).verdict,
        Verdict::NoMatch
    );
    // A destructive action: a signature is asked for, and the lease must be strict.
    let del = action_on(
        "jira/project/PAY/issue/PAY-12",
        "issue.delete",
        Kind::Destructive,
        json!({}),
    );
    let e = policy.evaluate(&subject, &del, Reversal::Restore);
    assert_eq!(
        e.verdict,
        Verdict::Countersign(vec![Needed {
            class: Class::Signed,
            group: id("security"),
            issuer_may_approve: true,
        }])
    );
    assert_eq!(e.freshness, Freshness::Strict);
    assert_eq!(
        policy
            .evaluate(&subject, &upd("PAY-12"), Reversal::Restore)
            .freshness,
        Freshness::Standard
    );
    // The same action as another agent.
    let other = id("agent-2");
    let subject2 = Subject {
        agent: &other,
        labels: &labels,
    };
    assert_eq!(
        policy
            .evaluate(&subject2, &upd("PAY-12"), Reversal::Restore)
            .verdict,
        Verdict::Refuse
    );
    // A countersignature rule alone permits; without it or an allow, nothing does.
    let only_refusals = bundle(vec![rule("r", Match::default(), Effect::Refuse)]);
    assert_eq!(
        only_refusals
            .evaluate(&subject, &hr, Reversal::Restore)
            .verdict,
        Verdict::Refuse
    );
    assert_eq!(
        bundle(vec![])
            .evaluate(&subject, &hr, Reversal::Restore)
            .verdict,
        Verdict::NoMatch
    );
}

#[test]
fn a_rule_may_raise_freshness_and_the_strictest_wins() {
    let labels = BTreeSet::new();
    let agent = id("agent-1");
    let subject = Subject {
        agent: &agent,
        labels: &labels,
    };
    let mut strict = rule(
        "strict-pay",
        jira(&[], &["jira/project/PAY/*"]),
        Effect::Allow,
    );
    strict.freshness = Some(Freshness::Strict);
    let mut relaxed = rule("relaxed", jira(&["issue.update"], &[]), Effect::Allow);
    relaxed.freshness = Some(Freshness::Standard);
    let policy = bundle(vec![relaxed, strict]);
    let e = policy.evaluate(&subject, &update(), Reversal::Restore);
    assert_eq!(e.freshness, Freshness::Strict);
    assert_eq!(e.matched, vec![id("relaxed"), id("strict-pay")]);
}

#[test]
fn the_lint_refuses_a_rule_that_permits_everything() {
    assert!(bundle(vec![rule("all", Match::default(), Effect::Allow)])
        .validate()
        .is_err());
    let dup = bundle(vec![
        rule("a", jira(&[], &[]), Effect::Allow),
        rule("a", jira(&[], &[]), Effect::Refuse),
    ]);
    assert!(dup.validate().is_err());
    let empty = Match {
        operations: Some(BTreeSet::new()),
        ..jira(&[], &[])
    };
    assert!(bundle(vec![rule("e", empty, Effect::Allow)])
        .validate()
        .is_err());
    let mut bad = bundle(vec![]);
    bad.freshness = FreshnessLimits {
        strict_secs: 200,
        standard_secs: 120,
    };
    assert!(bad.validate().is_err());
    // A signed bundle round-trips, and an unknown section is refused.
    let policy = bundle(vec![rule(
        "read",
        jira(&["issue.read"], &[]),
        Effect::Allow,
    )]);
    let env = seal(&policy, &authority()).unwrap();
    assert_eq!(
        open::<PolicyBundle>(&env, &authority_keys(), &scope())
            .unwrap()
            .doc,
        policy
    );
    let mut v = serde_json::to_value(&policy).unwrap();
    v["overrides"] = json!([]);
    assert!(serde_json::from_value::<PolicyBundle>(v).is_err());
}

// ----- the reviewer's findings of 2026-11-26 --------------------------------------------

/// GR-5: a stolen agent key, revoked, does not keep its old mandates alive; nor does a
/// countersignature signed by a revoked Authority key pass.
#[test]
fn a_revoked_key_stops_what_it_signed_or_holds() {
    let (env, _, _) = chain();
    let keys = authority_keys();
    let sc = scope();
    for kid in ["agent-1-key", "agent-0-key"] {
        let mut revoked = RevocationSet::default();
        revoked.revoke(Revocable::Key, id(kid));
        let r = verify_chain(&env, &check(&sc, &keys, &revoked));
        assert_eq!(r.unwrap_err(), Error::Revoked, "{kid}");
    }
    let mut revoked = RevocationSet::default();
    revoked.revoke(Revocable::Key, id("authority-1"));
    let cs = countersignature(Class::Attested);
    let w = Want::new();
    assert_eq!(
        cs.satisfies(&w.of(Class::Attested, NOW), &NoPasskeys, &revoked)
            .unwrap_err(),
        Error::Revoked
    );
}

/// GR-7: "refuse X/*" refuses X itself; a grant of X/* still does not reach X.
#[test]
fn a_refusal_of_everything_under_a_thing_refuses_the_thing() {
    let labels = BTreeSet::new();
    let agent = id("agent-1");
    let subject = Subject {
        agent: &agent,
        labels: &labels,
    };
    let policy = bundle(vec![
        rule("all-jira", jira(&[], &[]), Effect::Allow),
        rule(
            "secret",
            jira(&[], &["jira/project/SECRET/*"]),
            Effect::Refuse,
        ),
    ]);
    let delete_project = action_on(
        "jira/project/SECRET",
        "project.delete",
        Kind::Destructive,
        json!({}),
    );
    assert_eq!(
        policy
            .evaluate(&subject, &delete_project, Reversal::Restore)
            .verdict,
        Verdict::Refuse
    );
    let inside = action_on(
        "jira/project/SECRET/issue/S-1",
        "issue.read",
        Kind::Read,
        json!({}),
    );
    assert_eq!(
        policy
            .evaluate(&subject, &inside, Reversal::Restore)
            .verdict,
        Verdict::Refuse
    );
    let neighbour = action_on(
        "jira/project/SECRETS",
        "project.read",
        Kind::Read,
        json!({}),
    );
    assert_eq!(
        policy
            .evaluate(&subject, &neighbour, Reversal::Restore)
            .verdict,
        Verdict::Allow
    );
    // The same pattern as an allow, or in a grant, does not reach the prefix.
    let only_under = bundle(vec![rule(
        "under",
        jira(&[], &["jira/project/PAY/*"]),
        Effect::Allow,
    )]);
    let the_project = action_on(
        "jira/project/PAY",
        "project.delete",
        Kind::Destructive,
        json!({}),
    );
    assert_eq!(
        only_under
            .evaluate(&subject, &the_project, Reversal::Restore)
            .verdict,
        Verdict::NoMatch
    );
    assert!(!action::carries(
        &[grant(&["project.delete"], &["jira/project/PAY/*"])],
        &the_project
    ));
}

/// GR-8: the gate itself refuses the mandate's issuer as approver, unless the rule allows it;
/// and the challenge a passkey signs covers the class and the issue time.
#[test]
fn the_issuer_does_not_approve_their_own_agents_action() {
    let none = RevocationSet::default();
    let cs = countersignature(Class::Attested);
    let mut w = Want::new();
    w.not_approver = Some(id("bo"));
    assert_eq!(
        cs.satisfies(&w.of(Class::Attested, NOW), &NoPasskeys, &none)
            .unwrap_err(),
        Error::ApproverIsIssuer
    );
    w.not_approver = Some(id("ana"));
    cs.satisfies(&w.of(Class::Attested, NOW), &NoPasskeys, &none)
        .unwrap();

    let signed = countersignature(Class::Signed);
    let mut relabelled = signed.clone();
    relabelled.class = Class::Attested;
    assert_ne!(relabelled.challenge(), signed.challenge());
    let mut redated = signed.clone();
    redated.issued_at -= 1;
    assert_ne!(redated.challenge(), signed.challenge());
    // A rule says whether the issuer may approve; by default they may not.
    let parsed: Effect = serde_json::from_value(json!({
        "effect": "countersign", "class": "attested", "group": "pay-leads"
    }))
    .unwrap();
    assert_eq!(
        parsed,
        Effect::Countersign {
            class: Class::Attested,
            group: id("pay-leads"),
            issuer_may_approve: false
        }
    );
}

/// GR-16: an enforced allow names its mandate and its policy; a receipt's time is not negative.
#[test]
fn an_enforced_allow_names_its_mandate_and_policy() {
    let mut r = receipt(1, None, Phase::Intent);
    r.mandate.clear();
    assert!(r.validate().is_err());
    let mut r = receipt(1, None, Phase::Intent);
    r.policy_version = None;
    assert!(r.validate().is_err());
    let mut r = receipt(1, None, Phase::Intent);
    r.at = -1;
    assert!(r.validate().is_err());
}

/// GR-17: a session belongs to the agent key that opened it.
#[test]
fn another_agent_cannot_sign_into_a_session() {
    // agent-0 holds the root mandate and its own key; the session was opened by agent-1.
    let a0 = key("agent-0-key", 10);
    let r = root(&a0, "agent-0");
    let digest = update().digest();
    let mut s = session();
    let intruder = seal(&proof(u64::MAX, &digest, "m-root"), &a0).unwrap();
    assert_eq!(
        s.accept(&intruder, &r, &digest, &scope()).unwrap_err(),
        Error::WrongSession
    );
    // The session's own agent is not locked out by the attempt.
    let (_, leaf, a1) = chain();
    s.accept(
        &seal(&proof(1, &digest, "m-child-1"), &a1).unwrap(),
        &leaf,
        &digest,
        &scope(),
    )
    .unwrap();
}

/// GR-19: an action read from the wire keeps the rule that its resource lies in its system,
/// and so does a policy rule.
#[test]
fn a_resource_outside_its_system_is_refused_wherever_it_is_read() {
    let mut v = serde_json::to_value(update()).unwrap();
    assert!(serde_json::from_value::<CanonicalAction>(v.clone()).is_ok());
    v["resource"] = json!("github/repo/pay");
    assert!(serde_json::from_value::<CanonicalAction>(v.clone()).is_err());
    v["resource"] = json!("jira/project/PAY");
    v["note"] = json!("x");
    assert!(serde_json::from_value::<CanonicalAction>(v).is_err());
    let crossed = Match {
        resources: Some(vec![ResourcePattern::new("github/repo/*").unwrap()]),
        ..jira(&[], &[])
    };
    assert!(bundle(vec![rule("x", crossed, Effect::Allow)])
        .validate()
        .is_err());
}

/// GR-11: a chain head at the largest number has no next receipt; nothing overflows.
#[test]
fn the_last_possible_number_has_no_successor() {
    let gate = key("gate-1-key", 20);
    let keys = KeySet::new([gate.public()]);
    let head = ChainHead {
        seq: u64::MAX,
        hash: Digest::of(&[b"h"]),
    };
    let r = receipt(1, None, Phase::Intent);
    let opened = open::<Receipt>(&seal(&r, &gate).unwrap(), &keys, &scope()).unwrap();
    assert_eq!(
        follow(Some(&head), &opened).unwrap_err(),
        Error::BrokenChain
    );
}

/// GR-6: a verifier handed a small-order key by some other route still gains nothing useful
/// from it at enrolment, which refuses it.
#[test]
fn enrolment_refuses_a_key_anyone_can_forge_for() {
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let weak = gov_core::PublicKey::from_bytes_unchecked(id("gate-9-key"), &identity);
    assert_eq!(weak.enrolable().unwrap_err(), Error::WeakKey);
    key("gate-1-key", 20).public().enrolable().unwrap();
}

// ----- undo: the fields reserved for slice G9 (spec §4.7) --------------------------------

/// "What cannot be undone needs a person" is one rule.
#[test]
fn a_rule_can_ask_for_a_person_only_where_there_is_no_undo() {
    let labels = BTreeSet::new();
    let agent = id("agent-1");
    let subject = Subject {
        agent: &agent,
        labels: &labels,
    };
    let policy = bundle(vec![
        rule(
            "writes",
            jira(&["issue.update", "comment.send"], &[]),
            Effect::Allow,
        ),
        rule(
            "no-undo-needs-a-person",
            Match {
                reversible: Some(false),
                ..jira(&[], &[])
            },
            Effect::Countersign {
                class: Class::Attested,
                group: id("pay-leads"),
                issuer_may_approve: true,
            },
        ),
    ]);
    policy.validate().unwrap();
    assert_eq!(
        policy
            .evaluate(&subject, &update(), Reversal::Restore)
            .verdict,
        Verdict::Allow
    );
    assert_eq!(
        policy
            .evaluate(&subject, &update(), Reversal::Inverse)
            .verdict,
        Verdict::Allow
    );
    let send = action_on(
        "jira/project/PAY/issue/PAY-12",
        "comment.send",
        Kind::Write,
        json!({}),
    );
    assert!(matches!(
        policy.evaluate(&subject, &send, Reversal::None).verdict,
        Verdict::Countersign(_)
    ));
}

#[test]
fn a_before_digest_belongs_to_the_intent_of_a_write_that_can_be_put_back() {
    let mut r = receipt(1, None, Phase::Intent);
    r.before_digest = Some(Digest::of(&[b"before"]));
    r.validate().unwrap();
    r.reversal = Reversal::None;
    assert!(r.validate().is_err());
    let mut r = receipt(2, Some(Digest::of(&[b"x"])), Phase::Outcome);
    r.before_digest = Some(Digest::of(&[b"before"]));
    assert!(r.validate().is_err());
    // An undo names the receipt it puts back, and is a write.
    let mut undo = receipt(50, Some(Digest::of(&[b"r49"])), Phase::Intent);
    undo.reverses = Some(gov_core::receipt::ReceiptRef {
        gate: id("gate-1"),
        seq: 41,
        hash: Digest::of(&[b"r41"]),
    });
    undo.validate().unwrap();
    undo.action.kind = Kind::Read;
    assert!(undo.validate().is_err());
    // Every receipt says whether its operation can be put back.
    let mut v = serde_json::to_value(receipt(1, None, Phase::Intent)).unwrap();
    v.as_object_mut().unwrap().remove("reversal");
    assert!(serde_json::from_value::<Receipt>(v).is_err());
}

/// GR-39 and GR-40: an undo points at an earlier receipt; an outcome names its intent.
#[test]
fn an_undo_and_an_outcome_point_backwards() {
    let earlier = |gate: &str, seq: u64| gov_core::receipt::ReceiptRef {
        gate: id(gate),
        seq,
        hash: Digest::of(&[b"r"]),
    };
    let mut undo = receipt(5, Some(Digest::of(&[b"x"])), Phase::Intent);
    undo.reverses = Some(earlier("gate-1", 4));
    undo.validate().unwrap();
    for bad in [
        earlier("gate-1", 5),
        earlier("gate-1", 6),
        earlier("gate-1", 0),
        earlier("gate-2", 0),
    ] {
        undo.reverses = Some(bad);
        assert!(undo.validate().is_err());
    }
    // Another gate's chain has its own numbers.
    undo.reverses = Some(earlier("gate-2", 900));
    undo.validate().unwrap();

    let mut outcome = receipt(6, Some(Digest::of(&[b"x"])), Phase::Outcome);
    assert_eq!(outcome.intent, Some(5));
    outcome.validate().unwrap();
    for bad in [None, Some(0), Some(6), Some(7)] {
        outcome.intent = bad;
        assert!(outcome.validate().is_err(), "{bad:?}");
    }
    let mut intent = receipt(5, Some(Digest::of(&[b"x"])), Phase::Intent);
    intent.intent = Some(4);
    assert!(intent.validate().is_err());
}
