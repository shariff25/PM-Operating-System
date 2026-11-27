//! Agent Governance, the Authority. Spec: `docs/agent-governance-spec.md` §7, and §7.6 for
//! how this slice is cut.
//!
//! Step G4a: the registry of people, agents and gates; enrolment of an agent's or a gate's
//! key by a one-time code; and the signed, chained record of everything an admin did. It is
//! a library. Nothing here is served on a listener, and there is no sign-in yet: **every
//! record made by this step says its actor is unverified** (review finding GR-111).
//!
//! **A check and the write it guards are one step** (review finding GR-119). Every action
//! reads the chain's head first, makes its checks, and commits on that head; the store
//! refuses the commit if the head has moved, and the action starts again. Nothing changes
//! the registry without moving the head, so no check can be overtaken.

pub mod pg;
pub mod store;

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use gov_core::admin::{Actor, AdminAction, AdminRecord, Subject, SubjectKind};
use gov_core::enrol::{code_digest, key_digest, new_code, EnrolKind, EnrolRequest};
use gov_core::lease::{batch_digest, IntakeAck, ShipRequest};
use gov_core::receipt::ChainHead;
use gov_core::{
    open, random_id, seal, Digest, Envelope, Id, KeySet, PublicKey, Scope, SigningKey, VERSION,
};

pub use store::{
    Agent, Effect, EnrolledKey, Gate, IssuedMandate, MemoryStore, PendingCode, Person, Published,
    Role, Status, Store, StoreError, StoredRecord,
};

/// How long a one-time code is good for, in seconds.
pub const CODE_SECS: i64 = 15 * 60;
/// Wrong codes for one name before its pending code is cancelled.
pub const WRONG_CODES: u32 = 5;
/// How often an action starts again when another writer moved the chain's head first.
const TRIES: u32 = 16;

#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The actor is not an active admin of this installation.
    #[error("not allowed")]
    NotAllowed,
    /// What the action names does not exist, or is not in a state to take it.
    #[error("no such {0}")]
    Unknown(&'static str),
    #[error("{0} exists already")]
    Exists(&'static str),
    /// A bundle offered for publishing does not carry the next number, which is this one.
    #[error("the next bundle is number {0}")]
    NotNext(u64),
    /// A request to enrol was refused. It says no more: the caller may be anyone.
    #[error("refused")]
    Refused,
    /// The Ledger's answer does not agree with this Authority's own chain at this record.
    #[error("the Ledger disagrees with this Authority's chain at record {0}")]
    LedgerDisagrees(u64),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Core(#[from] gov_core::Error),
}

type Result<T> = std::result::Result<T, AuthorityError>;

/// A batch of this Authority's records for the Ledger, under a request signed with the key
/// the records are signed with.
#[derive(Debug, Clone)]
pub struct Shipment {
    pub request: Envelope,
    pub batch: Vec<Envelope>,
    nonce: Id,
}

pub struct Authority<S> {
    store: S,
    scope: Scope,
    /// The Authority's name: its chain's, and the one a request to enrol must be for.
    name: Id,
    /// Signs admin records, and nothing else.
    admin_key: SigningKey,
    /// The root key gates and agents pin when they enrol. This step only hands it out.
    root: PublicKey,
    /// Signs mandates, and nothing else. Absent until the installation has one; an
    /// Authority without it issues nothing.
    mandates_key: Option<SigningKey>,
    /// Signs the revocation feed (and leases, G4d), and nothing else.
    leases_key: Option<SigningKey>,
}

/// Where a mandate being issued comes from: the person at the root of its chain, and the
/// mandate it is delegated from, if it is.
struct Origin<'a> {
    root_person: &'a Id,
    parent: Option<Id>,
}

/// What a person asks for when they ask for a mandate (spec §7.6.2, point 2).
#[derive(Debug, Clone)]
pub struct MandateAsk {
    pub agent: Id,
    /// Which of the agent's enrolled keys the mandate is bound to.
    pub key: Id,
    /// The template's id, as the latest published bundle names it.
    pub template: Id,
    /// At most the template's. Nothing is granted by default.
    pub grants: Vec<gov_core::action::Grant>,
    pub life_secs: u64,
    /// Absent ceilings take the template's.
    pub limits: gov_core::mandate::Limits,
}

/// What listing shows of a mandate: never its grants' resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MandateInfo {
    pub id: Id,
    pub agent: Id,
    pub issued_by: Id,
    pub root_person: Id,
    pub template: Id,
    pub issued_at: i64,
    pub expires_at: i64,
    pub revoked: bool,
}

/// How far an asking agent's clock may be from this Authority's, and how long past its
/// expiry a revocation stays in the feed.
pub const CLOCK_ALLOWANCE_SECS: i64 = 300;

/// What a record says was done: the action, what it was done to, the second thing it
/// names, and the digest of what was set.
struct Did {
    action: AdminAction,
    on: Subject,
    about: Option<Subject>,
    value: Option<Digest>,
}

impl Did {
    fn to(action: AdminAction, kind: SubjectKind, id: &Id) -> Self {
        Did {
            action,
            on: subject(kind, id),
            about: None,
            value: None,
        }
    }
}

fn subject(kind: SubjectKind, id: &Id) -> Subject {
    Subject {
        kind,
        id: id.clone(),
    }
}

