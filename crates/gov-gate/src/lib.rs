//! Agent Governance, the gate. Spec: `docs/agent-governance-spec.md` §5.
//!
//! Slice G3a: the part of a gate every mode shares. It signs a receipt for each call into its
//! own numbered chain, keeps each one on disk before going on, ships them to the Ledger, and
//! keeps them until a checkpoint it has read from the witness itself covers them. In this
//! slice it runs in **observe** mode only: it holds no credential, decides nothing and blocks
//! nothing, and every receipt it writes says so. The front doors (the MCP gate and the shim)
//! are slice G3b.

pub mod forward;
pub mod link;
pub mod sessions;
pub mod spool;

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use gov_core::action::{CanonicalAction, Reversal};
use gov_core::lease::{batch_digest, Checkpoint, IntakeAck, ShipRequest};
use gov_core::receipt::{
    ChainHead, Coverage, Decision, DecisionKind, Mode, Outcome, OutcomeStatus, Phase, Proof,
    ProofFailure, Reason, Receipt, ReceiptAction, ReceiptAgent,
};
use gov_core::{
    open, random_id, seal, Digest, Envelope, Error, Id, KeySet, Scope, SigningKey, VERSION,
};

pub use spool::{
    DirWitnessReader, FileSpool, Spool, SpoolError, SpoolState, Spooled, WitnessRead, WitnessReader,
};

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// The spool has reached its limit. In observe mode the front door lets the call through
    /// unrecorded and counts it; in enforce mode this refuses the call.
    #[error("the spool is full")]
    SpoolFull,
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error("{0}")]
    Core(#[from] Error),
    /// The Ledger's answer, or a checkpoint, does not agree with what this gate wrote.
    #[error("the Ledger disagrees with this gate's chain at receipt {0}")]
    LedgerDisagrees(u64),
    #[error("the Ledger could not be reached: {0}")]
    Link(String),
    #[error("no such open intent: {0}")]
    NoSuchIntent(u64),
    /// The Ledger holds less of this gate's chain than it acknowledged before, and this gate
    /// no longer holds the receipts to send again.
    #[error("the Ledger lost receipts this gate no longer holds, back to receipt {0}")]
    LedgerLost(u64),
    #[error("the witness could not be read: {0}")]
    Witness(String),
}

/// What the Ledger sent back for a batch.
#[derive(Debug, Clone)]
pub struct LinkAnswer {
    /// The Ledger's signed statement of the chain it holds.
    pub ack: Envelope,
    /// The Ledger's reason for the first receipt it refused, if it refused any. Unsigned:
    /// it is there to be shown to an operator, not to be acted on.
    pub refused: Option<String>,
}

/// A batch ready to go to the Ledger: the gate's signed request and the receipts it covers.
#[derive(Debug, Clone)]
pub struct Shipment {
    pub request: Envelope,
    pub batch: Vec<Envelope>,
    nonce: Id,
}

/// What one round of trimming did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    /// Receipts removed from the spool.
    pub receipts: u64,
    /// Checkpoints in the witness, newer than the one used, that could not be read or do not
    /// verify. They did not stop the trim; someone should look at them.
    pub bad_checkpoints: Vec<u64>,
}

/// What one round of shipping did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shipped {
    /// The Ledger had already acknowledged everything this gate holds.
    NothingToSend,
    /// The Ledger now holds this many more receipts.
    Advanced(u64),
    /// Receipts were sent and the Ledger holds no more than before: it is refusing them, for
    /// the reason given if it gave one. A refused receipt ends this gate's chain at the
    /// Ledger until someone acts, so the front door shows it; it is not silent.
    NoProgress { refused: Option<String> },
    /// The Ledger holds less than it acknowledged before (restored from an older copy). The
    /// gate sends again from this receipt.
    LedgerBehind(u64),
}

/// How a gate reaches the Ledger. The network one comes with the listener (G3b).
#[async_trait]
pub trait LedgerLink: Send + Sync {
    /// Send receipts in order, under the gate's signed request for this batch; get back the
    /// Ledger's signed statement of the chain it holds, carrying the request's nonce.
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String>;
}

