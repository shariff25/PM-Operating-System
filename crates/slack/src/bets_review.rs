//! The monthly bets review, Slack's half (`docs/staff-principal-spec.md` §5).
//!
//! **The commands** (typed alone in a DM): `bets review on` creates the PM's `[bets-review]`
//! routine on their personal bot with `POST /v1/routines {draft}` (the model isn't
//! involved), or says it's already on; `bets review off` deletes it; `bets review` says
//! whether it's on and when it next runs. It runs at 09:00 on the 1st of each month in the
//! PM's Slack time zone, else `PMOS_DEFAULT_TZ`. A member with no personal bot is told so,
//! and nothing changes. It works as `reply assistant` does ([`crate::reply`]), which holds
//! the shared handling.

use serde_json::Value;

use crate::bots::{Bots, Dm};
use crate::reply::{routine_command_of, Command, RoutineKind};

/// How Slack finds the routine: its prompt starts with this.
pub const TAG: &str = "[bets-review]";
pub const PROMPT: &str = "[bets-review] Review my bets register with the bets-register skill: find the open bets due for review, gather new evidence, and propose updates.";
/// 09:00 on the 1st of each month.
pub const CRON: &str = "0 9 1 * *";
const SCHEDULE_WORDS: &str = "at 09:00 on the 1st of each month";

pub const BETS_REVIEW: RoutineKind = RoutineKind {
    words: "bets review",
    the: "the bets review",
    tag: TAG,
    cron: CRON,
    prompt: PROMPT,
    schedule_words: SCHEDULE_WORDS,
    on_text: |shown, tz, next| {
        format!(
            "*The bets review is on.* {shown} reviews your bets register {SCHEDULE_WORDS} \
             ({tz} time): for each open bet due for review, it gathers new evidence and \
             proposes an update here for you to approve. Nothing changes in the register \
             without your tap.\nNext run: {next}."
        )
    },
    off_text: "*The bets review is off.* Your bets register is unchanged, and updates \
               already waiting stay until you decide them or they expire.",
    status_off_text: |shown| {
        format!(
            "*The bets review is off.* Say `bets review on` and {shown} reviews your bets \
             register {SCHEDULE_WORDS}, and proposes updates for you to approve."
        )
    },
};

/// `bets review on`, `bets review off` and `bets review` (or `… status`), typed alone, in
/// any case.
pub fn command(text: &str) -> Option<Command> {
    routine_command_of(text, BETS_REVIEW.words)
}

/// The routine `bets review on` creates (spec §5), exactly.
pub fn draft(handle: &str, tz: &str) -> Value {
    BETS_REVIEW.draft(handle, tz)
}

/// A PM's `[bets-review]` routine, by its prompt.
pub fn is_bets_review_routine(routine: &Value) -> bool {
    BETS_REVIEW.is_routine(routine)
}

impl Bots {
    /// `bets review on`, `off` and status.
    pub(crate) async fn bets_review(&self, dm: &Dm, handle: &str, cmd: Command) {
        self.routine_command(dm, handle, cmd, &BETS_REVIEW).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands() {
        assert_eq!(command("bets review on"), Some(Command::On));
        assert_eq!(command("  Bets  Review OFF. "), Some(Command::Off));
        assert_eq!(command("bets review?"), Some(Command::Status));
        assert_eq!(command("bets review status"), Some(Command::Status));
        assert_eq!(command("bets review on please"), None);
        assert_eq!(command("bets"), None);
        assert_eq!(command("reply assistant on"), None);
    }

    #[test]
    fn the_draft_is_the_specs() {
        let d = draft("ana", "Europe/London");
        assert_eq!(
            d,
            json!({"bot": "pm-ana", "owner": "ana",
             "schedule": {"cron": "0 9 1 * *", "tz": "Europe/London"},
             "prompt": "[bets-review] Review my bets register with the bets-register skill: find the open bets due for review, gather new evidence, and propose updates.",
             "output": {"kind": "dm"},
             "approval": "approve"})
        );
        assert!(is_bets_review_routine(&d));
        assert!(!is_bets_review_routine(&crate::reply::draft("ana", "UTC")));
        assert!(!crate::reply::is_reply_routine(&d));
    }
}
