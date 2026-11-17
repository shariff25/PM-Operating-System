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
/// marked `restricted`. An admin opens everything.
pub fn auditor_may_read(kind: &str, sensitivity: Option<&str>) -> bool {
    kind != "personal_master" && sensitivity != Some("restricted")
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
    let restricted =
        rel.ends_with(".md") && text.and_then(sensitivity).as_deref() == Some("restricted");
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
}