pub struct GateConfig {
    pub scope: Scope,
    pub gate: Id,
    /// The Ledger's public keys, for its acknowledgements and checkpoints.
    pub ledger_keys: KeySet,
    /// The most receipts the spool may hold that no witnessed checkpoint covers.
    pub spool_limit: usize,
    /// The most receipts in one batch to the Ledger.
    pub batch: usize,
    /// What every receipt carries as its policy version: the bundle the gate's connector
    /// map came from, or a word saying it came from no bundle. Absent where nothing loaded
    /// a policy at all.
    pub policy_version: Option<Id>,
}

/// Who made a call, and how the gate knows. There are two ways to make one, and no other:
/// from what the client said of itself, which is only ever a claim; and from what
/// [`sessions::Sessions::prove`] returned. No front door can write `proven` of its own accord
/// (review finding GR-101).
#[derive(Debug, Clone)]
pub struct Who {
    agent: Id,
    proof: Proof,
    failure: Option<ProofFailure>,
    mandates: Vec<Id>,
}

impl Who {
    /// A client that offered no proof: it is whoever it says it is, recorded as claimed.
    pub fn claimed(agent: Id) -> Self {
        Who {
            agent,
            proof: Proof::Claimed,
            failure: None,
            mandates: Vec::new(),
        }
    }

    /// A call that presented a session: proven, or failed with its reason. A failed proof
    /// under a session the gate does not know names nobody.
    pub fn from_session(proved: &sessions::Proved) -> Self {
        match proved {
            sessions::Proved::Proven(call) => Who {
                agent: call.who().agent.clone(),
                proof: Proof::Proven,
                failure: None,
                mandates: call.who().mandates.clone(),
            },
            sessions::Proved::Failed { who, why } => Who {
                agent: who.as_ref().map_or_else(
                    || Id::new("unknown-session").expect("an identifier"),
                    |w| w.agent.clone(),
                ),
                proof: Proof::Failed,
                failure: Some(*why),
                mandates: who.as_ref().map(|w| w.mandates.clone()).unwrap_or_default(),
            },
        }
    }
}

/// A time for a receipt, and where it came from. The two travel together, so that no
/// receipt carries the lease's time without saying so, or the machine's time while saying
/// it is the lease's (review finding GR-177).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub at: i64,
    /// The time is the Authority's: a held lease's time counted on by the gate's own clock.
    pub from_lease: bool,
}

impl When {
    /// This machine's clock.
    pub fn here(at: i64) -> Self {
        When {
            at,
            from_lease: false,
        }
    }
}

/// One observed call: who said they made it, and what it was.
pub struct Observed<'a> {
    pub who: Who,
    pub action: &'a CanonicalAction,
    pub reversal: Reversal,
    pub coverage: Coverage,
}

pub struct Gate<S> {
    config: GateConfig,
    key: SigningKey,

    spool: S,
    state: SpoolState,
    /// The number and hash of every receipt held, and which intents are still open. Built
    /// once when the gate opens and kept in step with the spool, so that no call has to
    /// read the spool to learn what is in it (review finding GR-82).
    index: BTreeMap<u64, Digest>,
    open_intents: BTreeSet<u64>,
}

impl<S: Spool> Gate<S> {
    /// Open a gate on its spool. Any intent left without an outcome by a crash gets an
    /// outcome of `unknown` now; the call is never made again (spec §4.5).
    pub fn open(
        config: GateConfig,
        key: SigningKey,
        spool: S,
        now: i64,
    ) -> Result<Self, GateError> {
        Self::open_after(config, key, &[], spool, now)
    }

