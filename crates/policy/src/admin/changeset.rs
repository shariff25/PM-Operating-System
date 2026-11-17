//! A proposal's change set, as `pmos-web` sends it to the tool server and as the tool server
//! checks it again before it opens a pull request (`docs/policy-admin.md`, How a proposal is
//! opened). The tool server never trusts `pmos-web`'s validation: [`recheck`] applies the
//! path allow-list, the caps, the kind's own limits and the base check, then lints the
//! changed tree and regenerates every policy itself, and the policy files in the change set
//! must be exactly what it regenerated.
//!
//! **The allow-list.** A path is either a content file (`<content_dir>/…`, a plain path, a
//! text type, not `team.yaml`) or a generated policy file (`<policies_dir>/bots/<bot>.yaml`,
//! `<policies_dir>/bots/<bot>.sandbox.yaml`, `<policies_dir>/providers/<profile>.yaml`).
//! Nothing else: never `.github/`, `deploy/`, code, a hidden file or the repository root.
//! [`Layout::new`] refuses an empty (repository root) content or policies directory, and one
//! inside the other, so neither setting can make the whole repository writable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::access;
use super::evaluate::{self, Evaluation, Roots};
use super::proposals::{self, git_blob_sha, repo_path, Change, Issue, Kind, Refusal};

/// The most generated policy files one proposal may carry, beyond its content files.
pub const MAX_POLICY_FILES: usize = 150;

/// Where the content tree and the generated policies are in the repository. Neither is the
/// repository root, and neither is inside the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    content_dir: String,
    policies_dir: String,
}

impl Layout {
    /// `content_dir` and `policies_dir` (`content`, `policies`): plain relative folders, not
    /// empty, not nested. `Err` says which setting is wrong, never more.
    pub fn new(content_dir: &str, policies_dir: &str) -> Result<Self, String> {
        let clean = |d: &str| d.trim().trim_matches('/').to_string();
        let (c, p) = (clean(content_dir), clean(policies_dir));
        if c.is_empty() || c == "." {
            return Err("the content directory can't be the repository root: a proposal could then write anywhere in the repository".into());
        }
        if p.is_empty() || p == "." {
            return Err("the policies directory can't be the repository root".into());
        }
        if !proposals::safe_repo_path(&c) || !proposals::safe_repo_path(&p) {
            return Err(
                "the content and policies directories must be plain relative folders".into(),
            );
        }
        let inside = |a: &str, b: &str| a == b || a.starts_with(&format!("{b}/"));
        if inside(&c, &p) || inside(&p, &c) {
            return Err("the content and policies directories can't be inside each other".into());
        }
        Ok(Self {
            content_dir: c,
            policies_dir: p,
        })
    }

    pub fn content_dir(&self) -> &str {
        &self.content_dir
    }

    pub fn policies_dir(&self) -> &str {
        &self.policies_dir
    }
}

/// What a path in a change set is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A content file, content-relative.
    Content(String),
    /// A generated policy file, relative to the policies directory.
    Policy(String),
}

fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
}

/// A file `pmos policy generate` writes: `bots/<bot>.yaml`, `bots/<bot>.sandbox.yaml` or
/// `providers/<profile>.yaml`, with plain lowercase names.
pub fn generated_policy_rel(rel: &str) -> bool {
    match rel.split('/').collect::<Vec<_>>().as_slice() {
        ["bots", file] => file
            .strip_suffix(".sandbox.yaml")
            .or_else(|| file.strip_suffix(".yaml"))
            .is_some_and(plain_name),
        ["providers", file] => file.strip_suffix(".yaml").is_some_and(plain_name),
        _ => false,
    }
}

