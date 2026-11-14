//! Onboarding helpers shared by the tool server and the Slack bot (`docs/onboarding-spec.md`):
//! proposing a handle for a new member, writing `team.yaml` back out, and the personal bot
//! every member gets.
//!
//! `team.yaml` stays the source of truth. When a platform owner approves a new member in
//! Slack, the tool server commits a new `team.yaml` (and the member's `BOT.md`) to the content
//! repo, produced by [`render_team_yaml`] and [`render_personal_bot`].

use std::collections::BTreeSet;

use crate::bots::valid_name;
use crate::model::{valid_handle, TeamMember};

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

/// The personal bot of the member with this handle: `pm-<handle>`, with `.` and `_` turned into
/// hyphens so it is a valid bot name.
pub fn personal_bot_name(handle: &str) -> String {
    let mut name = String::from("pm-");
    let mut last_hyphen = true;
    for c in handle.chars() {
        let c = if c == '.' || c == '_' { '-' } else { c };
        if c == '-' {
            if !last_hyphen {
                name.push('-');
            }
            last_hyphen = true;
        } else {
            name.push(c);
            last_hyphen = false;
        }
    }
    let mut name = name.trim_end_matches('-').to_string();
    name.truncate(32);
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

/// `team.yaml` with `members` in place of the old list. The comment block at the top of
/// `original` (every line before `members:`) is kept as written; the member list is
/// rewritten in one fixed layout, so the result parses back to exactly `members`.
pub fn render_team_yaml(original: &str, members: &[TeamMember]) -> String {
    let mut out = String::new();
    for line in original.lines() {
        if line.trim_start().starts_with("members:") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if members.is_empty() {
        out.push_str("members: []\n");
        return out;
    }
    out.push_str("members:\n");
    for m in members {
        out.push_str(&format!("  - handle: {}\n", quote(&m.handle)));
        out.push_str(&format!("    slack-id: {}\n", quote(&m.slack_id)));
        if let Some(email) = &m.email {
            out.push_str(&format!("    email: {}\n", quote(email)));
        }
        if let Some(github) = &m.github {
            out.push_str(&format!("    github: {}\n", quote(github)));
        }
        let roles: Vec<&str> = m.roles.iter().map(|r| r.as_str()).collect();
        out.push_str(&format!("    roles: [{}]\n", roles.join(", ")));
    }
    out
}

/// A double-quoted YAML scalar.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The path of a bot's `BOT.md` under the content root.
pub fn bot_path(bot: &str) -> String {
    format!("bots/{bot}/BOT.md")
}

/// The `BOT.md` for a new member's personal bot: made from the `pm-assistant` role, owned by
/// the member, reading as them.
pub fn render_personal_bot(handle: &str) -> String {
    let name = personal_bot_name(handle);
    format!(
        "---\n\
         name: {name}\n\
         role: pm-assistant\n\
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
    use crate::model::{Role, TeamFile};

    fn member(handle: &str, slack: &str) -> TeamMember {
        TeamMember {
            handle: handle.into(),
            slack_id: slack.into(),
            email: Some(format!("{handle}+x@acme.com")),
            github: Some("Ana-F".into()),
            roles: vec![Role::Pm, Role::SkillAuthor],
        }
    }

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
    }

    #[test]
    fn handles_come_from_the_email_and_avoid_taken_ones() {
        let mut taken = BTreeSet::new();
        assert_eq!(
            propose_handle("Ana.Kumar@acme.com", "", &taken),
            "ana-kumar"
        );
        taken.insert("ana-kumar".to_string());
        assert_eq!(
            propose_handle("ana.kumar@acme.com", "", &taken),
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
    fn team_yaml_round_trips_and_keeps_the_header() {
        let original = "# Who may use PM OS.\n#   - handle: example\nmembers: []\n";
        let members = vec![
            member("ana", "U012AB3CD"),
            TeamMember {
                handle: "raj".into(),
                slack_id: "W99".into(),
                email: Some("we\"ird\\@x.com".into()),
                github: None,
                roles: vec![Role::Admin],
            },
        ];
        let text = render_team_yaml(original, &members);
        assert!(text.starts_with("# Who may use PM OS.\n#   - handle: example\nmembers:\n"));
        assert_eq!(TeamFile::from_yaml(&text).unwrap().members, members);
        // Rendering again changes nothing.
        assert_eq!(render_team_yaml(&text, &members), text);
        // Removing everyone leaves an empty list.
        let empty = render_team_yaml(&text, &[]);
        assert!(empty.ends_with("members: []\n"));
        assert!(TeamFile::from_yaml(&empty).unwrap().members.is_empty());
    }

    #[test]
    fn the_personal_bot_parses_as_a_personal_bot() {
        let text = render_personal_bot("ana");
        let doc = crate::frontmatter::split(&text).expect("front matter");
        let fm: crate::bots::BotFrontMatter = serde_norway::from_str(doc.yaml).expect("parses");
        let body = doc.body;
        assert_eq!(fm.name, "pm-ana");
        assert_eq!(fm.role, "pm-assistant");
        assert_eq!(fm.scope, crate::bots::BotScope::Personal);
        assert_eq!(fm.owner, "ana");
        assert!(body.contains("Work for ana"));
    }
}
