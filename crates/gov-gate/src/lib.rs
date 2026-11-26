//! Agent Governance, the gate. Spec: `docs/agent-governance-spec.md` §5.
//!
//! Slice G3a: the part of a gate every mode shares. It signs a receipt for each call into its
//! own numbered chain, keeps each one on disk before going on, ships them to the Ledger, and
//! keeps them until a checkpoint it has read from the witness itself covers them. In this
//! slice it runs in **observe** mode only: it holds no credential, decides nothing and blocks
//! nothing, and every receipt it writes says so. The front doors (the MCP gate and the shim)
//! are slice G3b.

pub mod spool;

use async_trait::async_trait;
use gov_core::action::{CanonicalAction, Reversal};
use gov_core::lease::{Checkpoint, IntakeAck};
use gov_core::receipt::{
    ChainHead, Coverage, Decision, DecisionKind, Mode, Outcome, OutcomeStatus, Phase, Proof,
    Reason, Receipt, ReceiptAction, ReceiptAgent,
};
use gov_core::{open, random_id, seal, Envelope, Error, Id, KeySet, Scope, SigningKey, VERSION};

pub use spool::{
    DirWitnessReader, FileSpool, Spool, SpoolError, SpoolState, Spooled, WitnessReader,
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

/// What one round of shipping did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shipped {
    /// The Ledger had already acknowledged everything this gate holds.
    NothingToSend,
    /// The Ledger now holds this many more receipts.
    Advanced(u64),
    /// Receipts were sent and the Ledger holds no more than before: it is refusing them. The
    /// front door counts this and shows it, so a spool filling behind it is not silent.
    NoProgress,
    /// The Ledger holds less than it acknowledged before (restored from an older copy). The
    /// gate sends again from this receipt.
    LedgerBehind(u64),
}

/// How a gate reaches the Ledger. The network one comes with the listener (G3b).
#[async_trait]
pub trait LedgerLink: Send + Sync {
    /// Send receipts in order, with a nonce; get back the Ledger's signed statement of the
    /// chain it holds, carrying that nonce.
    async fn ship(&self, batch: &[Envelope], nonce: &Id) -> Result<Envelope, String>;
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
}

/// One observed call: who said they made it, and what it was.
pub struct Observed<'a> {
    /// In observe mode without the shim, an agent's identity is only what it says (§5.3).
    pub agent: &'a Id,
    pub proof: Proof,
    pub action: &'a CanonicalAction,
    pub reversal: Reversal,
    pub coverage: Coverage,
}

pub struct Gate<S> {
    config: GateConfig,
    key: SigningKey,
    spool: S,
    state: SpoolState,
}

