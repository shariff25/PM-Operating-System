//! `pmos-web`: the back end of the PM OS web client, phase 1 (local accounts).
//!
//! - **People:** the PM OS user database is the source of truth for who may sign in and with
//!   which roles ([`model`], [`store`], [`pg`]). `team.yaml` follows it (a later slice).
//! - **Sign-in:** a local handle and password, hashed with Argon2id, behind an
//!   [`provider::AuthProvider`] so Entra ID can be added later without touching sessions.
//!   Accounts lock with backoff after repeated failures, and each client IP is rate limited
//!   ([`password`], [`ratelimit`]).
//! - **Sessions:** server side, under a random 256-bit id kept only as a hash; idle and
//!   absolute expiry, a new id at sign-in and on any privilege change, and a `__Host-` cookie.
//!   Every state-changing call carries a CSRF token bound to the session ([`session`]).
//! - **Audit:** security events go to an [`audit::AuditSink`], never with a password or a
//!   session id in them.
//!
//! The HTTP surface is [`app::router`]; the rules live in [`service::Auth`], so they are
//! tested without HTTP too. The threat model is in this crate's `README.md`.

pub mod app;
pub mod audit;
pub mod clock;
pub mod config;
pub mod model;
pub mod password;
pub mod pg;
pub mod provider;
pub mod random;
pub mod ratelimit;
pub mod service;
pub mod session;
pub mod store;

pub use app::router;
pub use service::Auth;
