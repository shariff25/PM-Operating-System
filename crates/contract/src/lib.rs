//! The PM OS content contract.
//!
//! Skills, context, reviewer rubrics and templates are plain markdown files in git
//! (PRD principle: files are the source of truth). This crate defines their formats and the
//! lint that CI runs on every pull request. Any harness that reads these files (Deep Agents
//! for the team service, Hermes for personal agents) relies on the same contract (MOD-1).
//! The formats are documented in `docs/contract.md`.

pub mod bots;
pub mod frontmatter;
pub mod lint;
pub mod model;
pub mod onboarding;
pub mod text;

pub use lint::{lint, Diagnostic, LintOptions, Report, Severity};
pub use model::{Sensitivity, Skill, ToolDef, ToolKind, ToolRegistry};
