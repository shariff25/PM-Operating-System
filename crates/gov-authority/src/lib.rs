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
    Agent, Effect, EnrolledKey, Gate, MemoryStore, PendingCode, Person, Published, Role, Status,
    Store, StoreError, StoredRecord,
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
}

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
        }
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
