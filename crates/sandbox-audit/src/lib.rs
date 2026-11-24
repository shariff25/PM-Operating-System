//! `pmos-sandbox-audit`: OpenShell's own record of each sandbox's decisions, into PM OS's audit
//! log (`docs/openshell-prover-and-sandbox-audit-spec.md` Part B, slice B2).
//!
//! OpenShell's gateway and sandbox supervisors export OCSF records over OTLP/HTTP to this
//! collector on loopback ([`app`]), with a header token the workload never holds. The collector
//! decodes the protobuf by hand ([`otlp`]), keeps an allowlist of metadata, cleaned ([`ocsf`]),
//! rate-limits denies and sums allows ([`limits`]), spools each batch before it answers
//! OpenShell ([`spool`]), and posts the batches and a heartbeat to the tool server with its own
//! append-only token ([`wire`]). Off unless configured: nothing in the stack needs it.
//!
//! Nothing here has run against a real OpenShell. Every assumption about what OpenShell sends is
//! marked "verify on a real host" where it is made (slice B6).

pub mod app;
pub mod limits;
pub mod ocsf;
pub mod otlp;
pub mod spool;
pub mod wire;