    /// As [`Gate::open`], for a gate whose key has been replaced: `earlier` are the public
    /// halves of the keys it signed with before, under which its spool may still hold
    /// receipts (review finding GR-59). A receipt under any other key still stops it.
    pub fn open_after(
        config: GateConfig,
        key: SigningKey,
        earlier: &[gov_core::PublicKey],
        mut spool: S,
        now: i64,
    ) -> Result<Self, GateError> {
        let mut state = spool.state()?;
        // Receipts a checkpoint already covers, left behind by a crash between moving the
        // state and trimming (review finding GR-52).
        spool.trim(state.covered_through)?;
        let held = spool.held()?;
        // Nothing in the spool is trusted until it opens under this gate's own key with the
        // number and hash the spool says it has (review finding GR-51).
        let keys = KeySet::new(earlier.iter().cloned().chain([key.public()]));
        let mut docs = Vec::with_capacity(held.len());
        // Keys in the order they were used: none comes back once a later one has signed
        // (review finding GR-188).
        let order = |kid: &Id| {
            earlier
                .iter()
                .position(|k| &k.kid == kid)
                .unwrap_or(earlier.len())
        };
        let mut latest = 0;
        for r in &held {
            let opened = open::<Receipt>(&r.envelope, &keys, &config.scope)?;
            let used = order(&r.envelope.kid);
            if used < latest {
                return Err(GateError::Spool(SpoolError::Corrupt));
            }
            latest = used;
            if opened.doc.seq != r.seq || opened.hash != r.hash || opened.doc.gate != config.gate {
                return Err(GateError::Spool(SpoolError::Corrupt));
            }
            docs.push(opened.doc);
        }
        // A receipt written just before a crash, which the state file does not know of yet.
        for r in &held {
            if state.head.as_ref().is_none_or(|h| r.seq > h.seq) {
                state.head = Some(ChainHead {
                    seq: r.seq,
                    hash: r.hash.clone(),
                });
                state.last_at = state.last_at.max(r.at);
            }
        }
        let mut gate = Gate {
            config,
            key,
            spool,
            state,
            index: held.iter().map(|r| (r.seq, r.hash.clone())).collect(),
            open_intents: held
                .iter()
                .filter(|r| r.open_intent)
                .map(|r| r.seq)
                .collect(),
        };
        gate.spool.set_state(&gate.state)?;
        for (r, intent) in held.iter().zip(&docs).filter(|(r, _)| r.open_intent) {
            // A crash after the outcome was written and before the intent was marked
            // answered: the outcome is there. Mark the intent and write nothing more
            // (review finding GR-48).
            let answered = docs
                .iter()
                .any(|d| d.phase == Phase::Outcome && d.intent == Some(r.seq));
            if answered {
                gate.spool.close_intent(r.seq)?;
                gate.open_intents.remove(&r.seq);
            } else {
                // The gate has no lease yet when it opens: this is its machine's time.
                gate.write_outcome(intent, OutcomeStatus::Unknown, None, When::here(now))?;
            }
        }
        Ok(gate)
    }

    pub fn state(&self) -> &SpoolState {
        &self.state
    }

    /// Receipts no witnessed checkpoint covers yet.
    pub fn uncovered(&self) -> Result<usize, GateError> {
        Ok(self.index.len())
    }

    /// A call passed through with no receipt. Counted where it outlives a restart.
    pub fn note_not_recorded(&mut self) -> Result<(), GateError> {
        self.state.not_recorded += 1;
        Ok(self.spool.set_state(&self.state)?)
    }

    /// A time for the next receipt: never earlier than the last one's, whatever the clock
    /// says (review findings GR-16 and GR-42). The second value says the clock read earlier
    /// and was raised; the receipt records that (GR-53).
    fn stamp(&self, now: i64) -> (i64, bool) {
        (now.max(self.state.last_at), now < self.state.last_at)
    }

    fn append(&mut self, mut receipt: Receipt, open_intent: bool) -> Result<u64, GateError> {
        // The limit is checked when a call begins, with room kept for the outcome of every
        // call in flight: an outcome is never the receipt that does not fit.
        if open_intent && self.index.len() + self.open_intents.len() + 2 > self.config.spool_limit {
            return Err(GateError::SpoolFull);
        }
        let seq = self.state.head.as_ref().map_or(1, |h| h.seq + 1);
        receipt.seq = seq;
        receipt.prev = self.state.head.as_ref().map(|h| h.hash.clone());
        receipt.validate()?;
        let envelope = seal(&receipt, &self.key)?;
        let keys = KeySet::new([self.key.public()]);
        let hash = open::<Receipt>(&envelope, &keys, &self.config.scope)?.hash;
        let mut state = self.state.clone();
        state.head = Some(ChainHead {
            seq,
            hash: hash.clone(),
        });
        state.last_at = receipt.at;
        self.spool.append(
            &Spooled {
                seq,
                hash: hash.clone(),
                at: receipt.at,
                open_intent,
                envelope,
            },
            &state,
        )?;
        self.state = state;
        self.index.insert(seq, hash);
        if open_intent {
            self.open_intents.insert(seq);
        }
        Ok(seq)
    }

