//! Where the Ledger keeps things. Every call names the tenant and environment; there is no
//! way to read or write without them (spec §9).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use gov_core::receipt::ChainHead;
use gov_core::{Digest, Envelope, Id, PublicKey, Scope};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The chain's head moved between the check and the write. The caller reads again.
    #[error("the chain's head moved")]
    HeadMoved,
    #[error("storage: {0}")]
    Other(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// One receipt as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReceipt {
    pub seq: u64,
    pub hash: Digest,
    /// The time the gate wrote in the receipt.
    pub at: i64,
    /// The Ledger's own clock when the receipt arrived. Pruning goes by this, never by the
    /// gate's word (review finding GR-9).
    pub received_at: i64,
    pub envelope: Envelope,
}

/// One thing intake refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// When it was first refused.
    pub at: i64,
    /// How many times the same envelope arrived and was refused for the same reason, and
    /// when last. A refusal is stored once (review finding GR-74).
    pub seen: u64,
    pub last_at: i64,
    pub kid: Id,
    pub code: String,
    /// The receipt was taken into the chain all the same; this row is a note about it, not a
    /// refusal (a time that stepped back, review findings GR-42 and GR-47).
    pub taken: bool,
    pub envelope_hash: Digest,
    /// Kept only when the envelope's signature held.
    pub envelope: Option<Envelope>,
}

/// What a chain holds. A chain is of one kind, fixed by the first key enrolled for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainKind {
    /// A gate's receipts.
    #[default]
    Receipt,
    /// An Authority's admin records.
    Admin,
}

impl ChainKind {
    pub fn is_receipt(&self) -> bool {
        *self == ChainKind::Receipt
    }
}

