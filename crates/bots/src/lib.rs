//! `pmos-bots`: the runtime behind PM OS's named bots (0e; spec §3 of
//! `docs/bot-runtime-spec.md`, decisions D2, D6 and D8).
//!
//! - **Tasks:** Slack and the benchmark runner ask a bot for something; the task waits in a
//!   durable queue until the bot's harness worker claims it, then reports progress, a trace
//!   and a result. A claim is a lease, so a worker or runtime that dies mid-task never
//!   loses the task silently: it is retried once, then failed with a reason.
//! - **Approvals:** a result with outbound writes waits as `needs_approval` until the PM has
//!   decided every item in Slack.
//! - **Notifications:** one ordered feed the Slack app long-polls for milestones and results.
//! - **Routines:** plain-language schedules, parsed by the model and checked in code, run by a
//!   scheduler that survives restarts.
//! - **Traces:** the full record of each task for the console, kept 30 days, with Slack text
//!   removed after 24 hours.

pub mod app;
pub mod assertion;
pub mod catalog;
pub mod clients;
pub mod config;
pub mod model;
pub mod people;
pub mod pg;
pub mod routines;
pub mod runtime;
pub mod store;

pub use app::{router, router_with};
pub use runtime::Runtime;
