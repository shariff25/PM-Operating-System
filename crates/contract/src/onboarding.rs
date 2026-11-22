//! Onboarding helpers shared by the tool server, the Slack bot and the CLI
//! (`docs/onboarding-spec.md`): proposing a handle for a new member, and the personal bot every
//! member gets.
//!
//! Users and roles live in the PM OS user database (`docs/user-database-spec.md`). When a
//! platform owner approves a new member, the tool server creates the user there and then
//! commits the member's `BOT.md`, made by [`render_personal_bot`], to the content repo.

use std::collections::BTreeSet;

use crate::bots::valid_name;
use crate::model::valid_handle;

/// The role every personal bot is made from, and the ceiling of every persona
/// (`docs/bot-marketplace-spec.md` §2, "The ceiling rules"): no persona's role may have a tool,
/// a data class or a write setting looser than this role's.
pub const PERSONAL_ROLE: &str = "pm-assistant";

/// The longest handle onboarding proposes, so `pm-<handle>` stays a valid bot name.
pub const MAX_PROPOSED_HANDLE: usize = 24;

/// A GitHub login: 1–39 letters, digits and single hyphens, not starting or ending with one.
pub fn valid_github_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--")
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The longest bot name ([`crate::bots::valid_name`]).
pub const MAX_BOT_NAME: usize = 32;

/// The most characters of a handle's spelled form ([`spelled_handle`]) that fit in a personal
/// bot's name, `pm-<this>`, without being cut.
pub const MAX_SPELLED_HANDLE: usize = MAX_BOT_NAME - "pm-".len();

