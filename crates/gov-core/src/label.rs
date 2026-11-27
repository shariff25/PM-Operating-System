//! Labels: how confidential something is, and whether it can be trusted to steer an agent
//! (access governance spec, CG-1, CG-4 and CG-5).
//!
//! Slice CG0a adds the words and where they are written: on a receipt, and in a connector
//! map. Nothing reads them to decide anything yet.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::Digest;
use crate::receipt::ReceiptRef;

/// Who may see something. In order: each is more closely held than the one before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidentiality {
    Public,
    Internal,
    Confidential,
    Restricted,
}

/// Whether text may have been written by someone who means to steer the agent that reads it.
/// In order: untrusted is the worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    Trusted,
    Untrusted,
}

/// A pair of the two. A label only rises: see [`Label::join`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub confidentiality: Confidentiality,
    pub integrity: Integrity,
}

impl Label {
    /// Where a mandate starts: it has read nothing.
    pub const CLEAN: Label = Label {
        confidentiality: Confidentiality::Public,
        integrity: Integrity::Trusted,
    };

    /// What is assumed of anything nobody labelled.
    pub const WORST: Label = Label {
        confidentiality: Confidentiality::Restricted,
        integrity: Integrity::Untrusted,
    };

    /// The label of two things read together: the more closely held, the less trusted. The
    /// order of the two does not matter, and joining a label with itself changes nothing,
    /// so gates that learn of the same reads in different orders agree (GR-145).
    #[must_use]
    pub fn join(self, other: Label) -> Label {
        Label {
            confidentiality: self.confidentiality.max(other.confidentiality),
            integrity: self.integrity.max(other.integrity),
        }
    }

    /// Is this label at least `other` in both parts?
    pub fn covers(&self, other: &Label) -> bool {
        self.confidentiality >= other.confidentiality && self.integrity >= other.integrity
    }
}

/// On a receipt: the label of what this call reads or writes, and the mandate's label once
/// the call is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptLabels {
    pub data: Label,
    pub after: Label,
}

impl ReceiptLabels {
    pub fn validate(&self) -> Result<()> {
        // The mandate's label takes in what the call touched, so it is never below it.
        if !self.after.covers(&self.data) {
            return Err(Error::Malformed("a label after a call covers the call"));
        }
        Ok(())
    }
}

/// On a receipt: what came before it under the same root mandate (departure D5). One
/// reference, not a list, so a receipt stays one size however long the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cause {
    /// The previous receipt of the same root mandate at this gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<ReceiptRef>,
    /// The digest of the label token the call presented, signed by another gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Digest>,
}
