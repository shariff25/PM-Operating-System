//! Categories and their tasks: `evals/categories/<name>/category.yaml` and `tasks/*.yaml`.
//!
//! Loading validates everything and reports every problem at once, each with the file it is in.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grade::normalize_text;

/// A category: one bot, a pass bar, and the tasks it is graded on.
#[derive(Debug, Clone)]
pub struct Category {
    pub name: String,
    pub bot: String,
    /// The fraction of tasks (0–1) that must pass.
    pub pass_bar: f64,
    pub description: String,
    pub tasks: Vec<EvalTask>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CategoryFile {
    name: String,
    bot: String,
    pass_bar: f64,
    description: String,
}

/// One task: what to ask, and what a passing answer looks like.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvalTask {
    pub id: String,
    pub ask: String,
    pub expect: Expect,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Page URLs the answer must link to, matched loosely (see [`crate::grade::normalize_url`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_cite: Vec<String>,
    /// Text the answer must contain, matched loosely (see [`crate::grade::normalize_text`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_include: Vec<String>,
    /// Text the answer must not contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_not_include: Vec<String>,
    /// How many approval items the task must end with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals: Option<u32>,
}

impl Expect {
    fn is_empty(&self) -> bool {
        self.must_cite.is_empty()
            && self.must_include.is_empty()
            && self.must_not_include.is_empty()
            && self.approvals.is_none()
    }
}

/// Every problem found while loading, each as `path: message`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecErrors(pub Vec<String>);

impl fmt::Display for SpecErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0.len();
        writeln!(
            f,
            "{n} problem{} in the eval files:",
            if n == 1 { "" } else { "s" }
        )?;
        for e in &self.0 {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SpecErrors {}

/// Lowercase letters, digits and single hyphens, starting with a letter or digit.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The directory that holds a category.
pub fn category_dir(root: &Path, name: &str) -> PathBuf {
    root.join("categories").join(name)
}

/// The names of every category under `root/categories`, sorted.
pub fn category_names(root: &Path) -> Result<Vec<String>, SpecErrors> {
    let dir = root.join("categories");
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| SpecErrors(vec![format!("{}: can't read: {e}", dir.display())]))?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("category.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

/// Load and validate one category with all of its tasks.
pub fn load_category(root: &Path, name: &str) -> Result<Category, SpecErrors> {
    let dir = category_dir(root, name);
    let mut errors = Vec::new();
    let path = dir.join("category.yaml");
    let shown = path.display().to_string();
    if !is_slug(name) {
        return Err(SpecErrors(vec![format!(
            "category `{name}`: a name is lowercase letters, digits and hyphens"
        )]));
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return Err(SpecErrors(vec![format!(
                "{shown}: can't read: {e} (is `{name}` a category under {}?)",
                root.join("categories").display()
            )]))
        }
    };
    let file: CategoryFile = match serde_norway::from_str(&text) {
        Ok(f) => f,
        Err(e) => return Err(SpecErrors(vec![format!("{shown}: {e}")])),
    };
    if file.name != name {
        errors.push(format!(
            "{shown}: `name` is `{}` but the directory is `{name}`; they must match",
            file.name
        ));
    }
    if !is_slug(&file.bot) {
        errors.push(format!(
            "{shown}: `bot` `{}` is not a bot name (lowercase letters, digits and hyphens)",
            file.bot
        ));
    }
    if !file.pass_bar.is_finite() || !(0.0..=1.0).contains(&file.pass_bar) {
        errors.push(format!(
            "{shown}: `pass_bar` is {}; it is a fraction from 0 to 1, such as 0.8",
            file.pass_bar
        ));
    }
    if file.description.trim().is_empty() {
        errors.push(format!("{shown}: `description` is empty"));
    }

    let tasks_dir = dir.join("tasks");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&tasks_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|x| x.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect(),
        Err(e) => {
            errors.push(format!("{}: can't read: {e}", tasks_dir.display()));
            Vec::new()
        }
    };
    files.sort();
    let mut tasks = Vec::new();
    let mut ids = HashSet::new();
    for file in &files {
        let shown = file.display().to_string();
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{shown}: can't read: {e}"));
                continue;
            }
        };
        let task: EvalTask = match serde_norway::from_str(&text) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{shown}: {e}"));
                continue;
            }
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        validate_task(&task, stem, &shown, &mut errors);
        if !ids.insert(task.id.clone()) {
            errors.push(format!("{shown}: id `{}` is used twice", task.id));
        }
        tasks.push(task);
    }
    if files.is_empty() && tasks_dir.is_dir() {
        errors.push(format!(
            "{}: no tasks; add at least one `<id>.yaml`",
            tasks_dir.display()
        ));
    }
    if !errors.is_empty() {
        return Err(SpecErrors(errors));
    }
    Ok(Category {
        name: file.name,
        bot: file.bot,
        pass_bar: file.pass_bar,
        description: file.description.trim().to_string(),
        tasks,
    })
}

fn validate_task(task: &EvalTask, stem: &str, shown: &str, errors: &mut Vec<String>) {
    if !is_slug(&task.id) {
        errors.push(format!(
            "{shown}: id `{}` must be lowercase letters, digits and hyphens",
            task.id
        ));
    } else if task.id != stem {
        errors.push(format!(
            "{shown}: id `{}` must match the file name (`{}.yaml`)",
            task.id, task.id
        ));
    }
    if task.ask.trim().is_empty() {
        errors.push(format!("{shown}: `ask` is empty"));
    }
    let e = &task.expect;
    if e.is_empty() {
        errors.push(format!(
            "{shown}: `expect` has no checks; give at least one of must_cite, must_include, must_not_include or approvals"
        ));
    }
    for url in &e.must_cite {
        match reqwest::Url::parse(url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
            _ => errors.push(format!(
                "{shown}: must_cite `{url}` is not an absolute http(s) URL"
            )),
        }
    }
    for (field, list) in [
        ("must_include", &e.must_include),
        ("must_not_include", &e.must_not_include),
    ] {
        for t in list {
            if normalize_text(t).is_empty() {
                errors.push(format!("{shown}: {field} has an empty entry"));
            }
        }
    }
    let included: HashSet<String> = e.must_include.iter().map(|t| normalize_text(t)).collect();
    for t in &e.must_not_include {
        if included.contains(&normalize_text(t)) {
            errors.push(format!(
                "{shown}: `{t}` is in both must_include and must_not_include"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(is_slug("docs-qa"));
        assert!(is_slug("q1"));
        assert!(!is_slug("Docs"));
        assert!(!is_slug("-a"));
        assert!(!is_slug("a--b"));
        assert!(!is_slug(""));
        assert!(!is_slug("a_b"));
    }
}
