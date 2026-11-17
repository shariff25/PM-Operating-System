//! The content admin's logic (`docs/content-admin.md`): the content tree with each file's
//! metadata and lint status, one file's text, the deployed revision, and the running harness's
//! facts (bots, their roles and the gateway model each maps to).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use pmos_contract::bots::BotScope;
use pmos_contract::{Report, Severity};
use serde::Serialize;
use serde_json::{json, Value};

use crate::proposals::{self, repo_path, Issue};

/// What a file in the content tree is.
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

/// Loose front matter, for metadata: `None` if the file has none or it doesn't parse.
fn front_matter(text: &str) -> Option<serde_norway::Value> {
    let doc = pmos_contract::frontmatter::split(text).ok()?;
    serde_norway::from_str(doc.yaml).ok()
}

fn fm_str(fm: Option<&serde_norway::Value>, keys: &[&str]) -> Option<String> {
    let mut at = fm?;
    for k in keys {
        at = at.get(*k)?;
    }
    match at {
        serde_norway::Value::String(s) => Some(s.clone()),
        serde_norway::Value::Number(n) => Some(n.to_string()),
        serde_norway::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Files an auditor sees listed but may not open: a PM's personal master file, and context
/// marked `restricted`. An admin opens everything.
pub fn auditor_may_read(kind: &str, sensitivity: Option<&str>) -> bool {
    kind != "personal_master" && sensitivity != Some("restricted")
}

#[derive(Debug, Clone, Serialize)]
pub struct LintStatus {
    /// `ok`, `warning` or `error`.
    pub status: &'static str,
    pub errors: usize,
    pub warnings: usize,
    pub issues: Vec<Issue>,
}

fn status(issues: Vec<Issue>) -> LintStatus {
    let errors = issues.iter().filter(|i| i.severity == "error").count();
    let warnings = issues.len() - errors;
    LintStatus {
        status: if errors > 0 {
            "error"
        } else if warnings > 0 {
            "warning"
        } else {
            "ok"
        },
        errors,
        warnings,
        issues,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub kind: &'static str,
    pub name: Option<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub owner: Option<String>,
    pub sensitivity: Option<String>,
    pub last_reviewed: Option<String>,
    pub size: u64,
    pub editable: bool,
    /// Whether this viewer may open the file's text.
    pub readable: bool,
    pub lint: LintStatus,
}

/// Every regular file in the content tree (no symlinks, nothing hidden), with metadata and
/// its lint status, plus the lint findings that belong to no listed file.
pub fn tree(
    root: &Path,
    report: &Report,
    content_dir: &str,
    admin: bool,
) -> (Vec<Entry>, Vec<Issue>) {
    let mut by_path: BTreeMap<String, Vec<Issue>> = BTreeMap::new();
    for d in &report.diagnostics {
        by_path
            .entry(d.path.clone())
            .or_default()
            .push(Issue::from_lint(d, content_dir));
    }
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'));
    for e in walker.flatten() {
        if !e.file_type().is_file() {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if !proposals::safe_repo_path(&rel) {
            continue;
        }
        let k = kind(&rel);
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let text = if rel.ends_with(".md") && size as usize <= proposals::MAX_FILE_BYTES {
            fs::read_to_string(e.path()).ok()
        } else {
            None
        };
        let fm = text.as_deref().and_then(front_matter);
        let fm = fm.as_ref();
        let sensitivity = fm_str(fm, &["sensitivity"]);
        let (version, owner) = if k == "skill" {
            (
                fm_str(fm, &["metadata", "version"]),
                fm_str(fm, &["metadata", "owner"]),
            )
        } else {
            (fm_str(fm, &["version"]), fm_str(fm, &["owner"]))
        };
        out.push(Entry {
            path: repo_path(content_dir, &rel),
            kind: k,
            name: fm_str(fm, &["name"]),
            title: fm_str(fm, &["title"]),
            version,
            owner: owner.map(|o| o.trim_start_matches('@').to_string()),
            last_reviewed: fm_str(fm, &["last-reviewed"]),
            editable: proposals::writable(&rel).is_ok(),
            readable: admin || auditor_may_read(k, sensitivity.as_deref()),
            sensitivity,
            size,
            lint: status(by_path.remove(&rel).unwrap_or_default()),
        });
    }
    let other = by_path.into_values().flatten().collect();
    (out, other)
}

/// Roles, skills and bots as the lint read them: which roles use which skills, and which bots
/// are made from which role.
pub fn catalog(report: &Report) -> Value {
    let mut used_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in &report.roles {
        for s in &r.skills {
            used_by
                .entry(s.as_str())
                .or_default()
                .insert(r.name.as_str());
        }
    }
    let skills: Vec<Value> = report
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "version": s.version,
                "owner": s.owner,
                "model": s.model,
                "data_class": s.data_class,
                "tools": s.tools,
                "used_by": used_by.get(s.name.as_str()).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let roles: Vec<Value> = report
        .roles
        .iter()
        .map(|r| {
            json!({
                "name": r.name,
                "version": r.version,
                "owner": r.owner,
                "model": r.model,
                "data_class": r.data_class,
                "skills": r.skills,
                "bots": report.bots.iter().filter(|b| b.role == r.name).map(|b| &b.name).collect::<Vec<_>>(),
            })
        })
        .collect();
    let bots: Vec<Value> = report
        .bots
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "role": b.role,
                "scope": match b.scope { BotScope::Team => "team", BotScope::Personal => "personal" },
                "owner": b.owner,
            })
        })
        .collect();
    json!({ "roles": roles, "skills": skills, "bots": bots })
}

