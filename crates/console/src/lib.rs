//! PM OS console (bot runtime spec §6): a small server-rendered web console with four
//! screens, read-mostly.
//!
//! - **Bots:** the roster, with each bot's role, scope, owner, read scope and write settings.
//! - **Routines:** each with its next run; pause, resume and delete.
//! - **Approvals:** the queue from the tool server, read-only; decisions happen in Slack.
//! - **Activity:** recent tasks; a task opens its events, its full trace and its audit events.
//!
//! No frontend build and no script: plain HTML forms and one inline stylesheet. Admins see
//! everything; a PM sees their own bots, routines and tasks plus the team bots.

pub mod app;
pub mod audit;
pub mod auth;
pub mod config;
pub mod html;
pub mod upstream;

pub use app::router;
pub use config::Config;