fn of(kind: EnrolKind) -> SubjectKind {
    match kind {
        EnrolKind::Agent => SubjectKind::Agent,
        EnrolKind::Gate => SubjectKind::Gate,
    }
}

/// Run `$body` on the chain's head as it stands, and again from the top if the head moved
/// before `$body`'s commit landed. `$head` is the head every check in `$body` is made on.
macro_rules! on_head {
    ($self:ident, $head:ident, $body:block) => {{
        let mut tries = 0;
        loop {
            let $head = $self.store.head(&$self.scope).await?;
            let out: Result<_> = async { $body }.await;
            match out {
                Err(AuthorityError::Store(StoreError::HeadMoved)) if tries < TRIES => tries += 1,
                other => break other,
            }
        }
    }};
}

impl<S: Store> Authority<S> {
    pub fn new(store: S, scope: Scope, name: Id, admin_key: SigningKey, root: PublicKey) -> Self {
        Authority {
            store,
            scope,
            name,
            admin_key,
            root,
            mandates_key: None,
            leases_key: None,
        }
    }

    /// Give the Authority its two online keys. Each must be listed under its own role in the
    /// latest published bundle before it is of any use: that is checked when it signs.
    pub fn with_online_keys(mut self, mandates: SigningKey, leases: SigningKey) -> Self {
        self.mandates_key = Some(mandates);
        self.leases_key = Some(leases);
        self
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// The public key admin records are signed with: what the Ledger and an auditor hold.
    pub fn admin_public(&self) -> PublicKey {
        self.admin_key.public()
    }

    /// Write one record on `head` and apply what it stands for, together. If the head has
    /// moved the store writes nothing and this fails; the caller starts again.
    async fn commit(
        &self,
        head: Option<ChainHead>,
        actor: &Id,
        did: Did,
        effects: &[Effect],
        now: i64,
    ) -> Result<u64> {
        // A record's time never goes back along the chain, whatever the clock does.
        let at = match &head {
            Some(h) => self
                .store
                .records_after(&self.scope, h.seq - 1, 1)
                .await?
                .first()
                .map_or(now, |last| now.max(last.at)),
            None => now,
        };
        let record = AdminRecord {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            authority: self.name.clone(),
            seq: head.as_ref().map_or(1, |h| h.seq + 1),
            prev: head.map(|h| h.hash),
            at,
            actor: Actor {
                id: actor.clone(),
                // Nobody is signed in before G4e.
                verified: false,
            },
            action: did.action,
            subject: did.on,
            about: did.about,
            value: did.value,
        };
        record.validate()?;
        let envelope = seal(&record, &self.admin_key)?;
        let keys = KeySet::new([self.admin_key.public()]);
        let hash = open::<AdminRecord>(&envelope, &keys, &self.scope)?.hash;
        let stored = StoredRecord {
            seq: record.seq,
            hash,
            at,
            envelope,
        };
        self.store.commit(&self.scope, &stored, effects).await?;
        Ok(record.seq)
    }

    /// Only an active admin acts. Before sign-in exists this checks the name given, not the
    /// person: the record says so.
    async fn admin(&self, actor: &Id) -> Result<()> {
        match self.store.person(&self.scope, actor).await? {
            Some(p) if p.active && p.role == Role::Admin => Ok(()),
            _ => Err(AuthorityError::NotAllowed),
        }
    }

    /// Name the installation's first admin. Possible exactly once, while nobody is known.
    pub async fn bootstrap(&self, first_admin: &Id, now: i64) -> Result<u64> {
        on_head!(self, head, {
            if head.is_some() || self.store.any_person(&self.scope).await? {
                return Err(AuthorityError::Exists("an admin"));
            }
            let person = Person {
                id: first_admin.clone(),
                role: Role::Admin,
                active: true,
            };
            let did = Did::to(AdminAction::PersonAdded, SubjectKind::Person, first_admin);
            self.commit(head, first_admin, did, &[Effect::PutPerson(person)], now)
                .await
        })
    }

    pub async fn add_person(&self, actor: &Id, person: &Id, role: Role, now: i64) -> Result<u64> {
        on_head!(self, head, {
            self.admin(actor).await?;
            if self.store.person(&self.scope, person).await?.is_some() {
                return Err(AuthorityError::Exists("person"));
            }
            let added = Person {
                id: person.clone(),
                role,
                active: true,
            };
            let did = Did::to(AdminAction::PersonAdded, SubjectKind::Person, person);
            self.commit(head, actor, did, &[Effect::PutPerson(added)], now)
                .await
        })
    }

    /// A person leaves. Each of their agents is suspended first, with a record each, and
    /// the person is marked last: stopped halfway, what remains is a person still listed
    /// whose agents are already stopped, never the other way round. (An agent is in any
    /// case usable only while its owner is active: see [`Authority::agent_usable`].)
    pub async fn offboard_person(&self, actor: &Id, person: &Id, now: i64) -> Result<u64> {
        if actor == person {
            // The last admin cannot remove themselves and leave nobody able to act.
            return Err(AuthorityError::NotAllowed);
        }
        self.admin(actor).await?;
        if self.store.person(&self.scope, person).await?.is_none() {
            return Err(AuthorityError::Unknown("person"));
        }
        for agent in self.store.agents_of(&self.scope, person).await? {
            match self.suspend_agent(actor, &agent.id, now).await {
                // Suspended or disabled already.
                Ok(_) | Err(AuthorityError::Unknown(_)) => {}
                Err(e) => return Err(e),
            }
        }
        on_head!(self, head, {
            self.admin(actor).await?;
            let Some(mut leaving) = self.store.person(&self.scope, person).await? else {
                return Err(AuthorityError::Unknown("person"));
            };
            leaving.active = false;
            let did = Did::to(AdminAction::PersonOffboarded, SubjectKind::Person, person);
            self.commit(head, actor, did, &[Effect::PutPerson(leaving)], now)
                .await
        })
    }

    /// Name an agent and the person who answers for it. It has no key until it enrols.
    pub async fn register_agent(
        &self,
        actor: &Id,
        agent: &Id,
        owner: &Id,
        labels: BTreeSet<Id>,
        now: i64,
    ) -> Result<u64> {
        on_head!(self, head, {
            self.admin(actor).await?;
            if self.store.agent(&self.scope, agent).await?.is_some() {
                return Err(AuthorityError::Exists("agent"));
            }
            // A key's id is under the name it is for, so an agent may not take a gate's
            // name, nor the Authority's own (review finding GR-127).
            if agent == &self.name || self.store.gate(&self.scope, agent).await?.is_some() {
                return Err(AuthorityError::Exists("name"));
            }
            match self.store.person(&self.scope, owner).await? {
                Some(p) if p.active => {}
                _ => return Err(AuthorityError::Unknown("owner")),
            }
            let registered = Agent {
                id: agent.clone(),
                owner: owner.clone(),
                labels: labels.clone(),
                status: Status::Active,
            };
            let did = Did {
                about: Some(subject(SubjectKind::Person, owner)),
                ..Did::to(AdminAction::AgentRegistered, SubjectKind::Agent, agent)
            };
            self.commit(head, actor, did, &[Effect::PutAgent(registered)], now)
                .await
        })
    }

    pub async fn register_gate(&self, actor: &Id, gate: &Id, now: i64) -> Result<u64> {
        on_head!(self, head, {
            self.admin(actor).await?;
            if self.store.gate(&self.scope, gate).await?.is_some() {
                return Err(AuthorityError::Exists("gate"));
            }
            if gate == &self.name || self.store.agent(&self.scope, gate).await?.is_some() {
                return Err(AuthorityError::Exists("name"));
            }
            let registered = Gate {
                id: gate.clone(),
                status: Status::Active,
            };
            let did = Did::to(AdminAction::GateRegistered, SubjectKind::Gate, gate);
            self.commit(head, actor, did, &[Effect::PutGate(registered)], now)
                .await
        })
    }

    async fn set_agent(
        &self,
        actor: &Id,
        agent: &Id,
        action: AdminAction,
        from: &[Status],
        to: Status,
        now: i64,
    ) -> Result<u64> {
        // Stopping an agent revokes its live mandates first, each with its own record
        // (spec §7.6.2, point 6).
        if to != Status::Active {
            self.admin(actor).await?;
            match self.store.agent(&self.scope, agent).await? {
                Some(a) if from.contains(&a.status) => {}
                Some(_) => return Err(AuthorityError::Unknown("agent in that state")),
                None => return Err(AuthorityError::Unknown("agent")),
            }
            self.revoke_agents_mandates(actor, agent, now).await?;
        }
        on_head!(self, head, {
            self.admin(actor).await?;
            let Some(mut found) = self.store.agent(&self.scope, agent).await? else {
                return Err(AuthorityError::Unknown("agent"));
            };
            if !from.contains(&found.status) {
                return Err(AuthorityError::Unknown("agent in that state"));
            }
            found.status = to;
            // A code waiting for an agent that is stopped is of no more use (GR-118).
            let mut effects = vec![Effect::PutAgent(found)];
            if to != Status::Active {
                effects.push(Effect::DropCode(EnrolKind::Agent, agent.clone()));
            }
            // The feed only adds, so only what is for good goes in it: a disabled agent's
            // id, never a suspended one's (review finding GR-140).
            if to == Status::Disabled {
                let seq = self
                    .store
                    .revocations_after(&self.scope, 0)
                    .await?
                    .last()
                    .map_or(1, |e| e.seq + 1);
                effects.push(Effect::AddRevocation(
                    gov_core::revocation::RevocationEntry {
                        seq,
                        kind: gov_core::revocation::Revocable::Agent,
                        id: agent.clone(),
                        until: i64::MAX,
                    },
                ));
            }
            let did = Did::to(action, SubjectKind::Agent, agent);
            self.commit(head, actor, did, &effects, now).await
        })
    }

    pub async fn suspend_agent(&self, actor: &Id, agent: &Id, now: i64) -> Result<u64> {
        let (action, to) = (AdminAction::AgentSuspended, Status::Suspended);
        self.set_agent(actor, agent, action, &[Status::Active], to, now)
            .await
    }

    pub async fn resume_agent(&self, actor: &Id, agent: &Id, now: i64) -> Result<u64> {
        let (action, to) = (AdminAction::AgentResumed, Status::Active);
        self.set_agent(actor, agent, action, &[Status::Suspended], to, now)
            .await
    }

    /// For good: a disabled agent is not resumed.
    pub async fn disable_agent(&self, actor: &Id, agent: &Id, now: i64) -> Result<u64> {
        let from = [Status::Active, Status::Suspended];
        let (action, to) = (AdminAction::AgentDisabled, Status::Disabled);
        self.set_agent(actor, agent, action, &from, to, now).await
    }

    pub async fn disable_gate(&self, actor: &Id, gate: &Id, now: i64) -> Result<u64> {
        on_head!(self, head, {
            self.admin(actor).await?;
            match self.store.gate(&self.scope, gate).await? {
                Some(g) if g.status != Status::Disabled => {}
                _ => return Err(AuthorityError::Unknown("gate")),
            }
            let disabled = Gate {
                id: gate.clone(),
                status: Status::Disabled,
            };
            let effects = [
                Effect::PutGate(disabled),
                Effect::DropCode(EnrolKind::Gate, gate.clone()),
            ];
            let did = Did::to(AdminAction::GateDisabled, SubjectKind::Gate, gate);
            self.commit(head, actor, did, &effects, now).await
        })
    }

    /// Publish a policy bundle (spec §7.6.1, point 4). It was signed elsewhere, with the root
    /// key, from the reviewed repository; here it is opened under that root, must be this
    /// Authority's and carry the next number, and is kept together with a record of its
    /// number and digest. Returns the digest, for whoever signed it to compare.
    pub async fn publish_bundle(&self, actor: &Id, signed: &Envelope, now: i64) -> Result<Digest> {
        on_head!(self, head, {
            self.admin(actor).await?;
            let opened = gov_core::policy::open_bundle(signed, &self.root, &self.scope)?;
            if opened.bundle.issuer != self.name {
                return Err(AuthorityError::Unknown("bundle for this Authority"));
            }
            let latest = self.store.latest_bundle(&self.scope).await?;
            let next = latest.map_or(1, |b| b.number.saturating_add(1));
            if opened.bundle.number != next {
                return Err(AuthorityError::NotNext(next));
            }
            let number = Id::new(&opened.bundle.number.to_string())?;
            let did = Did {
                value: Some(opened.digest.clone()),
                ..Did::to(AdminAction::PolicyPublished, SubjectKind::Policy, &number)
            };
            let kept = Published {
                number: opened.bundle.number,
                digest: opened.digest.clone(),
                envelope: signed.clone(),
            };
            self.commit(head, actor, did, &[Effect::PutBundle(kept)], now)
                .await?;
            Ok(opened.digest)
        })
    }

    /// The latest published bundle, opened, and the key mandates are signed with, only if
    /// that bundle lists it under `mandates`: an Authority whose mandates key the root has
    /// not named issues nothing.
    async fn issuing(&self) -> Result<(gov_core::policy::OpenedBundle, &SigningKey)> {
        let latest = self
            .store
            .latest_bundle(&self.scope)
            .await?
            .ok_or(AuthorityError::Unknown("published bundle"))?;
        let opened = gov_core::policy::open_bundle(&latest.envelope, &self.root, &self.scope)?;
        let key = self
            .mandates_key
            .as_ref()
            .ok_or(AuthorityError::Unknown("mandates key"))?;
        if !opened.bundle.keys.mandates.contains(&key.public()) {
            return Err(AuthorityError::Unknown("mandates key in the bundle"));
        }
        Ok((opened, key))
    }

    /// Everything revoked so far, for checking a chain.
    async fn revoked(&self) -> Result<gov_core::revocation::RevocationSet> {
        let mut set = gov_core::revocation::RevocationSet::default();
        for e in self.store.revocations_after(&self.scope, 0).await? {
            set.revoke(e.kind, e.id);
        }
        Ok(set)
    }

    /// The agent's enrolled key of this id, if the agent is usable and the key is its own.
    async fn agent_key(&self, agent: &Id, kid: &Id) -> Result<PublicKey> {
        if !self.agent_usable(agent).await? {
            return Err(AuthorityError::Unknown("usable agent"));
        }
        match self.store.key(&self.scope, kid).await? {
            Some(k) if k.kind == EnrolKind::Agent && &k.name == agent => Ok(k.key),
            _ => Err(AuthorityError::Unknown("key of that agent")),
        }
    }

    /// Sign `mandate`, check it as a gate holding the same bundle would, and write it with
    /// its `MandateIssued` record in one step.
    async fn issue(
        &self,
        head: Option<ChainHead>,
        actor: &Id,
        mandate: gov_core::mandate::Mandate,
        origin: Origin<'_>,
        with: (&gov_core::policy::OpenedBundle, &SigningKey),
        now: i64,
    ) -> Result<Envelope> {
        use gov_core::mandate::{verify_chain, ChainCheck};
        let (opened, key) = with;
        let Origin {
            root_person,
            parent,
        } = origin;
        let envelope = seal(&mandate, key)?;
        let issuers = KeySet::new(opened.bundle.keys.mandates.iter().cloned());
        let revoked = self.revoked().await?;
        // The gate's own check, made here first: nothing is issued that a gate would refuse.
        verify_chain(
            &envelope,
            &ChainCheck {
                scope: &self.scope,
                authority: &issuers,
                now,
                revoked: &revoked,
                templates: Some(&opened.bundle.templates),
            },
        )?;
        let digest = open::<gov_core::mandate::Mandate>(&envelope, &issuers, &self.scope)?.hash;
        let kept = IssuedMandate {
            id: mandate.id.clone(),
            agent: mandate.agent.id.clone(),
            issued_by: mandate.issued_by.id.clone(),
            root_person: root_person.clone(),
            parent,
            template: mandate.template.clone(),
            issued_at: mandate.issued_at,
            expires_at: mandate.expires_at,
            revoked: false,
            envelope: envelope.clone(),
        };
        let did = Did {
            about: Some(subject(SubjectKind::Agent, &mandate.agent.id)),
            value: Some(digest),
            ..Did::to(
                AdminAction::MandateIssued,
                SubjectKind::Mandate,
                &mandate.id,
            )
        };
        self.commit(head, actor, did, &[Effect::PutMandate(kept)], now)
            .await?;
        Ok(envelope)
    }

    /// Issue a mandate to a person's request (spec §7.6.2, point 2). The person is the
    /// agent's owner, or one the template names; being an admin is not enough. Nobody is
    /// signed in before G4e, so the mandate carries no session, and an enforcing gate will
    /// refuse it (point 3).
    pub async fn issue_mandate(&self, person: &Id, ask: &MandateAsk, now: i64) -> Result<Envelope> {
        use gov_core::mandate::{AgentRef, IssuedBy, IssuerKind, Limits, Mandate, Requires};
        on_head!(self, head, {
            let (opened, key) = self.issuing().await?;
            match self.store.person(&self.scope, person).await? {
                Some(p) if p.active => {}
                _ => return Err(AuthorityError::NotAllowed),
            }
            let agent = self
                .store
                .agent(&self.scope, &ask.agent)
                .await?
                .ok_or(AuthorityError::Unknown("agent"))?;
            let agent_key = self.agent_key(&ask.agent, &ask.key).await?;
            let template = opened
                .bundle
                .templates
                .iter()
                .find(|t| t.id == ask.template)
                .ok_or(AuthorityError::Unknown("template"))?;
            if &agent.owner != person && !template.people.contains(person) {
                return Err(AuthorityError::NotAllowed);
            }
            let life = i64::try_from(ask.life_secs).map_err(|_| AuthorityError::Refused)?;
            if life == 0 {
                return Err(AuthorityError::Refused);
            }
            let mandate = Mandate {
                v: VERSION,
                tenant: self.scope.tenant.clone(),
                env: self.scope.env.clone(),
                id: random_id("m")?,
                issuer: self.name.clone(),
                issued_at: now,
                expires_at: now.saturating_add(life),
                agent: AgentRef {
                    id: ask.agent.clone(),
                    key: agent_key,
                },
                issued_by: IssuedBy {
                    kind: IssuerKind::Person,
                    id: person.clone(),
                    session: None,
                },
                template: Id::new(&format!("{}@{}", template.id, opened.bundle.number))?,
                grants: ask.grants.clone(),
                limits: Limits {
                    max_actions: ask.limits.max_actions.or(template.limits.max_actions),
                    max_writes: ask.limits.max_writes.or(template.limits.max_writes),
                },
                requires: Requires::default(),
                parent: None,
            };
            let origin = Origin {
                root_person: person,
                parent: None,
            };
            self.issue(head, person, mandate, origin, (&opened, key), now)
                .await
        })
    }

    /// Issue a narrower mandate to another agent, at the signed request of the agent that
    /// holds the parent (spec §7.6.2, point 4).
    pub async fn delegate(&self, request: &Envelope, now: i64) -> Result<Envelope> {
        use gov_core::mandate::{
            verify_chain, AgentRef, ChainCheck, DelegateRequest, IssuedBy, IssuerKind, Limits,
            Mandate, MAX_CHAIN,
        };
        on_head!(self, head, {
            let (opened, key) = self.issuing().await?;
            // The request is signed with an enrolled agent key: which one is in the open.
            let signer = match self.store.key(&self.scope, &request.kid).await? {
                Some(k) if k.kind == EnrolKind::Agent => k,
                _ => return Err(AuthorityError::Refused),
            };
            let asked =
                open::<DelegateRequest>(request, &KeySet::new([signer.key.clone()]), &self.scope)
                    .map_err(|_| AuthorityError::Refused)?;
            let ask = asked.doc;
            if ask.authority != self.name || ask.at.abs_diff(now) > CLOCK_ALLOWANCE_SECS as u64 {
                return Err(AuthorityError::Refused);
            }
            // The same request is never served twice: the mandate's id is the request's.
            let id = Id::new(&format!("m-{}", &asked.hash.as_str()[..32]))?;
            if self.store.mandate(&self.scope, &id).await?.is_some() {
                return Err(AuthorityError::Refused);
            }
            let parent = match self.store.mandate(&self.scope, &ask.parent).await? {
                Some(p) if !p.revoked => p,
                _ => return Err(AuthorityError::Refused),
            };
            // The parent verifies now, as a gate would check it, and the request is signed
            // with the very key the parent mandate is bound to.
            let issuers = KeySet::new(opened.bundle.keys.mandates.iter().cloned());
            let revoked = self.revoked().await?;
            let chain = verify_chain(
                &parent.envelope,
                &ChainCheck {
                    scope: &self.scope,
                    authority: &issuers,
                    now,
                    revoked: &revoked,
                    templates: Some(&opened.bundle.templates),
                },
            )
            .map_err(|_| AuthorityError::Refused)?;
            let held = chain.leaf();
            if held.agent.key != signer.key || held.agent.id != signer.name {
                return Err(AuthorityError::Refused);
            }
            if chain.links().len() >= MAX_CHAIN || ask.agent == held.agent.id {
                return Err(AuthorityError::Refused);
            }
            // The receiving side agrees: the child's owner is the person at the root, or
            // one the template names (review finding GR-139).
            let child = self
                .store
                .agent(&self.scope, &ask.agent)
                .await?
                .ok_or(AuthorityError::Refused)?;
            let child_key = self
                .agent_key(&ask.agent, &ask.key)
                .await
                .map_err(|_| AuthorityError::Refused)?;
            let template = opened
                .bundle
                .templates
                .iter()
                .find(|t| t.id.as_str() == gov_core::mandate::template_id(&held.template))
                .ok_or(AuthorityError::Refused)?;
            if child.owner != parent.root_person && !template.receivers.contains(&child.owner) {
                return Err(AuthorityError::Refused);
            }
            let mandate = Mandate {
                id,
                issued_at: now,
                expires_at: ask.expires_at,
                agent: AgentRef {
                    id: ask.agent.clone(),
                    key: child_key,
                },
                issued_by: IssuedBy {
                    kind: IssuerKind::Agent,
                    id: held.agent.id.clone(),
                    session: None,
                },
                grants: ask.grants.clone(),
                limits: Limits {
                    max_actions: ask.limits.max_actions.or(held.limits.max_actions),
                    max_writes: ask.limits.max_writes.or(held.limits.max_writes),
                },
                parent: Some(Box::new(parent.envelope.clone())),
                ..held.clone()
            };
            let actor = held.agent.id.clone();
            // Wider than the parent in grants, time or limits: the gate's check refuses it,
            // and so nothing is written.
            let origin = Origin {
                root_person: &parent.root_person,
                parent: Some(parent.id.clone()),
            };
            self.issue(head, &actor, mandate, origin, (&opened, key), now)
                .await
                .map_err(|e| match e {
                    AuthorityError::Core(_) => AuthorityError::Refused,
                    other => other,
                })
        })
    }

    /// The mandates a person may see (point 5): those of agents they own; every one for an
    /// admin or an auditor. Never a grant's resources.
    pub async fn list_mandates(&self, person: &Id) -> Result<Vec<MandateInfo>> {
        let viewer = match self.store.person(&self.scope, person).await? {
            Some(p) if p.active => p,
            _ => return Err(AuthorityError::NotAllowed),
        };
        let all = matches!(viewer.role, Role::Admin | Role::Auditor);
        let mut out = Vec::new();
        for m in self.store.mandates(&self.scope).await? {
            let owned = match self.store.agent(&self.scope, &m.agent).await? {
                Some(a) => &a.owner == person,
                None => false,
            };
            if all || owned {
                out.push(MandateInfo {
                    id: m.id,
                    agent: m.agent,
                    issued_by: m.issued_by,
                    root_person: m.root_person,
                    template: m.template,
                    issued_at: m.issued_at,
                    expires_at: m.expires_at,
                    revoked: m.revoked,
                });
            }
        }
        Ok(out)
    }

    /// Revoke one mandate (point 6): its number in the feed is returned. One entry covers
    /// its descendants, since a gate checks every link of a chain. `by_right` skips the
    /// check of who asks, for the Authority's own use when an agent is stopped.
    async fn revoke(&self, actor: &Id, mandate: &Id, by_right: bool, now: i64) -> Result<u64> {
        use gov_core::revocation::{Revocable, RevocationEntry};
        on_head!(self, head, {
            let Some(mut found) = self.store.mandate(&self.scope, mandate).await? else {
                return Err(AuthorityError::Unknown("mandate"));
            };
            if found.revoked {
                return Err(AuthorityError::Unknown("mandate in that state"));
            }
            if !by_right {
                let asker = match self.store.person(&self.scope, actor).await? {
                    Some(p) if p.active => p,
                    _ => return Err(AuthorityError::NotAllowed),
                };
                let owner = self
                    .store
                    .agent(&self.scope, &found.agent)
                    .await?
                    .map(|a| a.owner);
                let may = asker.role == Role::Admin
                    || actor == &found.root_person
                    || owner.as_ref() == Some(actor);
                if !may {
                    return Err(AuthorityError::NotAllowed);
                }
            }
            let seq = self
                .store
                .revocations_after(&self.scope, 0)
                .await?
                .last()
                .map_or(1, |e| e.seq + 1);
            let entry = RevocationEntry {
                seq,
                kind: Revocable::Mandate,
                id: mandate.clone(),
                until: found.expires_at,
            };
            found.revoked = true;
            let did = Did::to(AdminAction::Revoked, SubjectKind::Mandate, mandate);
            let effects = [Effect::PutMandate(found), Effect::AddRevocation(entry)];
            self.commit(head, actor, did, &effects, now).await?;
            Ok(seq)
        })
    }

    pub async fn revoke_mandate(&self, actor: &Id, mandate: &Id, now: i64) -> Result<u64> {
        self.revoke(actor, mandate, false, now).await
    }

    /// Revoke every live mandate of an agent, each with its own record. Done **before** the
    /// change that stops the agent, so that a stop between the two leaves an agent with
    /// less authority than it should have, never more.
    async fn revoke_agents_mandates(&self, actor: &Id, agent: &Id, now: i64) -> Result<()> {
        for m in self.store.mandates(&self.scope).await? {
            if &m.agent == agent && !m.revoked && m.expires_at > now {
                match self.revoke(actor, &m.id, true, now).await {
                    Ok(_) | Err(AuthorityError::Unknown(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    /// Everything revoked after number `after`, signed with the leases key (point 7). An
    /// entry whose subject expired longer ago than the clock allowance is left out and
    /// counted, so the asker sees that every number is accounted for.
    pub async fn revocation_feed(&self, after: u64, now: i64) -> Result<Envelope> {
        use gov_core::revocation::RevocationFeed;
        let latest = self
            .store
            .latest_bundle(&self.scope)
            .await?
            .ok_or(AuthorityError::Unknown("published bundle"))?;
        let opened = gov_core::policy::open_bundle(&latest.envelope, &self.root, &self.scope)?;
        let key = self
            .leases_key
            .as_ref()
            .ok_or(AuthorityError::Unknown("leases key"))?;
        if !opened.bundle.keys.leases.contains(&key.public()) {
            return Err(AuthorityError::Unknown("leases key in the bundle"));
        }
        let all = self.store.revocations_after(&self.scope, 0).await?;
        let through = all.last().map_or(0, |e| e.seq);
        if after > through {
            return Err(AuthorityError::Refused);
        }
        let later: Vec<_> = all.into_iter().filter(|e| e.seq > after).collect();
        let total = later.len() as u64;
        let entries: Vec<_> = later
            .into_iter()
            .filter(|e| e.until.saturating_add(CLOCK_ALLOWANCE_SECS) >= now)
            .collect();
        let feed = RevocationFeed {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            issuer: self.name.clone(),
            after,
            through,
            dropped: total - entries.len() as u64,
            at: now,
            entries,
        };
        feed.validate()?;
        Ok(seal(&feed, key)?)
    }

    /// The bundle gates are to hold: the last one published, as it was signed.
    pub async fn latest_bundle(&self) -> Result<Option<Published>> {
        Ok(self.store.latest_bundle(&self.scope).await?)
    }

    /// Whether an agent may act: it is active, and so is the person who answers for it.
    pub async fn agent_usable(&self, agent: &Id) -> Result<bool> {
        let Some(found) = self.store.agent(&self.scope, agent).await? else {
            return Ok(false);
        };
        let owner = self.store.person(&self.scope, &found.owner).await?;
        Ok(found.status == Status::Active && owner.is_some_and(|p| p.active))
    }

    /// Whether a key may be enrolled for this name now: it is registered and not stopped.
    async fn enrolable(&self, kind: EnrolKind, name: &Id) -> Result<bool> {
        Ok(match kind {
            EnrolKind::Agent => self.agent_usable(name).await?,
            EnrolKind::Gate => self
                .store
                .gate(&self.scope, name)
                .await?
                .is_some_and(|g| g.status == Status::Active),
        })
    }

    /// Make a one-time code for a registered agent or gate to enrol its key with. The code
    /// is returned once and never stored: only its digest is. A code made earlier for the
    /// same name is replaced. The code also names the root key its holder is to pin.
    pub async fn offer_enrolment(
        &self,
        actor: &Id,
        kind: EnrolKind,
        name: &Id,
        now: i64,
    ) -> Result<String> {
        let code = new_code(&self.root)?;
        on_head!(self, head, {
            self.admin(actor).await?;
            if !self.enrolable(kind, name).await? {
                return Err(AuthorityError::Unknown("agent or gate to enrol"));
            }
            let pending = PendingCode {
                kind,
                name: name.clone(),
                digest: code_digest(&code),
                expires_at: now + CODE_SECS,
                by: actor.clone(),
                wrong: 0,
            };
            let did = Did::to(AdminAction::EnrolmentOffered, of(kind), name);
            self.commit(head, actor, did, &[Effect::PutCode(pending)], now)
                .await
        })?;
        Ok(code)
    }

    /// Enrol a key. `code` is the code itself, as the admin gave it out; `request` is signed
    /// by the key being enrolled and names who is enrolling (spec §7.6, choice 3). Returns
    /// the Authority's root key, which the caller pins only if its code names that key
    /// ([`gov_core::enrol::code_names_root`]).
    ///
    /// The caller may be anyone, so every refusal looks the same. A wrong code is counted,
    /// and the fifth cancels the pending code, with a record. The right code is taken by
    /// exactly one request, whether or not the rest of that request holds.
    pub async fn enrol(&self, code: &str, request: &Envelope, now: i64) -> Result<PublicKey> {
        // Who the request says it is from, read before anything is believed, so that the
        // signature can be checked against the very key it asks to enrol.
        let claimed: EnrolRequest = URL_SAFE_NO_PAD
            .decode(&request.payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(AuthorityError::Refused)?;
        let (kind, name) = (claimed.kind, claimed.name.clone());
        // The code is taken, not read: of two requests with the right code, one gets it.
        let presented = code_digest(code);
        let Some(pending) = self
            .store
            .take_code(&self.scope, kind, &name, &presented)
            .await?
        else {
            self.wrong_code(kind, &name, now).await?;
            return Err(AuthorityError::Refused);
        };
        if now >= pending.expires_at {
            return Err(AuthorityError::Refused);
        }
        let own = KeySet::new([claimed.key.clone()]);
        let asked = open::<EnrolRequest>(request, &own, &self.scope)
            .map_err(|_| AuthorityError::Refused)?
            .doc;
        if asked.authority != self.name || asked.code != pending.digest {
            return Err(AuthorityError::Refused);
        }
        // A key anyone could forge for is not enrolled. Nor is one whose id is not under
        // the name it is for: `<name>:<anything>`, so no gate or agent can take an id
        // another will want (review finding GR-116).
        let own_id = asked
            .key
            .kid
            .as_str()
            .rsplit_once(':')
            .is_some_and(|(owner, rest)| owner == name.as_str() && !rest.is_empty());
        if asked.key.enrolable().is_err() || !own_id {
            return Err(AuthorityError::Refused);
        }
        let enrolled = EnrolledKey {
            kind,
            name: name.clone(),
            key: asked.key.clone(),
        };
        let done = on_head!(self, head, {
            // The agent or gate may have been stopped since the code was made (GR-118).
            if !self.enrolable(kind, &name).await? {
                return Err(AuthorityError::Refused);
            }
            let did = Did {
                action: AdminAction::KeyEnrolled,
                on: subject(SubjectKind::Key, &asked.key.kid),
                about: Some(subject(of(kind), &name)),
                value: Some(key_digest(&asked.key)?),
            };
            self.commit(
                head,
                &pending.by,
                did,
                &[Effect::AddKey(enrolled.clone())],
                now,
            )
            .await
        });
        match done {
            Ok(_) => Ok(self.root.clone()),
            // The key id, or the key itself, is enrolled already.
            Err(AuthorityError::Store(StoreError::KeyTaken)) => Err(AuthorityError::Refused),
            Err(e) => Err(e),
        }
    }

    /// A wrong code was presented for a name. It is counted, and the fifth cancels the
    /// pending code: by a record, because an enrolment an admin offered has been stopped
    /// (review finding GR-117). The actor of that record is the Authority itself.
    async fn wrong_code(&self, kind: EnrolKind, name: &Id, now: i64) -> Result<()> {
        if self.store.count_wrong_code(&self.scope, kind, name).await? < WRONG_CODES {
            return Ok(());
        }
        on_head!(self, head, {
            if self.store.code(&self.scope, kind, name).await?.is_none() {
                return Ok(());
            }
            let did = Did::to(AdminAction::EnrolmentCancelled, of(kind), name);
            let effects = [Effect::DropCode(kind, name.clone())];
            self.commit(head, &self.name, did, &effects, now).await?;
            Ok(())
        })
    }

    /// Every record of this Authority's chain, opened under its own key, in order.
    pub async fn records(&self) -> Result<Vec<AdminRecord>> {
        let keys = KeySet::new([self.admin_key.public()]);
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            let page = self.store.records_after(&self.scope, after, 500).await?;
            let Some(last) = page.last() else {
                return Ok(out);
            };
            after = last.seq;
            for stored in &page {
                out.push(open::<AdminRecord>(&stored.envelope, &keys, &self.scope)?.doc);
            }
        }
    }

    /// The keys enrolled for gates that are in use: what the Ledger is given beside the
    /// Authority's chain, so it can take those gates' receipts.
    pub async fn gate_keys(&self) -> Result<Vec<PublicKey>> {
        let mut out = Vec::new();
        for enrolled in self.store.keys(&self.scope).await? {
            if enrolled.kind == EnrolKind::Gate
                && self.enrolable(EnrolKind::Gate, &enrolled.name).await?
            {
                out.push(enrolled.key);
            }
        }
        Ok(out)
    }

    /// The records after `acked`, as a shipment for the Ledger. `None` when there are none.
    /// The Authority's chain goes to the Ledger as a gate's does, and is witnessed with it.
    pub async fn shipment(&self, acked: u64, limit: usize) -> Result<Option<Shipment>> {
        let batch: Vec<Envelope> = self
            .store
            .records_after(&self.scope, acked, limit)
            .await?
            .into_iter()
            .map(|r| r.envelope)
            .collect();
        if batch.is_empty() {
            return Ok(None);
        }
        let nonce = random_id("ship")?;
        let request = seal(
            &ShipRequest {
                v: VERSION,
                tenant: self.scope.tenant.clone(),
                env: self.scope.env.clone(),
                gate: self.name.clone(),
                nonce: nonce.clone(),
                count: batch.len() as u64,
                batch: batch_digest(&batch),
            },
            &self.admin_key,
        )?;
        Ok(Some(Shipment {
            request,
            batch,
            nonce,
        }))
    }

    /// Check the Ledger's signed answer to `shipment` against this Authority's own chain:
    /// it is the Ledger's, for this chain and this request, and the record it says it holds
    /// last is the one this Authority wrote under that number. Returns that number.
    pub async fn check_answer(
        &self,
        shipment: &Shipment,
        ack: &Envelope,
        ledger_keys: &KeySet,
    ) -> Result<u64> {
        let ack = open::<IntakeAck>(ack, ledger_keys, &self.scope)?.doc;
        if ack.gate != self.name {
            return Err(gov_core::Error::WrongGate.into());
        }
        if ack.nonce != shipment.nonce {
            return Err(gov_core::Error::Replay.into());
        }
        let mine = match ack.seq {
            0 => None,
            seq => self
                .store
                .records_after(&self.scope, seq - 1, 1)
                .await?
                .into_iter()
                .find(|r| r.seq == seq)
                .map(|r| r.hash),
        };
        if mine != ack.hash {
            return Err(AuthorityError::LedgerDisagrees(ack.seq));
        }
        Ok(ack.seq)
    }
}
