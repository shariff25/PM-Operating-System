//! Agent Governance, the gate. Spec: `docs/agent-governance-spec.md` §5.
//!
//! Slice G3a: the part of a gate every mode shares. It signs a receipt for each call into its
//! own numbered chain, keeps each one on disk before going on, ships them to the Ledger, and
//! keeps them until a witnessed checkpoint covers them. In this slice it runs in **observe**
//! mode only: it holds no credential, decides nothing and blocks nothing, and every receipt
//! it writes says so. The front doors (the MCP gate and the shim) are slice G3b.

pub mod spool;

use async_trait::async_trait;
use gov_core::action::{CanonicalAction, Reversal};
use gov_core::lease::{Checkpoint, IntakeAck};
use gov_core::receipt::{
    ChainHead, Coverage, Decision, DecisionKind, Mode, Outcome, OutcomeStatus, Phase, Proof,
    Reason, Receipt, ReceiptAction, ReceiptAgent,
};
use gov_core::{open, seal, Envelope, Error, Id, KeySet, Scope, SigningKey, VERSION};

pub use spool::{FileSpool, Spool, SpoolError, SpoolState, Spooled};

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
}

/// How a gate reaches the Ledger. The network one comes with the listener (G3b).
#[async_trait]
pub trait LedgerLink: Send + Sync {
    /// Send receipts in order; get back the Ledger's signed statement of the chain it holds.
    async fn ship(&self, batch: &[Envelope]) -> Result<Envelope, String>;
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
        spool: S,
        now: i64,
    ) -> Result<Self, GateError> {
        let mut state = spool.state()?;
        let held = spool.held()?;
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
        for r in held.iter().filter(|r| r.open_intent) {
            let keys = KeySet::new([gate.key.public()]);
            let intent = open::<Receipt>(&r.envelope, &keys, &gate.config.scope)?.doc;
            gate.write_outcome(&intent, OutcomeStatus::Unknown, None, now)?;
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
    /// says (review findings GR-16 and GR-42).
    fn stamp(&self, now: i64) -> i64 {
        now.max(self.state.last_at)
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
        let receipt = Receipt {
            v: VERSION,
            tenant: self.config.scope.tenant.clone(),
            env: self.config.scope.env.clone(),
            gate: self.config.gate.clone(),
            seq: 0,
            prev: None,
            at: self.stamp(now),
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
        let receipt = Receipt {
            seq: 0,
            prev: None,
            at: self.stamp(now),
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
    /// against this gate's own chain. Returns how many receipts the Ledger now holds beyond
    /// what it held before. Nothing is removed from the spool here.
    pub async fn ship(&mut self, link: &dyn LedgerLink) -> Result<u64, GateError> {
        let pending: Vec<Spooled> = self
            .spool
            .held()?
            .into_iter()
            .filter(|r| r.seq > self.state.acked_through)
            .take(self.config.batch)
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        let batch: Vec<Envelope> = pending.iter().map(|r| r.envelope.clone()).collect();
        let answer = link.ship(&batch).await.map_err(GateError::Link)?;
        let ack = open::<IntakeAck>(&answer, &self.config.ledger_keys, &self.config.scope)?.doc;
        if ack.gate != self.config.gate {
            return Err(GateError::Core(Error::WrongGate));
        }
        if ack.seq <= self.state.acked_through {
            // The Ledger took nothing new. The receipts stay, to be sent again.
            return Ok(0);
        }
        // The Ledger says it holds the chain through `ack.seq`: that must be a receipt this
        // gate wrote, with the hash this gate has for it.
        let mine = pending.iter().find(|r| r.seq == ack.seq);
        if mine.is_none_or(|r| r.hash != ack.hash) {
            return Err(GateError::LedgerDisagrees(ack.seq));
        }
        let moved = ack.seq - self.state.acked_through;
        self.state.acked_through = ack.seq;
        self.spool.set_state(&self.state)?;
        Ok(moved)
    }

    /// A checkpoint read from the witness. If it covers this gate's chain to a receipt this
    /// gate wrote, with the same hash, the spool is trimmed to there. A checkpoint that names
    /// another hash for one of this gate's receipts is an alarm, and nothing is trimmed.
    pub fn covered_by(&mut self, checkpoint: &Envelope) -> Result<u64, GateError> {
        let cp = open::<Checkpoint>(checkpoint, &self.config.ledger_keys, &self.config.scope)?.doc;
        cp.validate()?;
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
        // An intent still waiting for its outcome stays, and so does everything after it.
        let keep_from = held
            .iter()
            .filter(|r| r.open_intent)
            .map(|r| r.seq)
            .min()
            .unwrap_or(u64::MAX);
        let through = head.seq.min(keep_from.saturating_sub(1));
        if through <= self.state.covered_through {
            return Ok(0);
        }
        let trimmed = through - self.state.covered_through;
        self.state.covered_through = through;
        self.spool.set_state(&self.state)?;
        self.spool.trim(through)?;
        Ok(trimmed)
    }
}
