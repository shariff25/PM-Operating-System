//! PM OS tool server.
//!
//! The only path from an agent sandbox to the context library, memory and the systems of
//! record (Jira, Confluence, Slack). It serves MCP to the harness and enforces the PRD's
//! rules in one place, for the team service and personal agents alike (MOD-3):
//! per-skill tool allowlists, a sensitivity ceiling per skill, untrusted-content wrapping,
//! an approval for every write outside PM OS, and an audit log.

pub mod app;
pub mod approvals;
pub mod audit;
pub mod catalog;
pub mod config;
pub mod connectors;
pub mod context;
pub mod mcp;
pub mod memory;
pub mod oauth;
pub mod pg;
pub mod schemas;
pub mod service;
pub mod session;
pub mod store;
pub mod team;
pub mod vault;

pub use app::{router, AppConfig};
pub use service::ToolService;
pub use store::Stores;
