//! Who may read which content file (`docs/content-admin.md`, Safety): one rule, used by the
//! file read, the tree's lint messages, validation diffs and the tool server alike.
//!
//! An admin reads everything; reading a PM's personal master file
//! (`master/people/<handle>/AGENTS.md`) or context marked `sensitivity: restricted` is
//! [`Sensitive`], and the caller records it in the audit log. An auditor (any viewer without
//! the edit permission) never reads those two.

use std::path::Path;

/// What a file in the content tree is, from its content-relative path.
pub fn kind(rel: &str) -> &'static str {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        ["team.yaml"] => "team",
        ["tools.yaml"] => "tools",
        ["master", "AGENTS.md"] => "master",
        ["master", "people", _, "AGENTS.md"] => "personal_master",
        ["roles", _, "ROLE.md"] => "role",
        ["bots", _, "BOT.md"] => "bot",
        // The bot marketplace's catalog (slice A1): one file per bot.
        ["personas", _, "PERSONA.md"] => "persona",
        ["skills", _, "SKILL.md"] => "skill",
        ["skills", _, ..] => "skill_file",
        [.., "README.md"] => "readme",
        ["playbooks", _] => "playbook",
        ["templates", _] => "template",
        ["reviewers", _] => "reviewer",
        ["context", ..] => "context",
        _ => "other",
    }
}

/// Files an auditor sees listed but may not open: a PM's personal master file, and context
/// marked `restricted` (in any letter case). An admin opens everything. Prefer [`may_read`],
/// which also fails closed on front matter that doesn't parse.
pub fn auditor_may_read(kind: &str, sensitivity: Option<&str>) -> bool {
    kind != "personal_master" && !sensitivity.is_some_and(is_restricted_value)
}

/// `restricted`, ignoring case and surrounding space.
fn is_restricted_value(v: &str) -> bool {
    v.trim().eq_ignore_ascii_case("restricted")
}

/// Is this Markdown text restricted to admins? Its front matter says `sensitivity:
/// restricted` in any letter case, or **can't be read**: front matter that starts but never
/// closes, YAML that doesn't parse, or a `sensitivity` that isn't text all fail closed (review
/// of `b469d5b`, UM3). No front matter at all is not restricted.
pub fn restricted_text(text: &str) -> bool {
    let doc = match pmos_contract::frontmatter::split(text) {
        Ok(doc) => doc,
        Err(pmos_contract::frontmatter::FrontMatterError::Missing) => return false,
        Err(_) => return true,
    };
    let Ok(v) = serde_norway::from_str::<serde_norway::Value>(doc.yaml) else {
        return true;
    };
    match v.get("sensitivity") {
        None | Some(serde_norway::Value::Null) => false,
        Some(serde_norway::Value::String(s)) => is_restricted_value(s),
        Some(_) => true,
    }
}

/// Why reading a file is sensitive, for the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitive {
    /// `master/people/<handle>/AGENTS.md`: whose it is.
    Personal,
    /// Front matter `sensitivity: restricted`.
    Restricted,
}

impl Sensitive {
    /// The audit `detail` note for it.
    pub fn note(self) -> &'static str {
        match self {
            Self::Personal => "personal file",
            Self::Restricted => "restricted context",
        }
    }
}

