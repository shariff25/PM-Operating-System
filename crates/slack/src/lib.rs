//! `pmos-slack`: the PM OS Slack bot, the team's interface to its bots (spec §5 of
//! `docs/bot-runtime-spec.md`).
//!
//! - **DMs:** a PM's DM becomes a task for a bot (`pm-<handle>`, `docs`, or the one named with
//!   a `<bot>:` prefix) in the bots runtime. Progress, answers and failures come back in the
//!   thread from the runtime's notification feed, posted under the bot's own name.
//! - **Approval cards (D7):** each write a bot drafts is a card with **Approve**, **Edit** and
//!   **Reject**; only the PM it is for can decide it, through the tool server.
//! - **Routines (D8):** "every weekday at 8am …" is read back for a one-tap confirm; "my
//!   routines" lists them with **Pause**, **Resume** and **Delete**.
//! - **`/docs`:** search the Fortanix support docs from any conversation, with results only
//!   the asker can see, through the tool server's `docs` skill.
//!
//! It connects with Socket Mode (outbound only) and maps each Slack user to a PM OS handle
//! through `content/team.yaml`.

pub mod blocks;
pub mod bots;
pub mod bounded;
pub mod cards;
pub mod commands;
pub mod notify;
pub mod runtime;
pub mod slackapi;
pub mod socket;
pub mod toolserver;
