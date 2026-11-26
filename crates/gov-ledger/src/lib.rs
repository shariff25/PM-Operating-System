//! Agent Governance, the Ledger: signed receipts in, a hash chain per gate, checkpoints out
//! to a witness the customer controls, and a check anyone can run with public keys alone.
//! Spec: `docs/agent-governance-spec.md` §6.
//!
//! Built in slice G2, before the security reviewer's read, on the owner's instruction of
//! 2026-11-26. This is the library; the network listener that gates ship receipts to comes
//! with the gate (G3).

pub mod export;
pub mod http;
pub mod ledger;
pub mod pg;
pub mod store;
pub mod verify;
pub mod witness;

pub use export::{Export, GateExport, TrustedKeys};
pub use ledger::{Ingested, Ledger, LedgerError};
pub use store::{ChainKind, Enrolment, MemoryStore, Store};
pub use witness::{DirWitness, Witness};
