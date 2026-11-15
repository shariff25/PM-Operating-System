//! `pmos-slack`: the PM OS Slack bot, the team's interface to its bots (spec §5 of
//! `docs/bot-runtime-spec.md`).
//!
//! - **DMs:** a PM's DM becomes a task for a bot (`pm-<handle>`, `docs`, or the one named with
//!   a `<bot>:` prefix) in the bots runtime. Progress, answers and failures come back in the
//!   thread from the runtime's notification feed, posted under the bot's own name.
//! - **Approval cards (D7):** each write a bot drafts is a card with **Approve**, **Edit** and
//!   **Reject**; only the PM it is for can decide it, through the tool server. **Reject**
//!   asks why first, with six one-tap reasons the feedback ledger keeps
//!   (`docs/improvement-loop-spec.md` §4).
//! - **The reply assistant** (`docs/slack-reply-spec.md` §1 and §5): `reply assistant on`,
//!   `off` and status manage a PM's `[reply-assistant]` routine; each reply it drafts is a
//!   reply card with **Post as you**, **Edit**, **Snooze** and **Reject** (`reply.rs`).
//! - **Triage digest:** on weekday mornings each bot's quality owner gets a DM of the drafts
//!   that were rejected or heavily edited since the last one (`triage.rs`).
//! - **Routines (D8):** "every weekday at 8am …" is read back for a one-tap confirm; "my
//!   routines" lists them with **Pause**, **Resume** and **Delete**.
//! - **Onboarding** (`docs/onboarding-spec.md` §4): someone outside `team.yaml` taps
//!   **Request access**, a platform owner approves them in a DM card, and the new member gets
//!   a welcome and a setup checklist (`setup`) for their accounts, memory repo and bot.
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
pub mod member_cards;
pub mod members;
pub mod notify;
pub mod proxy;
pub mod reply;
pub mod runtime;
pub mod slackapi;
pub mod socket;
pub mod toolserver;
pub mod triage;
