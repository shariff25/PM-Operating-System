//! PM OS web app backend (`docs/web-api.md`): the session and the `/api` routes the web app
//! calls. It acts for the signed-in PM against the bot runtime and the tool server.
//!
//! - [`config`]: settings from the environment, checked at start-up.
//! - [`auth`]: the sign-in providers ([`auth::AuthProvider`]) and the origin check.
//! - [`entra`]: Entra ID sign-in (OpenID Connect, authorization code with PKCE).
//! - [`session`]: sessions (random 256-bit ids, stored hashed) and the `__Host-pmos` cookie.
//! - [`settings`]: per-PM state the web app keeps (hints, nudges, edits, suggestions,
//!   practice drafts).
//! - [`pg`]: the Postgres stores (sessions, settings, follow-ups), chosen by
//!   `PMOS_WEB_DATABASE_URL` ([`store_config`]); memory without it.
//! - [`followups`]: the follow-up store and the daily brief's data (`app::brief` has its routes).
//! - Who may sign in, their roles, and the admin Team page's member list come from the user
//!   database (`pmos_directory::resolve`; `app::users` for the admin list). `pmos-web` reads no
//!   `team.yaml`.
//! - [`upstream`]: the bot runtime (client `web`) and the tool server (approvals).
//! - [`drafts`]: approval items shown as drafts.
//! - [`progress`]: a task's runtime events as the app shows them (milestones, progress,
//!   retrying).
//! - [`app`]: the router and the `/api` handlers.
//! - [`admin_config`], [`policy_admin`], [`content_admin`], [`proposals`]: the admin policy
//!   and content screens, read-and-propose: a change is validated here and sent to the tool
//!   server, which checks it again and opens a GitHub pull request with the admin's own linked
//!   GitHub account. `pmos-web` holds no GitHub token (`docs/policy-admin.md`,
//!   `docs/content-admin.md`).
//!
//! Security notes, and what is not built, are in `crates/web/README.md`.

pub mod admin_config;
pub mod app;
pub mod auth;
pub mod config;
pub mod content_admin;
pub mod drafts;
pub mod entra;
pub mod followups;
pub mod host;
pub mod pg;
pub mod policy_admin;
pub mod progress;
pub mod proposals;
pub mod session;
pub mod settings;
pub mod store_config;
pub mod upstream;

pub use app::{build, Deps};
pub use config::Config;