#[async_trait]
pub trait Store: Send + Sync {
    /// Enrol a key for a chain of `kind`. Every store refuses a key anyone could forge for,
    /// a key id that is taken, a key already enrolled under another id, and a key of one
    /// kind for a chain that holds the other.
    async fn add_key(
        &self,
        scope: &Scope,
        chain: &Id,
        kind: ChainKind,
        key: &PublicKey,
    ) -> StoreResult<()>;
    /// Enrol a gate's receipt key.
    async fn add_gate_key(&self, scope: &Scope, gate: &Id, key: &PublicKey) -> StoreResult<()> {
        self.add_key(scope, gate, ChainKind::Receipt, key).await
    }
    /// Note what an Authority's record says of a gate key id. Written once: a second
    /// enrolment under the same id changes nothing, and an ended one stays ended.
    async fn note_enrolment(&self, scope: &Scope, kid: &Id, e: &Enrolment) -> StoreResult<()>;
    /// End the enrolments an Authority recorded for a gate, or the one of a key id.
    async fn end_enrolments(
        &self,
        scope: &Scope,
        authority: &Id,
        gate: Option<&Id>,
        kid: Option<&Id>,
    ) -> StoreResult<()>;
    async fn enrolment(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<Enrolment>>;
    /// Retire a chain's keys: one of them, or all. Nothing more is taken under a retired
    /// key; what was taken stays, and so does the key, for checking it.
    async fn retire_keys(&self, scope: &Scope, chain: &Id, kid: Option<&Id>) -> StoreResult<()>;
    async fn key_retired(&self, scope: &Scope, kid: &Id) -> StoreResult<bool>;
    /// What kind of document a chain holds, if it has any key.
    async fn chain_kind(&self, scope: &Scope, chain: &Id) -> StoreResult<Option<ChainKind>>;
    /// The gate a key id belongs to, and the key.
    async fn gate_key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<(Id, PublicKey)>>;
    /// Every gate key, by gate.
    async fn gate_keys(&self, scope: &Scope) -> StoreResult<BTreeMap<Id, Vec<PublicKey>>>;

    async fn head(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<ChainHead>>;
    /// Every gate that has a head, in order of gate id.
    async fn heads(&self, scope: &Scope) -> StoreResult<Vec<(Id, ChainHead)>>;
    /// Add `receipt` and move the head to it, only if the head's number is still
    /// `expected_head_seq` (0 for an empty chain). One transaction.
    async fn append(
        &self,
        scope: &Scope,
        gate: &Id,
        expected_head_seq: u64,
        receipt: &StoredReceipt,
    ) -> StoreResult<()>;
    async fn receipt(
        &self,
        scope: &Scope,
        gate: &Id,
        seq: u64,
    ) -> StoreResult<Option<StoredReceipt>>;
    /// A gate's stored receipts in order.
    async fn receipts(&self, scope: &Scope, gate: &Id) -> StoreResult<Vec<StoredReceipt>>;

    async fn record_refusal(&self, scope: &Scope, refusal: &Refusal) -> StoreResult<()>;
    async fn refusals(&self, scope: &Scope) -> StoreResult<Vec<Refusal>>;

    /// Fails if a checkpoint with that number exists.
    async fn add_checkpoint(
        &self,
        scope: &Scope,
        seq: u64,
        at: i64,
        envelope: &Envelope,
    ) -> StoreResult<()>;
    async fn last_checkpoint(&self, scope: &Scope) -> StoreResult<Option<(u64, Envelope)>>;

    /// The largest number, at most `max_seq`, such that it and every stored receipt before it
    /// arrived before `cutoff`; with its hash.
    async fn prunable_through(
        &self,
        scope: &Scope,
        gate: &Id,
        cutoff: i64,
        max_seq: u64,
    ) -> StoreResult<Option<(u64, Digest)>>;
    /// Delete a gate's receipts up to `through_seq` and keep `record` in their place. One
    /// transaction.
    async fn prune(
        &self,
        scope: &Scope,
        gate: &Id,
        through_seq: u64,
        record: &Envelope,
    ) -> StoreResult<()>;
    async fn pruned(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<(u64, Envelope)>>;
}

type Key = (Id, Id);

/// What an Authority's chain says of one gate key id (review finding GR-124).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolment {
    pub authority: Id,
    pub gate: Id,
    /// The digest of the key that was enrolled.
    pub digest: gov_core::Digest,
    /// The gate was disabled or the key revoked since.
    pub ended: bool,
}

#[derive(Default)]
struct Inner {
    enrolments: BTreeMap<(Key, Id), Enrolment>,
    gate_keys: BTreeMap<(Key, Id), (Id, PublicKey)>,
    kinds: BTreeMap<(Key, Id), ChainKind>,
    retired: std::collections::BTreeSet<(Key, Id)>,
    heads: BTreeMap<(Key, Id), ChainHead>,
    receipts: BTreeMap<(Key, Id, u64), StoredReceipt>,
    refusals: Vec<(Key, Refusal)>,
    checkpoints: BTreeMap<(Key, u64), Envelope>,
    pruned: BTreeMap<(Key, Id), (u64, Envelope)>,
}

/// A store in memory, for tests and for a gate's own tests later.
#[derive(Default)]
pub struct MemoryStore(Mutex<Inner>);

fn k(scope: &Scope) -> Key {
    (scope.tenant.clone(), scope.env.clone())
}

#[async_trait]
impl Store for MemoryStore {
    async fn add_key(
        &self,
        scope: &Scope,
        gate: &Id,
        kind: ChainKind,
        key: &PublicKey,
    ) -> StoreResult<()> {
        key.enrolable()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let mut s = self.0.lock().unwrap();
        if s.kinds
            .get(&(k(scope), gate.clone()))
            .is_some_and(|held| *held != kind)
        {
            return Err(StoreError::Other(
                "that chain holds the other kind of document".into(),
            ));
        }
        let slot = (k(scope), key.kid.clone());
        if s.gate_keys.contains_key(&slot) {
            return Err(StoreError::Other("that key id is already enrolled".into()));
        }
        // One key, one gate: a key under two ids could be disowned under either.
        let same = s.gate_keys.iter().any(|((scope_key, _), (_, held))| {
            *scope_key == k(scope) && held.bytes().ok() == key.bytes().ok()
        });
        if same {
            return Err(StoreError::Other("that key is already enrolled".into()));
        }
        s.gate_keys.insert(slot, (gate.clone(), key.clone()));
        s.kinds.insert((k(scope), gate.clone()), kind);
        Ok(())
    }

    async fn note_enrolment(&self, scope: &Scope, kid: &Id, e: &Enrolment) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        s.enrolments
            .entry((k(scope), kid.clone()))
            .or_insert_with(|| e.clone());
        Ok(())
    }

    async fn end_enrolments(
        &self,
        scope: &Scope,
        authority: &Id,
        gate: Option<&Id>,
        kid: Option<&Id>,
    ) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        for ((scope_key, held), e) in s.enrolments.iter_mut() {
            if *scope_key == k(scope)
                && &e.authority == authority
                && gate.is_none_or(|g| g == &e.gate)
                && kid.is_none_or(|one| one == held)
            {
                e.ended = true;
            }
        }
        Ok(())
    }