/// The `sensitivity` in a Markdown file's front matter, if it has one.
pub fn sensitivity(text: &str) -> Option<String> {
    let doc = pmos_contract::frontmatter::split(text).ok()?;
    let v: serde_norway::Value = serde_norway::from_str(doc.yaml).ok()?;
    match v.get("sensitivity")? {
        serde_norway::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Whether reading `rel` (with its current `text`, if any) is sensitive.
pub fn sensitive(rel: &str, text: Option<&str>) -> Option<Sensitive> {
    if kind(rel) == "personal_master" {
        return Some(Sensitive::Personal);
    }
    let restricted = rel.ends_with(".md") && text.is_some_and(restricted_text);
    restricted.then_some(Sensitive::Restricted)
}

/// Whether a viewer may read `rel` (with its current `text`): an admin (`edit`) always, anyone
/// else only when it isn't [`sensitive`].
pub fn may_read(edit: bool, rel: &str, text: Option<&str>) -> bool {
    edit || sensitive(rel, text).is_none()
}

/// [`may_read`] for a file on disk under `content_root` (read through
/// [`super::proposals::read_text`], so never through a symlink). A file that can't be read is
/// treated as not readable for a non-admin.
pub fn may_read_on_disk(edit: bool, content_root: &Path, rel: &str) -> bool {
    if edit {
        return true;
    }
    match super::proposals::read_text(content_root, rel) {
        Ok(text) => may_read(false, rel, text.as_deref()),
        Err(_) => false,
    }
}

/// The message that replaces a finding's on a file the viewer may not read: lint and parser
/// messages can quote a file's values.
pub const REDACTED_MESSAGE: &str = "A finding on a file your role can't open.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind("skills/docs/SKILL.md"), "skill");
        assert_eq!(kind("skills/docs/examples.md"), "skill_file");
        assert_eq!(kind("roles/docs-helper/ROLE.md"), "role");
        assert_eq!(kind("bots/docs/BOT.md"), "bot");
        assert_eq!(kind("personas/chief/PERSONA.md"), "persona");
        assert_eq!(kind("personas/chief/notes.md"), "other");
        assert_eq!(kind("context/products/x.md"), "context");
        assert_eq!(kind("context/products/README.md"), "readme");
        assert_eq!(kind("master/people/ana/AGENTS.md"), "personal_master");
        assert_eq!(kind("playbooks/bar.md"), "playbook");
        assert_eq!(kind("team.yaml"), "team");
    }

    #[test]
    fn auditors_dont_open_personal_or_restricted_files() {
        assert!(auditor_may_read("skill", None));
        assert!(auditor_may_read("context", Some("internal")));
        assert!(!auditor_may_read("context", Some("restricted")));
        assert!(!auditor_may_read("personal_master", None));
    }

    #[test]
    fn sensitive_files_are_told_apart() {
        let restricted = "---\ntitle: R\nsensitivity: restricted\n---\n\nBody\n";
        let internal = "---\ntitle: R\nsensitivity: internal\n---\n\nBody\n";
        assert_eq!(
            sensitive("context/products/r.md", Some(restricted)),
            Some(Sensitive::Restricted)
        );
        assert_eq!(sensitive("context/products/r.md", Some(internal)), None);
        assert_eq!(sensitive("context/products/r.md", None), None);
        assert_eq!(
            sensitive("master/people/ana/AGENTS.md", None),
            Some(Sensitive::Personal)
        );
        assert!(may_read(true, "master/people/ana/AGENTS.md", None));
        assert!(!may_read(false, "master/people/ana/AGENTS.md", None));
        assert!(!may_read(false, "context/products/r.md", Some(restricted)));
        assert!(may_read(false, "context/products/r.md", Some(internal)));
    }

    /// UM3: the rule fails closed. `Restricted` in any case is restricted; so is front matter
    /// that never closes, YAML that doesn't parse, or a `sensitivity` that isn't text. A file
    /// without front matter, or with no `sensitivity`, isn't.
    #[test]
    fn the_read_rule_fails_closed() {
        let rel = "context/products/r.md";
        for text in [
            "---\nsensitivity: Restricted\n---\nBody\n",
            "---\nsensitivity: RESTRICTED \n---\nBody\n",
            "---\nsensitivity: restricted\ntitle: [unclosed\n---\nBody\n",
            "---\nsensitivity: restricted\nBody with no closing line\n",
            "---\nsensitivity: [restricted]\n---\nBody\n",
            "---\nsensitivity: {level: restricted}\n---\n",
            "---\n\t: :\n---\n",
        ] {
            assert_eq!(
                sensitive(rel, Some(text)),
                Some(Sensitive::Restricted),
                "{text:?}"
            );
            assert!(!may_read(false, rel, Some(text)), "{text:?}");
            assert!(may_read(true, rel, Some(text)));
        }
        for text in [
            "# No front matter\n",
            "---\ntitle: T\n---\nBody\n",
            "---\nsensitivity: internal\n---\n",
            "---\nsensitivity:\n---\n",
        ] {
            assert_eq!(sensitive(rel, Some(text)), None, "{text:?}");
        }
        assert!(!auditor_may_read("context", Some("Restricted")));
        assert!(!auditor_may_read("context", Some(" restricted ")));
    }
}
