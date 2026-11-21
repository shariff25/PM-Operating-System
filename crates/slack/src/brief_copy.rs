//! The words of the brief commands, as data, so the owner can edit them without touching the
//! logic in `brief.rs`. Plain and short; the one light touch is in the confirmation.
//!
//! Dynamic parts are escaped by the caller before they reach these functions.

/// The default time for `brief on`.
pub const DEFAULT_TIME: (u32, u32) = (8, 30);

pub const NOT_A_MEMBER: &str = "The daily brief is for PM OS team members.";

pub const NO_BOT: &str = "The brief runs on your personal bot, and you don't have one yet, so \
     nothing was changed. Say `setup` to see where it stands.";

pub const UNAVAILABLE: &str = "PM OS couldn't reach its bots right now. Try again in a minute.";

pub const BAD_TIME: &str = "I couldn't read that time. Use 00:00 to 23:59, like `brief me at 8`, \
     `brief me at 8:30` or `brief me at 7am`.";

pub const OFF: &str = "Your daily brief is off. Say `brief me at 8` to turn it back on.";

pub const ALREADY_OFF: &str = "Your daily brief is already off. Say `brief me at 8` to start it.";

pub const NOW_STARTED: &str = "On it. Your brief will land in this thread.";

pub const STATUS_OFF: &str =
    "Your daily brief is off. Say `brief me at 8` to get it on weekday mornings.";

pub fn on(time: &str, tz: &str) -> String {
    format!(
        "Your morning brief is set for weekdays at {time} ({tz}), coffee not included. \
         Say `brief off` to stop it."
    )
}

pub fn unchanged(time: &str, tz: &str) -> String {
    format!("Your brief is already set for weekdays at {time} ({tz}). Say `brief off` to stop it.")
}

pub fn resumed(time: &str, tz: &str) -> String {
    format!(
        "Your brief was paused; it's back on for weekdays at {time} ({tz}). Say `brief off` to \
         stop it."
    )
}

pub fn status_on(time: &str, tz: &str, next: &str) -> String {
    format!(
        "Your brief is on: weekdays at {time} ({tz}). Next run: {next}. Say `brief off` to stop it."
    )
}

pub fn status_paused(time: &str, tz: &str) -> String {
    format!(
        "Your brief is paused (it was set for weekdays at {time}, {tz}). Say `brief me at {time}` \
         to resume it, or `brief off` to remove it."
    )
}

pub fn failed(why: &str) -> String {
    format!("I couldn't change your brief, so nothing was changed: {why}")
}
