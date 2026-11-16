//! PM OS web app backend (`docs/web-api.md`): the session and the `/api` routes the web app
//! calls. It acts for the signed-in PM against the bot runtime and the tool server.
//!
//! - [`config`]: settings from the environment, checked at start-up.
//! - [`auth`]: the sign-in providers ([`auth::AuthProvider`]) and the origin check.
//! - [`session`]: sessions (random 256-bit ids, stored hashed) and the `__Host-pmos` cookie.
//! - [`settings`]: per-PM state the web app keeps (hints, nudges, edits, suggestions,
//!   practice drafts). In memory for now.
//! - [`upstream`]: the bot runtime (client `web`) and the tool server (approvals).
//! - [`drafts`]: approval items shown as drafts.
//! - [`progress`]: a task's runtime events as the app shows them (milestones, progress,
//!   retrying).
//! - [`app`]: the router and the `/api` handlers.
//!
//! Security notes, and what is not built, are in `crates/web/README.md`.

pub mod app;
pub mod auth;
pub mod config;
pub mod drafts;
pub mod progress;
pub mod session;
pub mod settings;
pub mod upstream;

pub use app::{build, Deps};
pub use config::Config;
