//! PM OS benchmarks (0g, D10): real tasks per category, run through the bot runtime as client
//! `evals`, graded, and each failure tagged to its bottleneck. See `docs/evals.md`.

pub mod client;
pub mod config;
pub mod grade;
pub mod history;
pub mod load;
pub mod outcome;
pub mod report;
pub mod run;
pub mod spec;
pub mod tag;
