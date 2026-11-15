//! The PM's level, set from a DM (`docs/staff-principal-spec.md` §1).
//!
//! **The commands** (typed alone in a DM, for members): `level` says which level the PM is
//! set to; `level senior`, `level staff` and `level principal` set it with
//! `PUT /v1/members/{handle}/settings`. An unknown level lists the three, and a runtime that
//! doesn't answer changes nothing.

use crate::blocks::escape;
use crate::bots::{Bots, Dm};
use crate::cards;
use crate::reply::runtime_note;
use crate::runtime::RuntimeError;

/// A runtime failure while setting the level.
pub const NOT_CHANGED: &str =
    "PM OS couldn't reach its bots right now, so your level wasn't changed. Try again in a minute.";

/// The levels, lowest first.
pub const LEVELS: [&str; 3] = ["senior", "staff", "principal"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `level`: which level the PM is set to.
    Show,
    /// `level staff`.
    Set(&'static str),
    /// `level <something else>`, lowercased.
    Unknown(String),
}

/// `level` or `level <word>`, typed alone, in any case.
pub fn command(text: &str) -> Option<Command> {
    let t = text.trim().trim_end_matches(['?', '.', '!']).to_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();
    match words.as_slice() {
        ["level"] => Some(Command::Show),
        ["level", word] => Some(
            LEVELS
                .into_iter()
                .find(|l| l == word)
                .map_or_else(|| Command::Unknown(word.to_string()), Command::Set),
        ),
        _ => None,
    }
}

/// "Senior", "Staff", "Principal"; anything else as it came.
pub fn title(level: &str) -> String {
    match level {
        "senior" => "Senior".into(),
        "staff" => "Staff".into(),
        "principal" => "Principal".into(),
        other => other.to_string(),
    }
}

/// What a level means, for the PM: scope, horizon, audience.
pub fn meaning(level: &str) -> &'static str {
    match level {
        "staff" => "several teams or a product line, 4 to 6 quarters ahead, written for directors and peer leads",
        "principal" => "the company and its market, 2 to 3 years ahead, written for executives and the board",
        _ => "one product area, this quarter and next, written for the team and its stakeholders",
    }
}

/// `level`: "You're set to the *Staff* level. Change it with …".
pub fn shown(level: &str) -> String {
    format!(
        "You're set to the *{}* level. Change it with `level senior`, `level staff` or \
         `level principal`.",
        escape(&title(level))
    )
}

/// `level staff`: "Done. I'll work at the Staff level: …".
pub fn set(level: &str) -> String {
    format!(
        "Done. I'll work at the {} level: {}.",
        title(level),
        meaning(level)
    )
}

/// An unknown level: the three there are.
pub fn unknown(word: &str) -> String {
    format!(
        "`{}` isn't a level. The levels are `senior`, `staff` and `principal`: say \
         `level senior`, `level staff` or `level principal`.",
        escape(word)
    )
}

/// The line at the top of the App Home's Skills section.
pub fn home_line(level: &str) -> String {
    format!(
        "You're set to the *{}* level. Change it by sending me `level …`.",
        escape(&title(level))
    )
}

impl Bots {
    /// `level`, `level <level>`.
    pub(crate) async fn level_command(&self, dm: &Dm, handle: &str, cmd: Command) {
        let text = match cmd {
            Command::Show => match self.runtime().level(handle).await {
                Ok(level) => shown(&level),
                Err(e) => runtime_note(&e),
            },
            Command::Unknown(word) => unknown(&word),
            Command::Set(level) => match self.runtime().set_level(handle, level).await {
                Ok(saved) => {
                    tracing::info!(owner = handle, level = %saved, "level set");
                    set(&saved)
                }
                Err(RuntimeError::Unavailable) => NOT_CHANGED.into(),
                Err(e) => format!(
                    "I couldn't change your level, so nothing was changed: {}",
                    runtime_note(&e)
                ),
            },
        };
        self.post(&dm.channel, Some(&dm.thread_ts), None, cards::note(&text))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(command("level"), Some(Command::Show));
        assert_eq!(command(" Level? "), Some(Command::Show));
        assert_eq!(command("level staff"), Some(Command::Set("staff")));
        assert_eq!(command("LEVEL Principal."), Some(Command::Set("principal")));
        assert_eq!(
            command("level director"),
            Some(Command::Unknown("director".into()))
        );
        assert_eq!(command("level up the roadmap"), None);
        assert_eq!(command("levels"), None);
    }

    #[test]
    fn the_answers_are_the_specs() {
        assert_eq!(
            shown("staff"),
            "You're set to the *Staff* level. Change it with `level senior`, `level staff` or `level principal`."
        );
        assert_eq!(
            set("staff"),
            "Done. I'll work at the Staff level: several teams or a product line, 4 to 6 quarters ahead, written for directors and peer leads."
        );
        assert_eq!(
            set("senior"),
            "Done. I'll work at the Senior level: one product area, this quarter and next, written for the team and its stakeholders."
        );
        assert_eq!(
            set("principal"),
            "Done. I'll work at the Principal level: the company and its market, 2 to 3 years ahead, written for executives and the board."
        );
        assert_eq!(
            home_line("principal"),
            "You're set to the *Principal* level. Change it by sending me `level …`."
        );
        assert!(unknown("vp").contains("`level senior`, `level staff` or `level principal`"));
    }
}
