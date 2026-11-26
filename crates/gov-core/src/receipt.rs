//! The gate's signed record of one decision and its outcome (spec §4.5).
//!
//! A receipt holds identifiers, digests and closed lists only. It has no argument value, no
//! response body and no field that can carry free text (threat T17).

use serde::{Deserialize, Serialize};

use crate::action::{CanonicalAction, Kind, Reversal};
use crate::countersign::Class;
use crate::envelope::{document, Opened};
use crate::error::{Error, Result};
use crate::ids::{Digest, Id, ResourcePath};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// A refusal, or a wait for a countersignature: nothing ran.
    Decision,
    /// Written and synced before the action is sent.
    Intent,
    /// What the system answered, or `unknown` after a crash.
    Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Observe,
    Shadow,
    Enforce,
}

impl Mode {
    /// Observe and shadow protect nothing, and nothing may count them as protection (T16).
    pub fn enforced(self) -> bool {
        matches!(self, Mode::Enforce)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// The gate holds the only credential.
    Custody,
    /// Calls pass through the gate, but another credential exists.
    InLine,
    /// The agent only tells the gate afterwards.
    Reported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proof {
    /// The request was signed by the agent's enrolled key.
    Proven,
    /// The agent only said who it was (a stock client in observe mode).
    Claimed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    Refuse,
    WaitCountersign,
}

/// Why. A closed list: a receipt never carries a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Ok,
    Observed,
    BadRequestSignature,
    Replay,
    MandateInvalid,
    MandateExpired,
    Revoked,
    UnmappedTool,
    ToolChanged,
    LeaseStale,
    PolicyRefuses,
    NoPolicyMatch,
    NotInMandate,
    LimitSpent,
    CountersignRequired,
    CountersignInvalid,
    Budget,
    DigestChanged,
    VersionChanged,
    SpoolFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub kind: DecisionKind,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAgent {
    pub id: Id,
    pub proof: Proof,
}

/// The canonical action and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAction {
    pub system: Id,
    pub operation: Id,
    pub kind: Kind,
    pub resource: ResourcePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_version: Option<Id>,
    pub args_digest: Digest,
    pub digest: Digest,
}

impl From<&CanonicalAction> for ReceiptAction {
    fn from(a: &CanonicalAction) -> Self {
        ReceiptAction {
            system: a.system.clone(),
            operation: a.operation.clone(),
            kind: a.kind,
            resource: a.resource.clone(),
            resource_version: a.resource_version.clone(),
            args_digest: a.args_digest.clone(),
            digest: a.digest(),
        }
    }
}

impl ReceiptAction {
    fn consistent(&self) -> bool {
        let action = CanonicalAction {
            system: self.system.clone(),
            operation: self.operation.clone(),
            kind: self.kind,
            resource: self.resource.clone(),
            resource_version: self.resource_version.clone(),
            args_digest: self.args_digest.clone(),
        };
        action.digest() == self.digest && self.resource.system() == self.system.as_str()
    }
}

/// The receipt an undo puts back: which gate's chain, which number, and its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRef {
    pub gate: Id,
    pub seq: u64,
    pub hash: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountersignRef {
    pub id: Id,
    pub class: Class,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Ok,
    Failed,
    /// The gate stopped between intent and outcome. The action may or may not have happened,
    /// and is never run again by the gate.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub status: OutcomeStatus,
    /// The system's own id for what was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_digest: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub gate: Id,
    /// Rises by one from 1.
    pub seq: u64,
    /// The hash of this gate's previous receipt. Absent only for `seq` 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<Digest>,
    pub at: i64,
    pub phase: Phase,
    pub mode: Mode,
    pub coverage: Coverage,
    pub agent: ReceiptAgent,
    /// Mandate ids, the presented one first. Empty in observe mode without a mandate.
    #[serde(default)]
    pub mandate: Vec<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<Id>,
    pub action: ReceiptAction,
    /// Whether the operation can be put back, from the connector map. Every receipt says.
    pub reversal: Reversal,
    /// The digest of the target's values before the write, which the gate keeps sealed for
    /// the undo window. Only on the intent of an operation that can be put back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_digest: Option<Digest>,
    /// Set when this action is the undo of an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverses: Option<ReceiptRef>,
    pub decision: Decision,
    /// Shadow mode only: what enforce mode would have decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_decide: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countersign: Option<CountersignRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

document!(Receipt, "gov.receipt.v1");

impl Receipt {
    /// The rules a receipt must keep whoever wrote it.
    pub fn validate(&self) -> Result<()> {
        if self.seq == 0 || (self.seq == 1) != self.prev.is_none() {
            return Err(Error::Malformed("seq and prev disagree"));
        }
        if !self.action.consistent() {
            return Err(Error::WrongDigest);
        }
        if (self.phase == Phase::Outcome) != self.outcome.is_some() {
            return Err(Error::Malformed("an outcome belongs to the outcome phase"));
        }
        if self.would_decide.is_some() != (self.mode == Mode::Shadow) {
            return Err(Error::Malformed("would_decide belongs to shadow mode"));
        }
        if self.phase != Phase::Decision && self.decision.kind != DecisionKind::Allow {
            return Err(Error::Malformed(
                "only an allowed action has an intent or an outcome",
            ));
        }
        if self.phase == Phase::Decision
            && self.mode == Mode::Enforce
            && self.decision.kind == DecisionKind::Allow
        {
            return Err(Error::Malformed(
                "an allowed action is an intent, not a decision",
            ));
        }
        if self.mode == Mode::Enforce && self.agent.proof != Proof::Proven {
            return Err(Error::Malformed("enforce mode needs a proven agent"));
        }
        if self.before_digest.is_some()
            && (self.phase != Phase::Intent
                || !self.reversal.reversible()
                || self.action.kind == Kind::Read)
        {
            return Err(Error::Malformed(
                "a before-digest belongs to the intent of a write that can be put back",
            ));
        }
        if self.reverses.is_some() && self.action.kind == Kind::Read {
            return Err(Error::Malformed("a read undoes nothing"));
        }
        if self.at < 0 {
            return Err(Error::Malformed("a receipt's time is before the epoch"));
        }
        if self.mode == Mode::Enforce
            && self.decision.kind == DecisionKind::Allow
            && (self.mandate.is_empty() || self.policy_version.is_none())
        {
            return Err(Error::Malformed(
                "an enforced allow names its mandate and policy",
            ));
        }
        if self.mode == Mode::Enforce && self.coverage == Coverage::Reported {
            return Err(Error::Malformed("a reported action cannot be enforced"));
        }
        Ok(())
    }
}

/// The last receipt of a gate's chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainHead {
    pub seq: u64,
    pub hash: Digest,
}

/// Does `receipt` follow `head` (or start the chain, when there is none)? Returns the new
/// head. A gap, a repeat, a fork or a receipt that breaks its own rules is refused (T10).
pub fn follow(head: Option<&ChainHead>, receipt: &Opened<Receipt>) -> Result<ChainHead> {
    receipt.doc.validate()?;
    let fits = match head {
        None => receipt.doc.seq == 1 && receipt.doc.prev.is_none(),
        Some(h) => {
            h.seq.checked_add(1) == Some(receipt.doc.seq)
                && receipt.doc.prev.as_ref() == Some(&h.hash)
        }
    };
    if !fits {
        return Err(Error::BrokenChain);
    }
    Ok(ChainHead {
        seq: receipt.doc.seq,
        hash: receipt.hash.clone(),
    })
}