    /// Record that a call is about to pass through, before it does. Returns the intent's
    /// number, to give to [`Gate::finish`].
    pub fn begin(&mut self, call: &Observed<'_>, now: i64) -> Result<u64, GateError> {
        let policy = self.config.policy_version.clone();
        self.begin_under(call, When::here(now), policy)
    }

    /// As [`Gate::begin`], for a gate whose policy bundle can change while it runs: the
    /// receipt carries the version of the bundle the call was read under, which the caller
    /// took together with the map it read the call with. The outcome carries its intent's
    /// (spec §7.6.3, point 7).
    pub fn begin_under(
        &mut self,
        call: &Observed<'_>,
        when: When,
        policy_version: Option<Id>,
    ) -> Result<u64, GateError> {
        let (at, at_clamped) = self.stamp(when.at);
        let receipt = Receipt {
            session: None,
            cause: None,
            rule: None,
            labels: None,
            v: VERSION,
            tenant: self.config.scope.tenant.clone(),
            env: self.config.scope.env.clone(),
            gate: self.config.gate.clone(),
            seq: 0,
            prev: None,
            at,
            at_clamped,
            at_from_lease: when.from_lease,
            phase: Phase::Intent,
            mode: Mode::Observe,
            coverage: call.coverage,
            agent: ReceiptAgent {
                id: call.who.agent.clone(),
                proof: call.who.proof,
                failure: call.who.failure,
            },
            mandate: call.who.mandates.clone(),
            policy_version,
            action: ReceiptAction::from(call.action),
            reversal: call.reversal,
            before_digest: None,
            reverses: None,
            decision: Decision {
                kind: DecisionKind::Allow,
                reason: Reason::Observed,
            },
            would_decide: None,
            countersign: None,
            outcome: None,
            intent: None,
        };
        self.append(receipt, true)
    }

    /// Record what the system answered to the call of intent `intent`.
    pub fn finish(
        &mut self,
        intent: u64,
        status: OutcomeStatus,
        upstream_ref: Option<Id>,
        now: i64,
    ) -> Result<u64, GateError> {
        self.finish_at(intent, status, upstream_ref, When::here(now))
    }

    /// As [`Gate::finish`], with a time that says where it came from.
    pub fn finish_at(
        &mut self,
        intent: u64,
        status: OutcomeStatus,
        upstream_ref: Option<Id>,
        when: When,
    ) -> Result<u64, GateError> {
        if !self.open_intents.contains(&intent) {
            return Err(GateError::NoSuchIntent(intent));
        }
        let held = self
            .spool
            .get(intent)?
            .ok_or(GateError::NoSuchIntent(intent))?;
        // Signed with the key in use: an intent an earlier key left open was closed when
        // the gate opened.
        let keys = KeySet::new([self.key.public()]);
        let doc = open::<Receipt>(&held.envelope, &keys, &self.config.scope)?.doc;
        self.write_outcome(&doc, status, upstream_ref, when)
    }

    fn write_outcome(
        &mut self,
        intent: &Receipt,
        status: OutcomeStatus,
        upstream_ref: Option<Id>,
        when: When,
    ) -> Result<u64, GateError> {
        let (at, at_clamped) = self.stamp(when.at);
        let receipt = Receipt {
            session: None,
            cause: None,
            rule: None,
            labels: None,
            seq: 0,
            prev: None,
            at,
            at_clamped,
            at_from_lease: when.from_lease,
            phase: Phase::Outcome,
            outcome: Some(Outcome {
                status,
                upstream_ref,
                response_digest: None,
            }),
            intent: Some(intent.seq),
            before_digest: None,
            ..intent.clone()
        };
        let seq = self.append(receipt, false)?;
        self.spool.close_intent(intent.seq)?;
        self.open_intents.remove(&intent.seq);
        Ok(seq)
    }

