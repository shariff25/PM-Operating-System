//! What an Authority's admins did (spec §7.4, §7.6): a signed record of its own kind, numbered
//! and chained like receipts, and covered by the same witness.
//!
//! A receipt is about an agent's action on a system; its rules do not fit "Ana enrolled the
//! agent `refund-bot`", and bending them would weaken them. So an admin action has its own
//! document. Like a receipt it carries no free text: what was done is one of a closed list,
//! who and what it was done to are identifiers, and anything longer is carried as a digest.

use serde::{Deserialize, Serialize};

use crate::envelope::{document, Opened};
use crate::error::{Error, Result};
use crate::ids::{Digest, Id};
use crate::receipt::ChainHead;

/// What was done. A closed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminAction {
    /// A person was given a role in this installation.
    PersonAdded,
    /// A person left: their agents are suspended and those agents' mandates revoked.
    PersonOffboarded,
    /// An agent was named, with its owner, before it has a key.
    AgentRegistered,
    /// A gate was named, before it has a key.
    GateRegistered,
    /// A one-time code was made for an agent or a gate to enrol its key with.
    EnrolmentOffered,
    /// A code was cancelled after too many wrong guesses. Nobody decided it: the actor is
    /// the Authority itself.
    EnrolmentCancelled,
    /// An agent's or gate's key was enrolled.
    KeyEnrolled,
    AgentSuspended,
    AgentResumed,
    AgentDisabled,
    GateDisabled,
    /// A policy bundle was published.
    PolicyPublished,
    /// A system's mode (observe, shadow, enforce) was set.
    ModeSet,
    /// A mandate was issued. Recorded so that a mandate the witness never saw issued can be
    /// told from one it did (review finding GR-108).
    MandateIssued,
    /// An agent, a mandate, a countersignature or a key was revoked.
    Revoked,
    /// The key these records are signed with was replaced. The record is signed with the
    /// old key and carries the digest of the new one; every record after it is signed with
    /// the new key, and the old one is refused from here on (spec §7.6.3, point 9).
    AdminKeyReplaced,
}

/// The kind of thing an action was done to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Person,
    Agent,
    Gate,
    Key,
    Mandate,
    Countersignature,
    Policy,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: Id,
}

/// What kind of thing acted. People and agents are named from separate lists, so the same
/// id can be one of each: a record says which it means (review finding GR-154).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    #[default]
    Person,
    /// An agent, acting through a request signed with its enrolled key: a delegation.
    Agent,
}

impl ActorKind {
    fn is_person(&self) -> bool {
        *self == ActorKind::Person
    }
}

/// Who did it, and whether the Authority knew that or was told it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub id: Id,
    /// A person unless it says otherwise: left out for a person, so that a record written
    /// before this field existed reads as what it was.
    #[serde(default, skip_serializing_if = "ActorKind::is_person")]
    pub kind: ActorKind,
    /// True only when the person was signed in when they acted. Before sign-in exists (G4e)
    /// every record says false, as `claimed` does for an agent, so that none reads as a
    /// person's proven act (review finding GR-111).
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminRecord {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    /// The Authority whose chain this is.
    pub authority: Id,
    /// Rises by one from 1.
    pub seq: u64,
    /// The hash of this chain's previous record. Absent only for `seq` 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<Digest>,
    /// Seconds since the epoch, by the Authority's clock.
    pub at: i64,
    pub actor: Actor,
    pub action: AdminAction,
    pub subject: Subject,
    /// A second thing the action concerns: the owner of a registered agent, the agent or gate
    /// a key or a code is for, the agent a mandate is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<Subject>,
    /// The digest of what was set, where something was: a public key, a bundle, a mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Digest>,
}

document!(AdminRecord, "gov.admin.v1");

impl AdminRecord {
    /// The rules a record keeps whoever wrote it: what each action is done to, what else it
    /// names, and whether it carries a value.
    pub fn validate(&self) -> Result<()> {
        if self.seq == 0 || (self.seq == 1) != self.prev.is_none() {
            return Err(Error::Malformed("seq and prev disagree"));
        }
        use AdminAction as A;
        use SubjectKind as K;
        let about = self.about.as_ref().map(|s| s.kind);
        // (what it may be done to, what else it must name, whether a value is required)
        let (subjects, also, value): (&[K], &[Option<K>], bool) = match self.action {
            A::PersonAdded | A::PersonOffboarded => (&[K::Person], &[None], false),
            A::AgentRegistered => (&[K::Agent], &[Some(K::Person)], false),
            A::GateRegistered => (&[K::Gate], &[None], false),
            A::EnrolmentOffered | A::EnrolmentCancelled => (&[K::Agent, K::Gate], &[None], false),
            A::KeyEnrolled => (&[K::Key], &[Some(K::Agent), Some(K::Gate)], true),
            A::AgentSuspended | A::AgentResumed | A::AgentDisabled => (&[K::Agent], &[None], false),
            A::GateDisabled => (&[K::Gate], &[None], false),
            A::PolicyPublished => (&[K::Policy], &[None], true),
            A::ModeSet => (&[K::System], &[None], true),
            A::MandateIssued => (&[K::Mandate], &[Some(K::Agent)], true),
            A::AdminKeyReplaced => (&[K::Key], &[None], true),
            A::Revoked => (
                &[K::Agent, K::Mandate, K::Countersignature, K::Key],
                &[None],
                false,
            ),
        };
        if !subjects.contains(&self.subject.kind) {
            return Err(Error::Malformed(
                "the action is not done to that kind of thing",
            ));
        }
        if !also.contains(&about) {
            return Err(Error::Malformed("the action names the wrong second thing"));
        }
        if value != self.value.is_some() {
            return Err(Error::Malformed(
                "a value is carried exactly where one was set",
            ));
        }
        Ok(())
    }
}

/// Does `record` follow `head` (or start the chain, when there is none)? Returns the new
/// head. A gap, a repeat, a fork or a record that breaks its own rules is refused.
pub fn follow(head: Option<&ChainHead>, record: &Opened<AdminRecord>) -> Result<ChainHead> {
    record.doc.validate()?;
    let fits = match head {
        None => record.doc.seq == 1 && record.doc.prev.is_none(),
        Some(h) => {
            h.seq.checked_add(1) == Some(record.doc.seq)
                && record.doc.prev.as_ref() == Some(&h.hash)
        }
    };
    if !fits {
        return Err(Error::BrokenChain);
    }
    Ok(ChainHead {
        seq: record.doc.seq,
        hash: record.hash.clone(),
    })
}