    async fn enrolment(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<Enrolment>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .enrolments
            .get(&(k(scope), kid.clone()))
            .cloned())
    }

    async fn retire_keys(&self, scope: &Scope, chain: &Id, kid: Option<&Id>) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        let gone: Vec<(Key, Id)> = s
            .gate_keys
            .iter()
            .filter(|((scope_key, held), (of, _))| {
                *scope_key == k(scope) && of == chain && kid.is_none_or(|one| one == held)
            })
            .map(|(slot, _)| slot.clone())
            .collect();
        s.retired.extend(gone);
        Ok(())
    }

    async fn key_retired(&self, scope: &Scope, kid: &Id) -> StoreResult<bool> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .retired
            .contains(&(k(scope), kid.clone())))
    }

    async fn chain_kind(&self, scope: &Scope, chain: &Id) -> StoreResult<Option<ChainKind>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .kinds
            .get(&(k(scope), chain.clone()))
            .copied())
    }

    async fn gate_key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<(Id, PublicKey)>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .gate_keys
            .get(&(k(scope), kid.clone()))
            .cloned())
    }

    async fn gate_keys(&self, scope: &Scope) -> StoreResult<BTreeMap<Id, Vec<PublicKey>>> {
        let mut out: BTreeMap<Id, Vec<PublicKey>> = BTreeMap::new();
        for ((key, _), (gate, public)) in &self.0.lock().unwrap().gate_keys {
            if *key == k(scope) {
                out.entry(gate.clone()).or_default().push(public.clone());
            }
        }
        Ok(out)
    }

    async fn head(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<ChainHead>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .heads
            .get(&(k(scope), gate.clone()))
            .cloned())
    }

    async fn heads(&self, scope: &Scope) -> StoreResult<Vec<(Id, ChainHead)>> {
        let s = self.0.lock().unwrap();
        Ok(s.heads
            .iter()
            .filter(|((key, _), _)| *key == k(scope))
            .map(|((_, gate), head)| (gate.clone(), head.clone()))
            .collect())
    }

    async fn append(
        &self,
        scope: &Scope,
        gate: &Id,
        expected_head_seq: u64,
        receipt: &StoredReceipt,
    ) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        let slot = (k(scope), gate.clone());
        if s.heads.get(&slot).map_or(0, |h| h.seq) != expected_head_seq {
            return Err(StoreError::HeadMoved);
        }
        s.receipts
            .insert((k(scope), gate.clone(), receipt.seq), receipt.clone());
        s.heads.insert(
            slot,
            ChainHead {
                seq: receipt.seq,
                hash: receipt.hash.clone(),
            },
        );
        Ok(())
    }

    async fn receipt(
        &self,
        scope: &Scope,
        gate: &Id,
        seq: u64,
    ) -> StoreResult<Option<StoredReceipt>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .receipts
            .get(&(k(scope), gate.clone(), seq))
            .cloned())
    }

    async fn receipts(&self, scope: &Scope, gate: &Id) -> StoreResult<Vec<StoredReceipt>> {
        let s = self.0.lock().unwrap();
        Ok(s.receipts
            .iter()
            .filter(|((key, g, _), _)| *key == k(scope) && g == gate)
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn record_refusal(&self, scope: &Scope, refusal: &Refusal) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        let key = k(scope);
        let again = s.refusals.iter_mut().find(|(at, r)| {
            *at == key && r.envelope_hash == refusal.envelope_hash && r.code == refusal.code
        });
        match again {
            Some((_, r)) => {
                r.seen += 1;
                r.last_at = refusal.at;
            }
            None => s.refusals.push((key, refusal.clone())),
        }
        Ok(())
    }

    async fn refusals(&self, scope: &Scope) -> StoreResult<Vec<Refusal>> {
        let s = self.0.lock().unwrap();
        Ok(s.refusals
            .iter()
            .filter(|(key, _)| *key == k(scope))
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn add_checkpoint(
        &self,
        scope: &Scope,
        seq: u64,
        _at: i64,
        envelope: &Envelope,
    ) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        if s.checkpoints.contains_key(&(k(scope), seq)) {
            return Err(StoreError::Other("that checkpoint exists".into()));
        }
        s.checkpoints.insert((k(scope), seq), envelope.clone());
        Ok(())
    }

    async fn last_checkpoint(&self, scope: &Scope) -> StoreResult<Option<(u64, Envelope)>> {
        let s = self.0.lock().unwrap();
        Ok(s.checkpoints
            .iter()
            .rev()
            .find(|((key, _), _)| *key == k(scope))
            .map(|((_, seq), env)| (*seq, env.clone())))
    }

    async fn prunable_through(
        &self,
        scope: &Scope,
        gate: &Id,
        cutoff: i64,
        max_seq: u64,
    ) -> StoreResult<Option<(u64, Digest)>> {
        let mut last = None;
        for r in self.receipts(scope, gate).await? {
            if r.seq > max_seq || r.received_at >= cutoff {
                break;
            }
            last = Some((r.seq, r.hash));
        }
        Ok(last)
    }

    async fn prune(
        &self,
        scope: &Scope,
        gate: &Id,
        through_seq: u64,
        record: &Envelope,
    ) -> StoreResult<()> {
        let mut s = self.0.lock().unwrap();
        let key = k(scope);
        s.receipts
            .retain(|(rk, g, seq), _| !(*rk == key && g == gate && *seq <= through_seq));
        s.pruned
            .insert((key, gate.clone()), (through_seq, record.clone()));
        Ok(())
    }

    async fn pruned(&self, scope: &Scope, gate: &Id) -> StoreResult<Option<(u64, Envelope)>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pruned
            .get(&(k(scope), gate.clone()))
            .cloned())
    }
}
