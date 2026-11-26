//! Agent Governance, the Authority. Spec: `docs/agent-governance-spec.md` §7, and §7.6 for
//! how this slice is cut.
//!
//! Step G4a: the registry of people, agents and gates; enrolment of an agent's or a gate's
//! key by a one-time code; and the signed, chained record of everything an admin did. It is
//! a library. Nothing here is served on a listener, and there is no sign-in yet: **every
//! record made by this step says its actor is unverified** (review finding GR-111).

pub mod pg;
pub mod store;

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use gov_core::admin::{Actor, AdminAction, AdminRecord, Subject, SubjectKind};
use gov_core::enrol::{code_digest, new_code, pin, EnrolKind, EnrolRequest};
use gov_core::{open, seal, Digest, Envelope, Id, KeySet, PublicKey, Scope, SigningKey, VERSION};

pub use store::{
    Agent, Effect, EnrolledKey, Gate, MemoryStore, PendingCode, Person, Role, Status, Store,
    StoreError, StoredRecord,
};

/// How long a one-time code is good for, in seconds.
pub const CODE_SECS: i64 = 15 * 60;
/// Wrong codes for one name before its pending code is cancelled.
pub const WRONG_CODES: u32 = 5;

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
    /// A request to enrol was refused. It says no more: the caller may be anyone.
    #[error("refused")]
    Refused,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Core(#[from] gov_core::Error),
}

type Result<T> = std::result::Result<T, AuthorityError>;

/// What a successful enrolment answers: the Authority's root key, and the value that lets
/// the caller check it was handed the right one (see [`gov_core::enrol::pin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    pub root: PublicKey,
    pub pin: Digest,
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
}