    /// Send the Ledger what it has not acknowledged, in order, and check its signed answer
    /// against this gate's own chain. Nothing is removed from the spool here.
    ///
    /// This holds the gate for the whole exchange. A front door that must not make calls
    /// wait on the Ledger uses [`Gate::prepare_shipment`] and [`Gate::apply_answer`] instead,
    /// and sends in between without holding the gate (review finding GR-67).
    pub async fn ship(&mut self, link: &dyn LedgerLink) -> Result<Shipped, GateError> {
        let Some(shipment) = self.prepare_shipment()? else {
            return Ok(Shipped::NothingToSend);
        };
        let answer = link
            .ship(&shipment.request, &shipment.batch)
            .await
            .map_err(GateError::Link)?;
        self.apply_answer(&shipment, answer)
    }

    /// The next batch for the Ledger, under a request signed with the gate's own key, so the
    /// Ledger answers this gate and nobody else (review finding GR-55). `None` when the
    /// Ledger has acknowledged everything held.
    pub fn prepare_shipment(&mut self) -> Result<Option<Shipment>, GateError> {
        let mut batch = Vec::new();
        let next = self.state.acked_through.saturating_add(1);
        for seq in self
            .index
            .range(next..)
            .map(|(seq, _)| *seq)
            .take(self.config.batch)
        {
            let held = self.spool.get(seq)?.ok_or(SpoolError::Corrupt)?;
            batch.push(held.envelope);
        }
        if batch.is_empty() {
            return Ok(None);
        }
        let nonce = random_id("ship")?;
        let request = seal(
            &ShipRequest {
                v: VERSION,
                tenant: self.config.scope.tenant.clone(),
                env: self.config.scope.env.clone(),
                gate: self.config.gate.clone(),
                nonce: nonce.clone(),
                count: batch.len() as u64,
                batch: batch_digest(&batch),
            },
            &self.key,
        )?;
        Ok(Some(Shipment {
            request,
            batch,
            nonce,
        }))
    }

    /// Check the Ledger's answer to `shipment` against this gate's chain, and move on.
    pub fn apply_answer(
        &mut self,
        shipment: &Shipment,
        answer: LinkAnswer,
    ) -> Result<Shipped, GateError> {
        let nonce = &shipment.nonce;
        let ack = open::<IntakeAck>(&answer.ack, &self.config.ledger_keys, &self.config.scope)?.doc;
        if ack.gate != self.config.gate {
            return Err(GateError::Core(Error::WrongGate));
        }
        if &ack.nonce != nonce {
            // An answer to some other batch, played back (review finding GR-55).
            return Err(GateError::Core(Error::Replay));
        }
        // The Ledger says it holds the chain through `ack.seq`. That must be a receipt this
        // gate wrote, with the hash it has for it: looked up in the whole spool, so an honest
        // Ledger ahead of this batch is not taken for a liar (GR-50), and, for a receipt
        // already trimmed, in the checkpoint the spool was trimmed on (GR-58).
        let agrees = if ack.seq == 0 {
            ack.hash.is_none()
        } else if let Some(mine) = self.index.get(&ack.seq) {
            Some(mine) == ack.hash.as_ref()
        } else if ack.seq <= self.state.covered_through {
            match self.kept_head()? {
                Some((seq, hash)) if seq == ack.seq => Some(&hash) == ack.hash.as_ref(),
                // Trimmed, and not the receipt the kept checkpoint names: nothing to compare.
                _ => true,
            }
        } else {
            false
        };
        // The signed answer that raises an alarm is kept: it is the Ledger's own signature
        // on a statement about this gate's chain that this gate can show to be wrong.
        let alarm = |gate: &mut Self| -> Result<(), GateError> {
            let bytes = serde_json::to_vec(&answer.ack).map_err(|_| SpoolError::Corrupt)?;
            Ok(gate.spool.keep_alarm(ack.seq, &bytes)?)
        };
        if !agrees {
            alarm(self)?;
            return Err(GateError::LedgerDisagrees(ack.seq));
        }
        if ack.seq < self.state.acked_through {
            // It holds less than it said before. Send again from there, if this gate can.
            if ack.seq < self.state.covered_through {
                alarm(self)?;
                return Err(GateError::LedgerLost(ack.seq));
            }
            self.state.acked_through = ack.seq;
            self.spool.set_state(&self.state)?;
            return Ok(Shipped::LedgerBehind(ack.seq));
        }
        if ack.seq == self.state.acked_through {
            return Ok(Shipped::NoProgress {
                refused: answer.refused,
            });
        }
        let moved = ack.seq - self.state.acked_through;
        self.state.acked_through = ack.seq;
        self.spool.set_state(&self.state)?;
        Ok(Shipped::Advanced(moved))
    }

