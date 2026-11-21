//! `pmos-slack`: the PM OS Slack bot, the team's interface to its bots (spec §5 of
//! `docs/bot-runtime-spec.md`).
//!
//! - **DMs:** a PM's DM becomes a task for a bot (`pm-<handle>`, `docs`, or the one named with
//!   a `<bot>:` prefix) in the bots runtime. Progress, answers and failures come back in the
//!   thread from the runtime's notification feed, posted under the bot's own name.
//!   `Chief: …` or `sprout, …` asks a marketplace bot on the PM's own bot, and its messages
//!   start with its name (`personas.rs`, `docs/slack-bots.md`).
//! - **Approval cards (D7):** each write a bot drafts is a card with **Approve**, **Edit** and
//!   **Reject**; only the PM it is for can decide it, through the tool server. **Reject**
//!   asks why first, with six one-tap reasons the feedback ledger keeps
//!   (`docs/improvement-loop-spec.md` §4).
//! - **The reply assistant** (`docs/slack-reply-spec.md` §1 and §5): `reply assistant on`,
//!   `off` and status manage a PM's `[reply-assistant]` routine; each reply it drafts is a
//!   reply card with **Post as you**, **Edit**, **Snooze** and **Reject** (`reply.rs`).
//! - **Levels and the bets review** (`docs/staff-principal-spec.md` §1 and §5): `level` shows
//!   or sets the PM's level (`level.rs`); `bets review on`, `off` and status manage their
//!   monthly `[bets-review]` routine (`bets_review.rs`).
//! - **Triage digest:** on weekday mornings each bot's quality owner gets a DM of the drafts
//!   that were rejected or heavily edited since the last one (`triage.rs`).
//! - **Routines (D8):** "every weekday at 8am …" is read back for a one-tap confirm; "my
//!   routines" lists them with **Pause**, **Resume** and **Delete**.
//! - **Onboarding** (`docs/onboarding-spec.md` §4): someone outside the user directory taps
//!   **Request access**, a platform owner approves them in a DM card, and the new member gets
//!   a welcome and a setup checklist (`setup`) for their accounts, memory repo and bot.
//! - **Tagging PM OS in a channel** (`docs/slack-mentions-and-home-spec.md` §2): `@PM OS …`
//!   in a thread starts a task for the PM's personal bot, answered in their DM; the thread
//!   gets one line only they see (`mention.rs`).
//! - **The daily brief** (`docs/daily-brief.md`): `brief me at 8` and friends (`brief.rs`);
//!   each brief's items also go to the web app's home page (`brief_feed.rs`).
//! - **The App Home** (same spec, §4): Inbox, Skills and Accounts (`home.rs`).
//! - **`/docs`:** search the company's public docs from any conversation, with results only
//!   the asker can see, through the tool server's `docs` skill.
//!
//! It connects with Socket Mode (outbound only) and maps each Slack user to a PM OS handle
//! through the user database: a snapshot of the tool server's `GET /internal/directory`
//! (`people.rs`, `docs/user-database-spec.md` §2). It never reads `content/team.yaml`.

pub mod access_copy;
pub mod bets_review;
pub mod blocks;
pub mod bots;
pub mod bounded;
pub mod brief;
pub mod brief_feed;
pub mod cards;
pub mod commands;
pub mod home;
pub mod level;
pub mod member_cards;
pub mod members;
pub mod mention;
pub mod notify;
pub mod nudge_copy;
pub mod nudges;
pub mod onboard_copy;
pub mod onboard_flow;
pub mod people;
pub mod personas;
pub mod proxy;
pub mod reply;
pub mod runtime;
pub mod slackapi;
pub mod socket;
pub mod toolserver;
pub mod triage;