/// One file to show: its text and parsed front matter.
#[derive(Debug, Clone, Serialize)]
pub struct FileView {
    pub path: String,
    pub kind: &'static str,
    pub size: usize,
    /// Of the text as served: send it back as `base_sha256` with a change to it.
    pub sha256: String,
    pub text: String,
    pub front_matter: Option<Value>,
    pub front_matter_error: Option<String>,
    pub editable: bool,
}

pub fn view(rel: &str, path: String, text: String) -> FileView {
    let (front_matter, front_matter_error) = if rel.ends_with(".md") {
        match pmos_contract::frontmatter::split(&text) {
            Err(e) => (None, Some(e.to_string())),
            Ok(doc) => match serde_norway::from_str::<Value>(doc.yaml) {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(format!("the front matter doesn't parse: {e}"))),
            },
        }
    } else {
        (None, None)
    };
    FileView {
        path,
        kind: kind(rel),
        size: text.len(),
        sha256: proposals::sha256_hex(text.as_bytes()),
        editable: proposals::writable(rel).is_ok(),
        text,
        front_matter,
        front_matter_error,
    }
}

/// The deployed content's commit, from configuration or from git's files beside the content
/// root (`.git` as a folder or a worktree's `gitdir:` file; loose or packed refs). Never runs
/// git.
pub fn revision(content_root: &Path, configured: Option<&str>) -> Value {
    if let Some(r) = configured {
        return json!({"commit": r, "source": "config"});
    }
    match git_head(content_root) {
        Some(sha) => json!({"commit": sha, "source": "git"}),
        None => json!({"commit": null, "source": null}),
    }
}

fn git_head(content_root: &Path) -> Option<String> {
    let start = content_root.canonicalize().ok()?;
    let mut at: Option<&Path> = Some(&start);
    let git = loop {
        let dir = at?;
        let g = dir.join(".git");
        if g.exists() {
            break g;
        }
        at = dir.parent();
    };
    let git_dir = if git.is_file() {
        let text = fs::read_to_string(&git).ok()?;
        let p = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if p.is_absolute() {
            p
        } else {
            git.parent()?.join(p)
        }
    } else {
        git
    };
    let common = fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| {
            let p = PathBuf::from(c.trim());
            if p.is_absolute() {
                p
            } else {
                git_dir.join(p)
            }
        })
        .unwrap_or_else(|| git_dir.clone());
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let sha = match head.strip_prefix("ref:") {
        None => head.to_string(),
        Some(r) => {
            let r = r.trim();
            if !r.starts_with("refs/") || r.contains("..") {
                return None;
            }
            [&git_dir, &common]
                .iter()
                .find_map(|d| fs::read_to_string(d.join(r)).ok())
                .map(|s| s.trim().to_string())
                .or_else(|| {
                    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
                    packed.lines().find_map(|l| {
                        let (sha, name) = l.split_once(' ')?;
                        (name.trim() == r).then(|| sha.to_string())
                    })
                })?
        }
    };
    crate::admin_config::is_sha(&sha).then(|| sha.to_ascii_lowercase())
}

/// The gateway's model roles, names only: `{role: {model, upstream, workarounds}}`. Upstream
/// addresses and keys are never read out.
pub fn gateway_roles(path: &Path) -> Result<BTreeMap<String, Value>, &'static str> {
    let text = fs::read_to_string(path).map_err(|_| "unreadable")?;
    let v: serde_norway::Value = serde_norway::from_str(&text).map_err(|_| "unreadable")?;
    let roles = v
        .get("roles")
        .and_then(|r| r.as_mapping())
        .ok_or("unreadable")?;
    let mut out = BTreeMap::new();
    for (k, r) in roles {
        let Some(name) = k.as_str() else { continue };
        let s = |key: &str| r.get(key).and_then(|v| v.as_str()).map(String::from);
        let workarounds: Vec<String> = r
            .get("workarounds")
            .and_then(|w| w.as_sequence())
            .map(|w| {
                w.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            name.to_string(),
            json!({
                "alias": format!("pmos/{name}"),
                "model": s("model"),
                "upstream": s("upstream"),
                "workarounds": workarounds,
            }),
        );
    }
    Ok(out)
}

/// Lint issues as the API serves them, errors and warnings counted.
pub fn lint_summary(report: &Report) -> Value {
    json!({
        "errors": report.diagnostics.iter().filter(|d| d.severity == Severity::Error).count(),
        "warnings": report.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count(),
        "files_checked": report.files_checked,
    })
}

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
    fn the_gateway_config_gives_names_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gateway.yaml");
        fs::write(
            &p,
            "upstreams:\n  qwen: http://secret-host:8000/v1\nroles:\n  default:\n    upstream: qwen\n    model: qwen3\n    workarounds: [strip-think]\n",
        )
        .unwrap();
        let roles = gateway_roles(&p).unwrap();
        assert_eq!(roles["default"]["model"], "qwen3");
        assert_eq!(roles["default"]["upstream"], "qwen");
        assert!(!serde_json::to_string(&roles)
            .unwrap()
            .contains("secret-host"));
        assert!(gateway_roles(&dir.path().join("none.yaml")).is_err());
    }

    #[test]
    fn the_revision_comes_from_git_files() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        fs::create_dir_all(dir.path().join(".git/refs/heads")).unwrap();
        fs::create_dir_all(dir.path().join("content")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(dir.path().join(".git/refs/heads/main"), format!("{sha}\n")).unwrap();
        assert_eq!(revision(&dir.path().join("content"), None)["commit"], sha);
        fs::remove_file(dir.path().join(".git/refs/heads/main")).unwrap();
        fs::write(
            dir.path().join(".git/packed-refs"),
            format!("# pack-refs\n{sha} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(revision(&dir.path().join("content"), None)["source"], "git");
        assert_eq!(
            revision(&dir.path().join("content"), Some("abc"))["source"],
            "config"
        );
    }
}
