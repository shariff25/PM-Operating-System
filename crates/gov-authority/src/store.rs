//! What the Authority keeps: its registry, and the chain of records of what its admins did.
//!
//! **A record and its effect are written together or not at all** (spec §7.6, review finding
//! GR-108). `commit` takes the signed record and the changes it stands for; a store applies
//! both in one step, so there is never an effect with no record, and a change is visible
//! only once its record is durable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use gov_core::enrol::EnrolKind;
use gov_core::receipt::ChainHead;
use gov_core::{Digest, Envelope, Id, PublicKey, Scope};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The chain's head moved between the read and the write. The caller reads again.
    #[error("the chain's head moved")]
    HeadMoved,
    /// A key id, or a public key, that is enrolled already.
    #[error("that key is enrolled already")]
    KeyTaken,
    #[error("storage: {0}")]
    Other(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Governance's own roles (spec §7.2). Nothing is shared with PM OS's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Owner,
    Approver,
    Auditor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Suspended,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub id: Id,
    pub role: Role,
    /// False once the person is offboarded.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub id: Id,
    /// The person who answers for it. Every agent has one.
    pub owner: Id,
    pub labels: BTreeSet<Id>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub id: Id,
    pub status: Status,
}

/// A one-time code waiting to be used. Only its digest is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingCode {
    pub kind: EnrolKind,
    pub name: Id,
    pub digest: Digest,
    pub expires_at: i64,
    /// The admin who made it.
    pub by: Id,
    /// Wrong codes presented for this name since it was made.
    pub wrong: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledKey {
    pub kind: EnrolKind,
    /// The agent or gate the key belongs to.
    pub name: Id,
    pub key: PublicKey,
}

/// One change to the registry, applied with the record that stands for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    PutPerson(Person),
    PutAgent(Agent),
    PutGate(Gate),
    PutCode(PendingCode),
    DropCode(EnrolKind, Id),
    /// Enrol a key. Unlike the others this never replaces: if the key id, or the public key
    /// under any id, is enrolled already, the whole commit fails with
    /// [`StoreError::KeyTaken`] and nothing is written (review findings GR-114, GR-116).
    AddKey(EnrolledKey),
}