/// A handle as a personal bot's name spells it, in full (not cut to fit): `.` and `_` become
/// `-`, a run of hyphens one, and leading and trailing hyphens go. Several handles can give
/// one spelled form (`ana.k`, `ana_k`, `ana-k`; `ana`, `ana.`, `ana..`).
pub fn spelled_handle(handle: &str) -> String {
    let mut out = String::new();
    let mut last_hyphen = true;
    for c in handle.chars() {
        let c = if c == '.' || c == '_' { '-' } else { c };
        if c == '-' {
            if !last_hyphen {
                out.push('-');
            }
            last_hyphen = true;
        } else {
            out.push(c);
            last_hyphen = false;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The personal bot of the member with this handle: `pm-<handle>`, with `.` and `_` turned into
/// hyphens so it is a valid bot name ([`spelled_handle`]), cut to [`MAX_BOT_NAME`] characters.
/// A handle whose spelled form is longer than [`MAX_SPELLED_HANDLE`] is cut, so it can share
/// its bot name with other handles; onboarding never gives out such a handle
/// ([`valid_new_handle`]).
pub fn personal_bot_name(handle: &str) -> String {
    let mut name = format!("pm-{}", spelled_handle(handle));
    name.truncate(MAX_BOT_NAME);
    name.trim_end_matches('-').to_string()
}

/// A handle for a new member, from their email's local part (or their Slack display name if
/// the email gives nothing usable): lowercase letters, digits and single hyphens, at most
/// [`MAX_PROPOSED_HANDLE`] characters, and not already in `taken` (a digit is added if it is).
pub fn propose_handle(email: &str, display_name: &str, taken: &BTreeSet<String>) -> String {
    let from = |s: &str| {
        let mut out = String::new();
        for c in s.chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                out.push(c);
            } else if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        }
        out.truncate(MAX_PROPOSED_HANDLE);
        out.trim_end_matches('-').to_string()
    };
    let local = email.split('@').next().unwrap_or_default();
    let mut base = from(local);
    if base.is_empty() {
        base = from(display_name);
    }
    if base.is_empty() || !base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base = format!("pm-{base}").trim_end_matches('-').to_string();
        base.truncate(MAX_PROPOSED_HANDLE);
    }
    if !taken.contains(&base) {
        return base;
    }
    for n in 2.. {
        let suffix = n.to_string();
        let mut stem = base.clone();
        stem.truncate(MAX_PROPOSED_HANDLE - suffix.len());
        let candidate = format!("{}{suffix}", stem.trim_end_matches('-'));
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("some suffix is free")
}

/// Is `handle` one onboarding can give out: a valid handle that also makes a valid personal
/// bot name and is at most [`MAX_PROPOSED_HANDLE`] characters?
pub fn valid_new_handle(handle: &str) -> bool {
    valid_handle(handle)
        && handle.len() <= MAX_PROPOSED_HANDLE
        && valid_name(handle)
        && valid_name(&personal_bot_name(handle))
}

/// The path of a bot's `BOT.md` under the content root.
pub fn bot_path(bot: &str) -> String {
    format!("bots/{bot}/BOT.md")
}

/// The `BOT.md` for a new member's personal bot: made from the [`PERSONAL_ROLE`], owned by
/// the member, reading as them.
pub fn render_personal_bot(handle: &str) -> String {
    let name = personal_bot_name(handle);
    format!(
        "---\n\
         name: {name}\n\
         role: {PERSONAL_ROLE}\n\
         scope: personal\n\
         owner: \"{handle}\"\n\
         description: {handle}'s personal assistant. It reads what {handle} can read and drafts; {handle} approves anything others will see.\n\
         ---\n\
         \n\
         # {name}\n\
         \n\
         Work for {handle}: answer their questions, write up their meetings and draft PRDs, Jira\n\
         issues and Slack replies from the team's context and what they can read. Keep notes on\n\
         what they tell you to remember in their memory repo.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_logins() {
        assert!(valid_github_login("ana"));
        assert!(valid_github_login("Ana-F1"));
        assert!(!valid_github_login("-ana"));
        assert!(!valid_github_login("ana--f"));
        assert!(!valid_github_login("ana_f"));
        assert!(!valid_github_login(&"a".repeat(40)));
    }

    #[test]
    fn personal_bot_names_are_valid_bot_names() {
        assert_eq!(personal_bot_name("ana"), "pm-ana");
        assert_eq!(personal_bot_name("ana.k_r"), "pm-ana-k-r");
        assert_eq!(personal_bot_name("a..b"), "pm-a-b");
        let long = personal_bot_name(&"x".repeat(64));
        assert!(valid_name(&long), "{long}");
        // The spelled form is the bot name's part after `pm-`, uncut.
        assert_eq!(spelled_handle("ana.k_r"), "ana-k-r");
        assert_eq!(spelled_handle("ana.."), "ana");
        assert_eq!(spelled_handle(&"x".repeat(64)).len(), 64);
        assert_eq!(MAX_SPELLED_HANDLE, 29);
        // Every handle onboarding gives out is spelled in full in its bot name.
        let h = "a".repeat(MAX_PROPOSED_HANDLE);
        assert!(valid_new_handle(&h));
        assert!(spelled_handle(&h).len() <= MAX_SPELLED_HANDLE);
        assert_eq!(personal_bot_name(&h), format!("pm-{h}"));
    }

    #[test]
    fn handles_come_from_the_email_and_avoid_taken_ones() {
        let mut taken = BTreeSet::new();
        assert_eq!(
            propose_handle("Ana.Kumar@example.com", "", &taken),
            "ana-kumar"
        );
        taken.insert("ana-kumar".to_string());
        assert_eq!(
            propose_handle("ana.kumar@example.com", "", &taken),
            "ana-kumar2"
        );
        assert_eq!(propose_handle("@x.com", "Raj Patel", &taken), "raj-patel");
        assert_eq!(propose_handle("123@x.com", "", &taken), "pm-123");
        let long = propose_handle(&format!("{}@x.com", "a".repeat(60)), "", &taken);
        assert!(valid_new_handle(&long), "{long}");
        for h in ["ana-kumar", "ana-kumar2", "raj-patel", "pm-123"] {
            assert!(valid_new_handle(h), "{h}");
        }
        assert!(
            !valid_new_handle("ana.k"),
            "a dot doesn't make a clean bot name"
        );
    }

    #[test]
    fn the_personal_bot_parses_as_a_personal_bot() {
        let text = render_personal_bot("ana");
        let doc = crate::frontmatter::split(&text).expect("front matter");
        let fm: crate::bots::BotFrontMatter = serde_norway::from_str(doc.yaml).expect("parses");
        let body = doc.body;
        assert_eq!(fm.name, "pm-ana");
        assert_eq!(fm.role, PERSONAL_ROLE);
        assert_eq!(PERSONAL_ROLE, "pm-assistant");
        assert_eq!(fm.scope, crate::bots::BotScope::Personal);
        assert_eq!(fm.owner, "ana");
        assert!(body.contains("Work for ana"));
    }
}
