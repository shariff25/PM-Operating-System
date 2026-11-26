//! What the Ledger hands an auditor: every receipt it holds, and its record of what it pruned.

use gov_core::{Envelope, Id, PublicKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::store::ChainKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateExport {
    pub gate: Id,
    /// What the chain holds. Absent for a gate's receipts, as in every export made before
    /// there was another kind.
    #[serde(default, skip_serializing_if = "ChainKind::is_receipt")]
    pub kind: ChainKind,
    /// The Ledger's signed "pruned through" record, if it has deleted any of this chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruned: Option<Envelope>,
    /// In order of number.
    pub receipts: Vec<Envelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub gates: Vec<GateExport>,
}

/// The public keys a verifier trusts. They come from the customer's own record of what was
/// enrolled, not from the export being checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKeys {
    /// The Ledger's checkpoint and prune-record keys.
    pub ledger: Vec<PublicKey>,
    /// Each gate's receipt keys.
    pub gates: BTreeMap<Id, Vec<PublicKey>>,
    /// Each Authority's admin-record keys. A key is trusted for one kind of chain only: a
    /// gate's key here, or an Authority's key above, would let one kind of document stand
    /// where the other belongs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub authorities: BTreeMap<Id, Vec<PublicKey>>,
}
