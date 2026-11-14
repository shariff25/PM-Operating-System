//! `pmos-slack`: the PM OS Slack bot.
//!
//! Today it serves `/docs`: search the Acme support docs from any Slack conversation,
//! with results only the asker can see. It connects with Socket Mode (outbound only), maps
//! the Slack user to a PM OS handle through `content/team.yaml`, and calls the tool server's
//! internal API, so the `docs` skill's allowlist and the audit log apply. The rest of the bot
//! (mentions, the assistant panel, approvals) arrives in Phase 1; see
//! `docs/implementation-plan.md`.

pub mod blocks;
pub mod commands;
pub mod socket;
pub mod toolserver;
