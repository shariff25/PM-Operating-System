//! Agent Governance, the core: the signed documents, the canonical action, and the checks a
//! gate, an Authority and a Ledger all make the same way. Spec: `docs/agent-governance-spec.md`.
//!
//! This crate reads no file, no clock and no network. Time comes in as a number; keys come in
//! as values. It depends on no PM OS crate (`tests/layout.rs`).
//!
//! Built in slice G1, before the security reviewer's read of the spec, on the owner's
//! instruction of 2026-11-26. Nothing here decides anything about a real system yet.

pub mod action;
pub mod connector;
pub mod countersign;
pub mod envelope;
pub mod error;
pub mod ids;
pub mod jcs;
pub mod keys;
pub mod lease;
pub mod mandate;
pub mod policy;
pub mod receipt;
pub mod request;
pub mod revocation;

pub use envelope::{open, seal, Document, Envelope, Opened, VERSION};
pub use error::{Error, Result};
pub use ids::{Digest, Id, ResourcePath, ResourcePattern, Scope};
pub use keys::{random_id, random_salt, KeySet, PublicKey, SigningKey};
