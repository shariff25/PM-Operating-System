//! PM OS benchmarks (0g, D10): real tasks per category, run through the bot runtime as client
//! `evals`, graded, and each failure tagged to its bottleneck. The improvement loop adds
//! repeats, comparisons, the eval gate, the failure taxonomy and LLM judges. See
//! `docs/evals.md`.

pub mod affected;
pub mod client;
pub mod config;
pub mod gate;
pub mod grade;
pub mod history;
pub mod isolate;
pub mod judge;
pub mod load;
pub mod outcome;
pub mod report;
pub mod run;
pub mod spec;
pub mod stats;
pub mod tag;
pub mod taxonomy;
