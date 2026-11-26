//! What the Ledger hands an auditor: every receipt it holds, and its record of what it pruned.

use gov_core::{Envelope, Id, PublicKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateExport {
    pub gate: Id,
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
}