impl<S: Spool> Gate<S> {
    /// Open a gate on its spool. Any intent left without an outcome by a crash gets an
    /// outcome of `unknown` now; the call is never made again (spec §4.5).
    pub fn open(
        config: GateConfig,
        key: SigningKey,
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
        let keys = KeySet::new([key.public()]);
        let mut docs = Vec::with_capacity(held.len());
        for r in &held {
            let opened = open::<Receipt>(&r.envelope, &keys, &config.scope)?;
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
            } else {
                gate.write_outcome(intent, OutcomeStatus::Unknown, None, now)?;
            }
        }
        Ok(gate)
    }

    pub fn state(&self) -> &SpoolState {
        &self.state
    }

    /// Receipts no witnessed checkpoint covers yet.
    pub fn uncovered(&self) -> Result<usize, GateError> {
        Ok(self.spool.held()?.len())
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
        if open_intent {
            let held = self.spool.held()?;
            let in_flight = held.iter().filter(|r| r.open_intent).count();
            if held.len() + in_flight + 2 > self.config.spool_limit {
                return Err(GateError::SpoolFull);
            }
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
                hash,
                at: receipt.at,
                open_intent,
                envelope,
            },
            &state,
        )?;
        self.state = state;
        Ok(seq)
    }

    /// Record that a call is about to pass through, before it does. Returns the intent's
    /// number, to give to [`Gate::finish`].
    pub fn begin(&mut self, call: &Observed<'_>, now: i64) -> Result<u64, GateError> {
        let (at, at_clamped) = self.stamp(now);
        let receipt = Receipt {
            v: VERSION,
            tenant: self.config.scope.tenant.clone(),
            env: self.config.scope.env.clone(),
            gate: self.config.gate.clone(),
            seq: 0,
            prev: None,
            at,
            at_clamped,
            phase: Phase::Intent,
            mode: Mode::Observe,
            coverage: call.coverage,
            agent: ReceiptAgent {
                id: call.agent.clone(),
                proof: call.proof,
            },
            mandate: Vec::new(),
            policy_version: None,
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
        let held = self
            .spool
            .held()?
            .into_iter()
            .find(|r| r.seq == intent && r.open_intent)
            .ok_or(GateError::NoSuchIntent(intent))?;
        let keys = KeySet::new([self.key.public()]);
        let doc = open::<Receipt>(&held.envelope, &keys, &self.config.scope)?.doc;
        self.write_outcome(&doc, status, upstream_ref, now)
    }

    fn write_outcome(
        &mut self,
        intent: &Receipt,
        status: OutcomeStatus,
        upstream_ref: Option<Id>,
        now: i64,
    ) -> Result<u64, GateError> {
        let (at, at_clamped) = self.stamp(now);
        let receipt = Receipt {
            seq: 0,
            prev: None,
            at,
            at_clamped,
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
        Ok(seq)
    }

    /// Send the Ledger what it has not acknowledged, in order, and check its signed answer
    /// against this gate's own chain. Nothing is removed from the spool here.
    pub async fn ship(&mut self, link: &dyn LedgerLink) -> Result<Shipped, GateError> {
        let held = self.spool.held()?;
        let batch: Vec<Envelope> = held
            .iter()
            .filter(|r| r.seq > self.state.acked_through)
            .take(self.config.batch)
            .map(|r| r.envelope.clone())
            .collect();
        if batch.is_empty() {
            return Ok(Shipped::NothingToSend);
        }
        let nonce = random_id("ship")?;
        let answer = link.ship(&batch, &nonce).await.map_err(GateError::Link)?;
        let ack = open::<IntakeAck>(&answer, &self.config.ledger_keys, &self.config.scope)?.doc;
        if ack.gate != self.config.gate {
            return Err(GateError::Core(Error::WrongGate));
        }
        if ack.nonce != nonce {
            // An answer to some other batch, played back (review finding GR-55).
            return Err(GateError::Core(Error::Replay));
        }
        // The Ledger says it holds the chain through `ack.seq`. Where this gate still holds
        // that receipt, the hash must be the one it wrote. It is looked up in the whole
        // spool, so an honest Ledger that is ahead of this batch is not taken for a liar
        // (review finding GR-50).
        if ack.seq > self.state.covered_through {
            let mine = held.iter().find(|r| r.seq == ack.seq);
            if mine.is_none_or(|r| Some(&r.hash) != ack.hash.as_ref()) {
                return Err(GateError::LedgerDisagrees(ack.seq));
            }
        }
        if ack.seq < self.state.acked_through {
            // It holds less than it said before. Send again from there, if this gate can.
            if ack.seq < self.state.covered_through {
                return Err(GateError::LedgerLost(ack.seq));
            }
            self.state.acked_through = ack.seq;
            self.spool.set_state(&self.state)?;
            return Ok(Shipped::LedgerBehind(ack.seq));
        }
        if ack.seq == self.state.acked_through {
            return Ok(Shipped::NoProgress);
        }
        let moved = ack.seq - self.state.acked_through;
        self.state.acked_through = ack.seq;
        self.spool.set_state(&self.state)?;
        Ok(Shipped::Advanced(moved))
    }

    /// Read the witness and trim the spool to what it covers (review finding GR-49).
    ///
    /// The gate reads the witness store itself: a checkpoint is believed only if it is there,
    /// signed by the Ledger, and names the hash this gate has for the receipt it covers.
    /// Never trimmed: anything the Ledger has not acknowledged, an intent still waiting for
    /// its outcome, or anything after one. A checkpoint that names another hash is an alarm
    /// and nothing is trimmed. The checkpoint trimmed on is kept with the spool.
    pub fn trim_to_witness(&mut self, witness: &dyn WitnessReader) -> Result<u64, GateError> {
        let all = witness.checkpoints().map_err(GateError::Witness)?;
        let Some((file_seq, bytes)) = all.last() else {
            return Ok(0);
        };
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| GateError::Core(Error::BadEnvelope))?;
        let cp = open::<Checkpoint>(&envelope, &self.config.ledger_keys, &self.config.scope)?.doc;
        cp.validate()?;
        if cp.seq != *file_seq {
            return Err(GateError::Core(Error::Malformed("checkpoint number")));
        }
        let Some(head) = cp.heads.iter().find(|h| h.gate == self.config.gate) else {
            return Ok(0);
        };
        if head.seq <= self.state.covered_through {
            return Ok(0);
        }
        let held = self.spool.held()?;
        let mine = held.iter().find(|r| r.seq == head.seq);
        if mine.is_none_or(|r| r.hash != head.hash) {
            return Err(GateError::LedgerDisagrees(head.seq));
        }
        let keep_from = held
            .iter()
            .filter(|r| r.open_intent)
            .map(|r| r.seq)
            .min()
            .unwrap_or(u64::MAX);
        let through = head
            .seq
            .min(self.state.acked_through)
            .min(keep_from.saturating_sub(1));
        if through <= self.state.covered_through {
            return Ok(0);
        }
        let trimmed = through - self.state.covered_through;
        self.spool.keep_checkpoint(bytes)?;
        self.state.covered_through = through;
        self.spool.set_state(&self.state)?;
        self.spool.trim(through)?;
        Ok(trimmed)
    }
}