/// One admin record as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRecord {
    pub seq: u64,
    pub hash: Digest,
    pub at: i64,
    pub envelope: Envelope,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn head(&self, scope: &Scope) -> StoreResult<Option<ChainHead>>;
    /// Append `record` and apply `effects`, both or neither. `record` must follow the head:
    /// if the head is not at `record.seq - 1`, nothing is written and the answer is
    /// [`StoreError::HeadMoved`].
    async fn commit(
        &self,
        scope: &Scope,
        record: &StoredRecord,
        effects: &[Effect],
    ) -> StoreResult<()>;
    /// The records after `seq`, in order, at most `limit`.
    async fn records_after(
        &self,
        scope: &Scope,
        seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<StoredRecord>>;

    async fn any_person(&self, scope: &Scope) -> StoreResult<bool>;
    async fn person(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Person>>;
    async fn agent(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Agent>>;
    async fn agents_of(&self, scope: &Scope, owner: &Id) -> StoreResult<Vec<Agent>>;
    async fn gate(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Gate>>;
    async fn key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<EnrolledKey>>;
    /// Every enrolled key, in order of key id.
    async fn keys(&self, scope: &Scope) -> StoreResult<Vec<EnrolledKey>>;

    async fn code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<Option<PendingCode>>;
    /// A wrong code was presented for this name. Counts it and returns the count. Not an
    /// admin action and not recorded: nobody decided it.
    async fn count_wrong_code(&self, scope: &Scope, kind: EnrolKind, name: &Id)
        -> StoreResult<u32>;
    /// Take the pending code whose digest is `digest`: remove it and return it, in one step.
    /// Of two callers presenting the same code at once, one gets it and the other gets
    /// nothing, so a code is used once (review finding GR-114).
    async fn take_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
        digest: &Digest,
    ) -> StoreResult<Option<PendingCode>>;
}

type Key = (Id, Id);

fn k(scope: &Scope) -> Key {
    (scope.tenant.clone(), scope.env.clone())
}

#[derive(Default)]
struct State {
    records: BTreeMap<Key, Vec<StoredRecord>>,
    people: BTreeMap<(Key, Id), Person>,
    agents: BTreeMap<(Key, Id), Agent>,
    gates: BTreeMap<(Key, Id), Gate>,
    keys: BTreeMap<(Key, Id), EnrolledKey>,
    codes: BTreeMap<(Key, EnrolKind, Id), PendingCode>,
}

/// A store in memory, for tests and for the library's own use where nothing must outlive
/// the process.
#[derive(Default)]
pub struct MemoryStore(Mutex<State>);

#[async_trait]
impl Store for MemoryStore {
    async fn head(&self, scope: &Scope) -> StoreResult<Option<ChainHead>> {
        let s = self.0.lock().unwrap();
        Ok(s.records
            .get(&k(scope))
            .and_then(|r| r.last())
            .map(|r| ChainHead {
                seq: r.seq,
                hash: r.hash.clone(),
            }))
    }

    async fn commit(
        &self,
        scope: &Scope,
        record: &StoredRecord,
        effects: &[Effect],
    ) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        let key = k(scope);
        let head = s
            .records
            .get(&key)
            .and_then(|c| c.last())
            .map_or(0, |r| r.seq);
        if head + 1 != record.seq {
            return Err(StoreError::HeadMoved);
        }
        // A key is enrolled once: checked before anything is written.
        for effect in effects {
            if let Effect::AddKey(e) = effect {
                let taken = s.keys.iter().any(|((scope_key, kid), held)| {
                    *scope_key == key
                        && (kid == &e.key.kid || held.key.bytes().ok() == e.key.bytes().ok())
                });
                if taken {
                    return Err(StoreError::KeyTaken);
                }
            }
        }
        s.records
            .entry(key.clone())
            .or_default()
            .push(record.clone());
        for effect in effects {
            match effect.clone() {
                Effect::PutPerson(p) => {
                    s.people.insert((key.clone(), p.id.clone()), p);
                }
                Effect::PutAgent(a) => {
                    s.agents.insert((key.clone(), a.id.clone()), a);
                }
                Effect::PutGate(g) => {
                    s.gates.insert((key.clone(), g.id.clone()), g);
                }
                Effect::PutCode(c) => {
                    s.codes.insert((key.clone(), c.kind, c.name.clone()), c);
                }
                Effect::DropCode(kind, name) => {
                    s.codes.remove(&(key.clone(), kind, name));
                }
                Effect::AddKey(e) => {
                    s.keys.insert((key.clone(), e.key.kid.clone()), e);
                }
            }
        }
        Ok(())
    }

    async fn records_after(
        &self,
        scope: &Scope,
        seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<StoredRecord>> {
        let s = self.0.lock().unwrap();
        Ok(s.records
            .get(&k(scope))
            .map(|r| {
                r.iter()
                    .filter(|r| r.seq > seq)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn any_person(&self, scope: &Scope) -> StoreResult<bool> {
        let key = k(scope);
        Ok(self.0.lock().unwrap().people.keys().any(|(k, _)| *k == key))
    }

    async fn person(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Person>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .people
            .get(&(k(scope), id.clone()))
            .cloned())
    }

    async fn agent(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Agent>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .agents
            .get(&(k(scope), id.clone()))
            .cloned())
    }

    async fn agents_of(&self, scope: &Scope, owner: &Id) -> StoreResult<Vec<Agent>> {
        let key = k(scope);
        Ok(self
            .0
            .lock()
            .unwrap()
            .agents
            .iter()
            .filter(|((k, _), a)| *k == key && &a.owner == owner)
            .map(|(_, a)| a.clone())
            .collect())
    }

    async fn gate(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Gate>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .gates
            .get(&(k(scope), id.clone()))
            .cloned())
    }

    async fn key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<EnrolledKey>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys
            .get(&(k(scope), kid.clone()))
            .cloned())
    }

    async fn keys(&self, scope: &Scope) -> StoreResult<Vec<EnrolledKey>> {
        let key = k(scope);
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys
            .iter()
            .filter(|((k, _), _)| *k == key)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<Option<PendingCode>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .codes
            .get(&(k(scope), kind, name.clone()))
            .cloned())
    }

    async fn count_wrong_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<u32> {
        let mut s = self.0.lock().unwrap();
        Ok(s.codes
            .get_mut(&(k(scope), kind, name.clone()))
            .map_or(0, |c| {
                c.wrong += 1;
                c.wrong
            }))
    }

    async fn take_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
        digest: &Digest,
    ) -> StoreResult<Option<PendingCode>> {
        let mut s = self.0.lock().unwrap();
        let slot = (k(scope), kind, name.clone());
        if s.codes.get(&slot).is_some_and(|c| &c.digest == digest) {
            return Ok(s.codes.remove(&slot));
        }
        Ok(None)
    }
}
