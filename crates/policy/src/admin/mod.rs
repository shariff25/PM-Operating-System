//! The admin policy and content screens' shared logic (`docs/policy-admin.md`,
//! `docs/content-admin.md`), used by `pmos-web` (reading and the interactive validation) and
//! by the tool server (which checks a proposal again, from nothing, before it opens a pull
//! request with the acting admin's own GitHub token).
//!
//! - [`proposals`]: paths, caps, a scratch copy of the content tree, clean titles, branch
//!   names, the rate limiter, diffs and git blob ids.
//! - [`evaluate`]: what each sandbox is allowed, the spec, and a change's evaluation (lint,
//!   regeneration, rules, allowance diff).
//! - [`changeset`]: the change set sent to the tool server and its re-check.
//! - [`access`]: who may read which content file.

pub mod access;
pub mod changeset;
pub mod evaluate;
pub mod proposals;