/// What a record says was done: the action, what it was done to, the second thing it
/// names, and the digest of what was set.
struct Did {
    action: AdminAction,
    on: Subject,
    about: Option<Subject>,
    value: Option<Digest>,
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

impl<S: Store> Authority<S> {
    pub fn new(store: S, scope: Scope, name: Id, admin_key: SigningKey, root: PublicKey) -> Self {
        Authority {
            store,
            scope,
            name,
            admin_key,
            root,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// The public key admin records are signed with: what the Ledger and an auditor hold.
    pub fn admin_public(&self) -> PublicKey {
        self.admin_key.public()
    }

    /// Write one record and apply what it stands for, together. The record is built on the
    /// chain's head as read; if another writer moved the head first, read again and retry.
    async fn act(&self, actor: &Id, did: Did, effects: &[Effect], now: i64) -> Result<u64> {
        let Did {
            action,
            on,
            about,
            value,
        } = did;
        let keys = KeySet::new([self.admin_key.public()]);
        for _ in 0..8 {
            let head = self.store.head(&self.scope).await?;
            let record = AdminRecord {
                v: VERSION,
                tenant: self.scope.tenant.clone(),
                env: self.scope.env.clone(),
                authority: self.name.clone(),
                seq: head.as_ref().map_or(1, |h| h.seq + 1),
                prev: head.map(|h| h.hash),
                at: now,
                actor: Actor {
                    id: actor.clone(),
                    // Nobody is signed in before G4e.
                    verified: false,
                },
                action,
                subject: on.clone(),
                about: about.clone(),
                value: value.clone(),
            };
            record.validate()?;
            let envelope = seal(&record, &self.admin_key)?;
            let hash = open::<AdminRecord>(&envelope, &keys, &self.scope)?.hash;
            let stored = StoredRecord {
                seq: record.seq,
                hash,
                at: now,
                envelope,
            };
            match self.store.commit(&self.scope, &stored, effects).await {
                Ok(()) => return Ok(record.seq),
                Err(StoreError::HeadMoved) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::HeadMoved.into())
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
        if self.store.any_person(&self.scope).await? {
            return Err(AuthorityError::Exists("an admin"));
        }
        let person = Person {
            id: first_admin.clone(),
            role: Role::Admin,
            active: true,
        };
        self.act(
            first_admin,
            Did {
                action: AdminAction::PersonAdded,
                on: subject(SubjectKind::Person, first_admin),
                about: None,
                value: None,
            },
            &[Effect::PutPerson(person)],
            now,
        )
        .await
    }

    pub async fn add_person(&self, actor: &Id, person: &Id, role: Role, now: i64) -> Result<u64> {
        self.admin(actor).await?;
        if self.store.person(&self.scope, person).await?.is_some() {
            return Err(AuthorityError::Exists("person"));
        }
        let added = Person {
            id: person.clone(),
            role,
            active: true,
        };
        self.act(
            actor,
            Did {
                action: AdminAction::PersonAdded,
                on: subject(SubjectKind::Person, person),
                about: None,
                value: None,
            },
            &[Effect::PutPerson(added)],
            now,
        )
        .await
    }

    /// A person leaves. Each of their agents is suspended first, with a record each, and
    /// the person is marked last: stopped halfway, what remains is a person still listed
    /// whose agents are already stopped, never the other way round. (An agent is in any
    /// case usable only while its owner is active: see [`Authority::agent_usable`].)
    pub async fn offboard_person(&self, actor: &Id, person: &Id, now: i64) -> Result<u64> {
        self.admin(actor).await?;
        let Some(mut leaving) = self.store.person(&self.scope, person).await? else {
            return Err(AuthorityError::Unknown("person"));
        };
        if actor == person {
            // The last admin cannot remove themselves and leave nobody able to act.
            return Err(AuthorityError::NotAllowed);
        }
        for mut agent in self.store.agents_of(&self.scope, person).await? {
            if agent.status == Status::Active {
                agent.status = Status::Suspended;
                self.act(
                    actor,
                    Did {
                        action: AdminAction::AgentSuspended,
                        on: subject(SubjectKind::Agent, &agent.id),
                        about: None,
                        value: None,
                    },
                    &[Effect::PutAgent(agent.clone())],
                    now,
                )
                .await?;
            }
        }
        leaving.active = false;
        self.act(
            actor,
            Did {
                action: AdminAction::PersonOffboarded,
                on: subject(SubjectKind::Person, person),
                about: None,
                value: None,
            },
            &[Effect::PutPerson(leaving)],
            now,
        )
        .await
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
        self.admin(actor).await?;
        if self.store.agent(&self.scope, agent).await?.is_some() {
            return Err(AuthorityError::Exists("agent"));
        }
        match self.store.person(&self.scope, owner).await? {
            Some(p) if p.active => {}
            _ => return Err(AuthorityError::Unknown("owner")),
        }
        let registered = Agent {
            id: agent.clone(),
            owner: owner.clone(),
            labels,
            status: Status::Active,
        };
        self.act(
            actor,
            Did {
                action: AdminAction::AgentRegistered,
                on: subject(SubjectKind::Agent, agent),
                about: Some(subject(SubjectKind::Person, owner)),
                value: None,
            },
            &[Effect::PutAgent(registered)],
            now,
        )
        .await
    }

    pub async fn register_gate(&self, actor: &Id, gate: &Id, now: i64) -> Result<u64> {
        self.admin(actor).await?;
        if self.store.gate(&self.scope, gate).await?.is_some() {
            return Err(AuthorityError::Exists("gate"));
        }
        let registered = Gate {
            id: gate.clone(),
            status: Status::Active,
        };
        self.act(
            actor,
            Did {
                action: AdminAction::GateRegistered,
                on: subject(SubjectKind::Gate, gate),
                about: None,
                value: None,
            },
            &[Effect::PutGate(registered)],
            now,
        )
        .await
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
        self.admin(actor).await?;
        let Some(mut found) = self.store.agent(&self.scope, agent).await? else {
            return Err(AuthorityError::Unknown("agent"));
        };
        if !from.contains(&found.status) {
            return Err(AuthorityError::Unknown("agent in that state"));
        }
        found.status = to;
        self.act(
            actor,
            Did {
                action,
                on: subject(SubjectKind::Agent, agent),
                about: None,
                value: None,
            },
            &[Effect::PutAgent(found)],
            now,
        )
        .await
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
        self.set_agent(
            actor,
            agent,
            AdminAction::AgentDisabled,
            &from,
            Status::Disabled,
            now,
        )
        .await
    }

    pub async fn disable_gate(&self, actor: &Id, gate: &Id, now: i64) -> Result<u64> {
        self.admin(actor).await?;
        match self.store.gate(&self.scope, gate).await? {
            Some(g) if g.status != Status::Disabled => {}
            _ => return Err(AuthorityError::Unknown("gate")),
        }
        let disabled = Gate {
            id: gate.clone(),
            status: Status::Disabled,
        };
        self.act(
            actor,
            Did {
                action: AdminAction::GateDisabled,
                on: subject(SubjectKind::Gate, gate),
                about: None,
                value: None,
            },
            &[Effect::PutGate(disabled)],
            now,
        )
        .await
    }

    /// Whether an agent may act: it is active, and so is the person who answers for it.
    pub async fn agent_usable(&self, agent: &Id) -> Result<bool> {
        let Some(found) = self.store.agent(&self.scope, agent).await? else {
            return Ok(false);
        };
        let owner = self.store.person(&self.scope, &found.owner).await?;
        Ok(found.status == Status::Active && owner.is_some_and(|p| p.active))
    }

    /// Make a one-time code for a registered agent or gate to enrol its key with. The code
    /// is returned once and never stored: only its digest is. A code made earlier for the
    /// same name is replaced.
    pub async fn offer_enrolment(
        &self,
        actor: &Id,
        kind: EnrolKind,
        name: &Id,
        now: i64,
    ) -> Result<String> {
        self.admin(actor).await?;
        let registered = match kind {
            EnrolKind::Agent => self
                .store
                .agent(&self.scope, name)
                .await?
                .is_some_and(|a| a.status == Status::Active),
            EnrolKind::Gate => self
                .store
                .gate(&self.scope, name)
                .await?
                .is_some_and(|g| g.status == Status::Active),
        };
        if !registered {
            return Err(AuthorityError::Unknown("agent or gate to enrol"));
        }
        let code = new_code()?;
        let pending = PendingCode {
            kind,
            name: name.clone(),
            digest: code_digest(&code),
            expires_at: now + CODE_SECS,
            by: actor.clone(),
            wrong: 0,
        };
        self.act(
            actor,
            Did {
                action: AdminAction::EnrolmentOffered,
                on: subject(of(kind), name),
                about: None,
                value: None,
            },
            &[Effect::PutCode(pending)],
            now,
        )
        .await?;
        Ok(code)
    }

    /// Enrol a key. `code` is the code itself, as the admin gave it out; `request` is signed
    /// by the key being enrolled and names who is enrolling (spec §7.6, choice 3).
    ///
    /// The caller may be anyone, so every refusal looks the same. A wrong code is counted,
    /// and the fifth cancels the pending code. The right code is used up by the first
    /// request that presents it, whether or not the rest of that request holds.
    pub async fn enrol(&self, code: &str, request: &Envelope, now: i64) -> Result<Enrolled> {
        // Who the request says it is from, read before anything is believed, so that the
        // signature can be checked against the very key it asks to enrol.
        let claimed: EnrolRequest = URL_SAFE_NO_PAD
            .decode(&request.payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(AuthorityError::Refused)?;
        let (kind, name) = (claimed.kind, claimed.name.clone());
        let Some(pending) = self.store.code(&self.scope, kind, &name).await? else {
            return Err(AuthorityError::Refused);
        };
        if code_digest(code) != pending.digest {
            let wrong = self
                .store
                .count_wrong_code(&self.scope, kind, &name)
                .await?;
            if wrong >= WRONG_CODES {
                self.store.drop_code(&self.scope, kind, &name).await?;
            }
            return Err(AuthorityError::Refused);
        }
        // The code was presented: it is spent, whatever comes next.
        self.store.drop_code(&self.scope, kind, &name).await?;
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
        // A key anyone could forge for, or a key id already in use, is not enrolled.
        if asked.key.enrolable().is_err()
            || self.store.key(&self.scope, &asked.key.kid).await?.is_some()
        {
            return Err(AuthorityError::Refused);
        }
        let key_bytes = serde_json::to_vec(&asked.key).map_err(|_| AuthorityError::Refused)?;
        let enrolled = EnrolledKey {
            kind,
            name: name.clone(),
            key: asked.key.clone(),
        };
        self.act(
            &pending.by,
            Did {
                action: AdminAction::KeyEnrolled,
                on: subject(SubjectKind::Key, &asked.key.kid),
                about: Some(subject(of(kind), &name)),
                value: Some(Digest::of(&[b"gov.key.v1\0", &key_bytes])),
            },
            &[Effect::PutKey(enrolled)],
            now,
        )
        .await?;
        Ok(Enrolled {
            root: self.root.clone(),
            pin: pin(code, &self.root)?,
        })
    }
}