/// The allow-list: a content file or a generated policy file, else why not.
pub fn classify(layout: &Layout, path: &str) -> Result<Target, Refusal> {
    if let Some(rel) = path
        .strip_prefix(layout.policies_dir())
        .and_then(|r| r.strip_prefix('/'))
    {
        if proposals::safe_repo_path(rel) && generated_policy_rel(rel) {
            return Ok(Target::Policy(rel.to_string()));
        }
        return Err(Refusal::new(
            "path_not_allowed",
            "Only the generated policy files (bots/<bot>.yaml, bots/<bot>.sandbox.yaml, providers/<profile>.yaml) can be changed in the policies folder.",
        ));
    }
    let rel = proposals::content_rel(path, layout.content_dir())?;
    proposals::writable(&rel)?;
    Ok(Target::Content(rel))
}

/// One file of a change set, as sent to the tool server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedFile {
    /// A repository path.
    pub path: String,
    /// The whole new text; `None` with `delete: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete: bool,
    /// The git blob id of the file as the sender computed the change from; `None`: the file
    /// didn't exist there.
    #[serde(default)]
    pub base_blob_sha: Option<String>,
}

/// The change set for `content` (what the admin changed) and its evaluation: the content
/// files, then the regenerated policy files, each with the blob id it was computed from.
pub fn proposed_files(layout: &Layout, content: &[Change], e: &Evaluation) -> Vec<ProposedFile> {
    let base = |path: &str| {
        e.base_texts
            .get(path)
            .and_then(|t| t.as_ref())
            .map(|t| git_blob_sha(t.as_bytes()))
    };
    let mut out: Vec<ProposedFile> = content
        .iter()
        .map(|c| {
            let path = repo_path(layout.content_dir(), &c.rel);
            ProposedFile {
                base_blob_sha: base(&path),
                delete: c.text.is_none(),
                text: c.text.clone(),
                path,
            }
        })
        .collect();
    out.extend(e.policy_writes.iter().map(|(rel, text)| {
        let path = repo_path(layout.policies_dir(), rel);
        ProposedFile {
            base_blob_sha: base(&path),
            delete: text.is_none(),
            text: text.clone(),
            path,
        }
    }));
    out
}

/// Why a change set was refused: an API `code` and `status`, a sentence, and for some codes
/// the paths or the findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejection {
    #[serde(skip)]
    pub status: u16,
    pub code: &'static str,
    #[serde(rename = "error")]
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<Issue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Issue>,
}

impl Rejection {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            paths: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn refused(r: Refusal, path: &str) -> Self {
        let status = if r.code == "too_large" { 413 } else { 400 };
        Self::new(status, r.code, format!("{path}: {}", r.message))
    }

    /// The audit note for it (a fixed phrase, never the message).
    pub fn note(&self) -> &'static str {
        match self.code {
            "stale_base" => "stale base",
            "policy_mismatch" => "policy mismatch",
            _ => "invalid",
        }
    }
}

/// A change set that passed [`recheck`]: what to commit, in order (content files, then
/// policy files), and the evaluation.
pub struct Checked {
    /// Repository path, and the new text (`None`: delete).
    pub files: Vec<(String, Option<String>)>,
    /// The text each file has in the deployed copy (`None`: absent), by repository path: what
    /// the stale-base check compares the default branch with.
    pub base_texts: BTreeMap<String, Option<String>>,
    pub evaluation: Evaluation,
}

/// Where the checking server's own deployed copy is.
pub struct Deployed<'a> {
    pub content_root: &'a Path,
    pub policies_root: Option<&'a Path>,
}

