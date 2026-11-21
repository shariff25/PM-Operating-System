//! PM OS tool server.
//!
//! The only path from an agent sandbox to the context library, memory and the systems of
//! record (Jira, Confluence, Slack). It serves MCP to the harness and enforces the PRD's
//! rules in one place, for the team service and personal agents alike (MOD-3):
//! per-skill tool allowlists, a sensitivity ceiling per skill, untrusted-content wrapping,
//! an approval for every write outside PM OS, and an audit log.

pub mod app;
pub mod approvals;
pub mod assertion;
pub mod audit;
pub mod bets;
pub mod catalog;
pub mod config;
pub mod connectors;
pub mod content_sync;
pub mod context;
pub mod github;
pub mod github_memory;
pub mod ledger;
pub mod mcp;
pub mod members;
pub mod memory;
pub mod memory_repo;
pub mod oauth;
pub mod onboarding;
pub mod password;
pub mod pg;
pub mod proposals;
pub mod retention;
pub mod reviews;
pub mod schemas;
pub mod service;
pub mod session;
pub mod sources;
pub mod stats;
pub mod store;
pub mod team;
pub mod users;
pub mod vault;

pub use app::{router, router_full, router_with, AppConfig, PersonClients};
pub use service::ToolService;
pub use store::Stores;