    /// This gate's head in the checkpoint the spool was last trimmed on.
    fn kept_head(&self) -> Result<Option<(u64, gov_core::Digest)>, GateError> {
        let Some(bytes) = self.spool.kept_checkpoint()? else {
            return Ok(None);
        };
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|_| GateError::Spool(SpoolError::Corrupt))?;
        let cp = open::<Checkpoint>(&envelope, &self.config.ledger_keys, &self.config.scope)?.doc;
        Ok(cp
            .heads
            .into_iter()
            .find(|h| h.gate == self.config.gate)
            .map(|h| (h.seq, h.hash)))
    }

    /// Read the witness and trim the spool to what it covers (review finding GR-49).
    ///
    /// The gate reads the witness store itself: a checkpoint is believed only if it is there,
    /// signed by the Ledger, and names the hash this gate has for the receipt it covers.
    /// Never trimmed: anything the Ledger has not acknowledged, an intent still waiting for
    /// its outcome, or anything after one. The checkpoint trimmed on is kept with the spool.
    ///
    /// The newest checkpoint that verifies is used; newer ones that do not are reported, and
    /// do not stop the trim (GR-57). A checkpoint that names another hash for one of this
    /// gate's receipts, or that leaves out or shortens a chain an earlier checkpoint covered,
    /// is an alarm, and nothing is trimmed (GR-56).
    pub fn trim_to_witness(&mut self, witness: &dyn WitnessReader) -> Result<Trimmed, GateError> {
        let read = WitnessRead::from(witness).map_err(GateError::Witness)?;
        self.trim_on(&read)
    }

    /// Trim on what was read from the witness. A front door reads the witness first, without
    /// holding the gate, and calls this with the result (review finding GR-82).
    pub fn trim_on(&mut self, read: &WitnessRead) -> Result<Trimmed, GateError> {
        let all = &read.0;
        let mut bad_checkpoints = Vec::new();
        let mut newest = None;
        for (file_seq, bytes) in all.iter().rev() {
            let verified = serde_json::from_slice::<Envelope>(bytes)
                .ok()
                .and_then(|e| {
                    open::<Checkpoint>(&e, &self.config.ledger_keys, &self.config.scope).ok()
                })
                .map(|o| o.doc)
                .filter(|cp| cp.seq == *file_seq && cp.validate().is_ok());
            match verified {
                Some(cp) => {
                    newest = Some((cp, bytes));
                    break;
                }
                None => bad_checkpoints.push(*file_seq),
            }
        }
        let none = |bad_checkpoints| Trimmed {
            receipts: 0,
            bad_checkpoints,
        };
        let Some((cp, bytes)) = newest else {
            return Ok(none(bad_checkpoints));
        };
        let head = cp.heads.iter().find(|h| h.gate == self.config.gate);
        // This gate's chain was covered before. The witness now shows it shorter, or not at
        // all: the gate is the party best placed to notice.
        if self.state.covered_through > 0 && head.is_none_or(|h| h.seq < self.state.covered_through)
        {
            return Err(GateError::LedgerDisagrees(self.state.covered_through));
        }
        let Some(head) = head else {
            return Ok(none(bad_checkpoints));
        };
        if head.seq == self.state.covered_through {
            return Ok(none(bad_checkpoints));
        }
        if self.index.get(&head.seq) != Some(&head.hash) {
            return Err(GateError::LedgerDisagrees(head.seq));
        }
        let keep_from = self.open_intents.first().copied().unwrap_or(u64::MAX);
        let through = head
            .seq
            .min(self.state.acked_through)
            .min(keep_from.saturating_sub(1));
        if through <= self.state.covered_through {
            return Ok(none(bad_checkpoints));
        }
        let receipts = through - self.state.covered_through;
        self.spool.keep_checkpoint(bytes)?;
        self.state.covered_through = through;
        self.spool.set_state(&self.state)?;
        let gone: Vec<u64> = self.index.range(..=through).map(|(seq, _)| *seq).collect();
        for seq in gone {
            self.spool.remove(seq)?;
            self.index.remove(&seq);
        }
        Ok(Trimmed {
            receipts,
            bad_checkpoints,
        })
    }
}