/// Check `files` again, from nothing: the allow-list and caps, the kind's limits, that each
/// file is what the sender computed from (against this server's own deployed copy), the
/// content lint and the policy rules on the changed tree, and that the policy files are
/// exactly what this server regenerates.
pub fn recheck(
    kind: Kind,
    layout: &Layout,
    deployed: &Deployed,
    files: &[ProposedFile],
    today: NaiveDate,
) -> Result<Checked, Rejection> {
    if files.is_empty() {
        return Err(Rejection::new(400, "no_change", "There are no changes."));
    }
    let mut content = Vec::new();
    let mut policy: BTreeMap<String, &ProposedFile> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for f in files {
        if !seen.insert(f.path.as_str()) {
            return Err(Rejection::new(
                400,
                "bad_request",
                format!("{}: listed twice.", f.path),
            ));
        }
        let text = match (&f.text, f.delete) {
            (Some(t), false) => {
                proposals::check_text(t).map_err(|r| Rejection::refused(r, &f.path))?;
                Some(t.clone())
            }
            (None, true) => None,
            _ => {
                return Err(Rejection::new(
                    400,
                    "bad_request",
                    format!("{}: give either `text` or `delete: true`.", f.path),
                ))
            }
        };
        if let Some(sha) = &f.base_blob_sha {
            if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Rejection::new(
                    400,
                    "bad_request",
                    format!("{}: `base_blob_sha` is a git blob id.", f.path),
                ));
            }
        }
        match classify(layout, &f.path).map_err(|r| Rejection::refused(r, &f.path))? {
            Target::Content(rel) => {
                if kind == Kind::Policy && !matches!(access::kind(&rel), "bot" | "role" | "skill") {
                    return Err(Rejection::new(
                        400,
                        "path_not_allowed",
                        format!(
                            "{}: a policy proposal changes only bot, role and skill definitions; make this a content proposal.",
                            f.path
                        ),
                    ));
                }
                content.push((rel, text, f));
            }
            Target::Policy(rel) => {
                policy.insert(rel, f);
            }
        }
    }
    if content.len() > proposals::MAX_FILES {
        return Err(Rejection::new(
            413,
            "too_many_files",
            format!("A proposal changes at most {} files.", proposals::MAX_FILES),
        ));
    }
    if policy.len() > MAX_POLICY_FILES {
        return Err(Rejection::new(
            413,
            "too_many_files",
            format!("A proposal carries at most {MAX_POLICY_FILES} policy files."),
        ));
    }

    // Each content file is what the sender computed from, here too.
    let mut stale = Vec::new();
    let mut changes = Vec::new();
    for (rel, text, f) in content {
        let current = proposals::read_text(deployed.content_root, &rel)
            .map_err(|r| Rejection::refused(r, &f.path))?;
        let have = current.as_ref().map(|t| git_blob_sha(t.as_bytes()));
        if have != f.base_blob_sha.as_ref().map(|s| s.to_ascii_lowercase()) {
            stale.push(f.path.clone());
            continue;
        }
        if current.is_none() && text.is_none() {
            return Err(Rejection::new(
                400,
                "not_found",
                format!("{}: doesn't exist, so it can't be deleted.", f.path),
            ));
        }
        if current == text {
            continue;
        }
        changes.push(Change { rel, text });
    }
    if !stale.is_empty() {
        let mut r = Rejection::new(
            409,
            "stale_base",
            "These files differ between the copy this change was made from and the tool server's deployed copy. Wait for both to deploy the same revision, then try again.",
        );
        r.paths = stale;
        return Err(r);
    }
    if changes.is_empty() && policy.is_empty() {
        return Err(Rejection::new(
            400,
            "no_change",
            "Every file is the same as the deployed one: there's nothing to propose.",
        ));
    }

    let roots = Roots {
        content: deployed.content_root,
        policies: deployed.policies_root,
        content_dir: layout.content_dir(),
        policies_dir: layout.policies_dir(),
    };
    let e = evaluate::evaluate(&roots, &changes, today)
        .map_err(|i| Rejection::new(400, "unavailable", i.message))?;
    if !e.valid {
        let (errors, warnings): (Vec<Issue>, Vec<Issue>) = e
            .issues
            .iter()
            .cloned()
            .partition(|i| i.severity == "error");
        let mut r = Rejection::new(
            422,
            "invalid",
            "This change doesn't pass validation; nothing was proposed.",
        );
        r.errors = errors;
        r.warnings = warnings;
        return Err(r);
    }

    // The policy files must be exactly what this server regenerates, from the same base.
    let regenerated: BTreeMap<&String, &Option<String>> =
        e.policy_writes.iter().map(|(rel, t)| (rel, t)).collect();
    let mut mismatch: Vec<String> = Vec::new();
    for (rel, text) in &regenerated {
        let path = repo_path(layout.policies_dir(), rel);
        let base = e
            .base_texts
            .get(&path)
            .and_then(|t| t.as_ref())
            .map(|t| git_blob_sha(t.as_bytes()));
        match policy.get(*rel) {
            Some(f)
                if &&f.text == text
                    && f.base_blob_sha.as_ref().map(|s| s.to_ascii_lowercase()) == base => {}
            _ => mismatch.push(path),
        }
    }
    for (rel, f) in &policy {
        if !regenerated.contains_key(rel) {
            mismatch.push(f.path.clone());
        }
    }
    if !mismatch.is_empty() {
        mismatch.sort();
        mismatch.dedup();
        let mut r = Rejection::new(
            422,
            "policy_mismatch",
            "The policy files in this change aren't what the tool server regenerates from it. The two servers may run different content or code: wait for both to deploy the same revision.",
        );
        r.paths = mismatch;
        return Err(r);
    }
    if changes.is_empty() && e.policy_writes.is_empty() {
        return Err(Rejection::new(
            400,
            "no_change",
            "Every file is the same as the deployed one: there's nothing to propose.",
        ));
    }

    let mut out: Vec<(String, Option<String>)> = changes
        .iter()
        .map(|c| (repo_path(layout.content_dir(), &c.rel), c.text.clone()))
        .collect();
    out.extend(
        e.policy_writes
            .iter()
            .map(|(rel, t)| (repo_path(layout.policies_dir(), rel), t.clone())),
    );
    Ok(Checked {
        files: out,
        base_texts: e.base_texts.clone(),
        evaluation: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new("content", "policies").unwrap()
    }

    #[test]
    fn the_repository_root_is_never_a_writable_folder() {
        for (c, p) in [
            ("", "policies"),
            ("/", "policies"),
            (".", "policies"),
            ("content", ""),
            ("content", "./"),
            ("content", "content"),
            ("content", "content/policies"),
            ("policies/content", "policies"),
            ("../x", "policies"),
            ("content", ".github"),
        ] {
            assert!(Layout::new(c, p).is_err(), "{c:?} {p:?}");
        }
        let l = Layout::new("/content/", "policies").unwrap();
        assert_eq!((l.content_dir(), l.policies_dir()), ("content", "policies"));
        assert!(Layout::new("pmos/content", "pmos/policies").is_ok());
    }

    #[test]
    fn the_allow_list() {
        let l = layout();
        assert_eq!(
            classify(&l, "content/skills/docs/SKILL.md").unwrap(),
            Target::Content("skills/docs/SKILL.md".into())
        );
        assert_eq!(
            classify(&l, "policies/bots/docs.yaml").unwrap(),
            Target::Policy("bots/docs.yaml".into())
        );
        assert_eq!(
            classify(&l, "policies/bots/docs.sandbox.yaml").unwrap(),
            Target::Policy("bots/docs.sandbox.yaml".into())
        );
        assert_eq!(
            classify(&l, "policies/providers/pmos-gateway.yaml").unwrap(),
            Target::Policy("providers/pmos-gateway.yaml".into())
        );
        for bad in [
            ".github/workflows/ci.yml",
            "deploy/compose.yaml",
            "crates/web/src/app.rs",
            "Cargo.toml",
            "README.md",
            "content/team.yaml",
            "content/.hidden.md",
            "content/skills/docs/run.sh",
            "policies/README.md",
            "policies/bots/Docs.yaml",
            "policies/bots/x/y.yaml",
            "policies/other/x.yaml",
            "policies/../deploy/x.yaml",
            "content/../deploy/compose.yaml",
            "content",
            "",
        ] {
            assert!(classify(&l, bad).is_err(), "{bad}");
        }
    }
}
