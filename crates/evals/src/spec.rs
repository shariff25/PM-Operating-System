//! Categories and their tasks: `evals/categories/<name>/category.yaml` and `tasks/*.yaml`.
//!
//! Loading validates everything and reports every problem at once, each with the file it is in.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grade::normalize_text;

/// The `bot:` value that means "the personal bot of whoever the tasks are for".
pub const PERSONAL_BOT: &str = "personal";

/// The levels a category may run at (`docs/staff-principal-spec.md` §6).
pub const LEVELS: [&str; 3] = ["senior", "staff", "principal"];

/// A category: one bot, a pass bar, and the tasks it is graded on.
#[derive(Debug, Clone)]
pub struct Category {
    pub name: String,
    /// A bot name, or [`PERSONAL_BOT`]; see [`resolve_bot`].
    pub bot: String,
    /// The fraction of attempts (0–1) that must pass. `None`: the category reports only.
    pub pass_bar: Option<f64>,
    /// How many times each task runs (at least 1).
    pub repeats: u32,
    /// The level every task runs at (`senior`, `staff` or `principal`), sent as the task's
    /// own `level`. `None`: the requester's setting in the runtime.
    pub level: Option<String>,
    /// The marketplace bot (persona) every task asks, sent as the task's `persona`
    /// (`docs/bot-marketplace-spec.md` §10). Only with `bot: personal`; `pmos-evals check` checks
    /// the key against the content's catalog. `None`: Nudge.
    pub persona: Option<String>,
    pub description: String,
    pub tasks: Vec<EvalTask>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CategoryFile {
    name: String,
    bot: String,
    #[serde(default)]
    pass_bar: Option<f64>,
    #[serde(default)]
    repeats: Option<u32>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    persona: Option<String>,
    description: String,
}

/// The bot a category's tasks go to: its `bot`, or for `bot: personal` the personal bot of
/// `requested_by`, `pm-<requested_by>` (with `.` and `_` as hyphens, as onboarding names it).
pub fn resolve_bot(bot: &str, requested_by: &str) -> String {
    if bot == PERSONAL_BOT {
        pmos_contract::onboarding::personal_bot_name(requested_by)
    } else {
        bot.to_string()
    }
}

/// One task: what to ask, and what a passing answer looks like.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvalTask {
    pub id: String,
    /// The one request of a single-turn task. A task has `ask` and `expect`, or `thread`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ask: String,
    #[serde(default, skip_serializing_if = "Expect::is_empty")]
    pub expect: Expect,
    /// A conversation: the turns, in order, each its own runtime task in one conversation.
    /// The task passes only if every turn that has an `expect` passes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread: Vec<Turn>,
    /// Tasks in one cluster share a source, so they are not independent; the standard error
    /// is clustered by it. Default: the first `must_cite` URL, else the task id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    /// A task that once broke and was fixed. The gate fails if it flips back to failing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub regression: bool,
    /// Failure modes from `evals/taxonomy.yaml` this task is there to catch, such as
    /// `model/answered-beyond-sources`. Informational; `check` validates them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// One turn of a thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub ask: String,
    /// What this turn's reply must be. A turn without one only has to settle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
}

impl EvalTask {
    /// Whether this task is a conversation (`thread`) and not a single `ask`.
    pub fn is_thread(&self) -> bool {
        !self.thread.is_empty()
    }

    /// The first thing asked: `ask`, or the thread's first turn.
    pub fn first_ask(&self) -> &str {
        self.thread.first().map_or(self.ask.as_str(), |t| &t.ask)
    }

    /// The task's cluster: `cluster`, else its first `must_cite` URL, else its id.
    pub fn cluster(&self) -> String {
        self.cluster
            .clone()
            .or_else(|| self.expect.must_cite.first().cloned())
            .unwrap_or_else(|| self.id.clone())
    }
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
    /// The answer must say it can't answer from its sources: it contains one of the
    /// `decline_phrases` in `evals/config.yaml`. Such a task has no `must_cite`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_decline: bool,
    /// Checks on the task's drafts, one entry per tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drafts: Vec<DraftCheck>,
}

impl Expect {
    pub fn is_empty(&self) -> bool {
        self.must_cite.is_empty()
            && self.must_include.is_empty()
            && self.must_not_include.is_empty()
            && self.approvals.is_none()
            && !self.must_decline
            && self.drafts.is_empty()
    }
}

/// Checks on one tool's drafts: the approval items the task ended with, read from
/// `GET /v1/tasks/{id}/drafts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DraftCheck {
    /// The registry name, such as `jira.create_issue`.
    pub tool: String,
    /// Arguments every draft of this tool must have, not empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_fields: Vec<String>,
    /// For each argument named, the values it may have, such as `project: [PM]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allowed: BTreeMap<String, Vec<String>>,
    /// Text every draft of this tool must contain (in its string arguments).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_include: Vec<String>,
    /// Text no draft of this tool may contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_not_include: Vec<String>,
    /// How many drafts of this tool there must be. Default: at least one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
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
pub fn is_slug(s: &str) -> bool {
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
    if let Some(bar) = file.pass_bar {
        if !bar.is_finite() || !(0.0..=1.0).contains(&bar) {
            errors.push(format!(
                "{shown}: `pass_bar` is {bar}; it is a fraction from 0 to 1, such as 0.8"
            ));
        }
    }
    if file.repeats == Some(0) {
        errors.push(format!(
            "{shown}: `repeats` is 0; each task runs at least once"
        ));
    }
    if let Some(level) = &file.level {
        if !LEVELS.contains(&level.as_str()) {
            errors.push(format!(
                "{shown}: `level` is `{level}`; use senior, staff or principal"
            ));
        }
    }
    if let Some(persona) = &file.persona {
        if !is_slug(persona) || persona.len() > 32 {
            errors.push(format!(
                "{shown}: `persona` `{persona}` is not a persona key (lowercase letters, digits and hyphens, at most 32)"
            ));
        }
        if file.bot != PERSONAL_BOT {
            errors.push(format!(
                "{shown}: `persona` needs `bot: {PERSONAL_BOT}`; a marketplace bot runs only on a PM's personal bot"
            ));
        }
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
        repeats: file.repeats.unwrap_or(1).max(1),
        level: file.level,
        persona: file
            .persona
            .filter(|p| p != pmos_contract::personas::ALWAYS_ON),
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
    if task.is_thread() {
        if !task.ask.is_empty() {
            errors.push(format!(
                "{shown}: a task has either `ask` or `thread`, not both; move `ask` into the thread's first turn"
            ));
        }
        if !task.expect.is_empty() {
            errors.push(format!(
                "{shown}: a thread task has no top-level `expect`; give each turn its own"
            ));
        }
        if task.thread.len() < 2 {
            errors.push(format!(
                "{shown}: a `thread` has at least two turns; a single request is an `ask`"
            ));
        }
        if task.thread.iter().all(|t| t.expect.is_none()) {
            errors.push(format!(
                "{shown}: no turn of the `thread` has an `expect`; nothing would be graded"
            ));
        }
        for (i, turn) in task.thread.iter().enumerate() {
            let at = format!("{shown}: thread turn {}", i + 1);
            if turn.ask.trim().is_empty() {
                errors.push(format!("{at}: `ask` is empty"));
            }
            if let Some(e) = &turn.expect {
                validate_expect(e, &at, errors);
            }
        }
    } else {
        if task.ask.trim().is_empty() {
            errors.push(format!(
                "{shown}: `ask` is empty; a task has `ask` (one request) or `thread` (a conversation)"
            ));
        }
        validate_expect(&task.expect, shown, errors);
    }
    if task.cluster.as_deref().is_some_and(|c| c.trim().is_empty()) {
        errors.push(format!("{shown}: `cluster` is empty"));
    }
}

/// The checks of one `expect`, at `shown` (a file, or a file and a turn).
fn validate_expect(e: &Expect, shown: &str, errors: &mut Vec<String>) {
    if e.is_empty() {
        errors.push(format!(
            "{shown}: `expect` has no checks; give at least one of must_cite, must_include, must_not_include, approvals, must_decline or drafts"
        ));
    }
    if e.must_decline && !e.must_cite.is_empty() {
        errors.push(format!(
            "{shown}: a `must_decline` task has no `must_cite`: no page answers it"
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
    let mut tools = HashSet::new();
    for d in &e.drafts {
        validate_draft_check(d, shown, errors);
        if !tools.insert(d.tool.as_str()) {
            errors.push(format!(
                "{shown}: drafts has `{}` twice; give one entry per tool",
                d.tool
            ));
        }
    }
}

/// A registry tool name: `<system>.<action>`, in lowercase letters, digits and `_`.
fn is_tool_name(s: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    matches!(s.split_once('.'), Some((a, b)) if part(a) && part(b))
}

fn validate_draft_check(d: &DraftCheck, shown: &str, errors: &mut Vec<String>) {
    let tool = &d.tool;
    if !is_tool_name(tool) {
        errors.push(format!(
            "{shown}: drafts tool `{tool}` is not a registry name such as `jira.create_issue`"
        ));
    }
    if d.require_fields.iter().any(|f| f.trim().is_empty()) {
        errors.push(format!(
            "{shown}: drafts `{tool}`: require_fields has an empty entry"
        ));
    }
    for (field, values) in &d.allowed {
        if values.is_empty() {
            errors.push(format!(
                "{shown}: drafts `{tool}`: allowed `{field}` lists no values"
            ));
        }
    }
    for (field, list) in [
        ("must_include", &d.must_include),
        ("must_not_include", &d.must_not_include),
    ] {
        if list.iter().any(|t| normalize_text(t).is_empty()) {
            errors.push(format!(
                "{shown}: drafts `{tool}`: {field} has an empty entry"
            ));
        }
    }
    let checks_content = !d.require_fields.is_empty()
        || !d.allowed.is_empty()
        || !d.must_include.is_empty()
        || !d.must_not_include.is_empty();
    if d.count == Some(0) && checks_content {
        errors.push(format!(
            "{shown}: drafts `{tool}`: `count: 0` means no drafts, so its other checks never run"
        ));
    }
}

/// Drafts checks that name a tool `content/tools.yaml` doesn't list as a write tool, each as
/// `path: message`. `check` runs this when it can read the registry.
pub fn check_draft_tools(
    root: &Path,
    category: &Category,
    registry: &pmos_contract::model::ToolRegistry,
) -> Vec<String> {
    let mut errors = Vec::new();
    for task in &category.tasks {
        for d in &task.expect.drafts {
            let is_write = registry
                .get(&d.tool)
                .is_some_and(|t| t.kind == pmos_contract::model::ToolKind::Write);
            if !is_write {
                errors.push(format!(
                    "{}: drafts tool `{}` is not a write tool in content/tools.yaml",
                    category_dir(root, &category.name)
                        .join("tasks")
                        .join(format!("{}.yaml", task.id))
                        .display(),
                    d.tool
                ));
            }
        }
    }
    errors
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

    #[test]
    fn tool_names() {
        assert!(is_tool_name("jira.create_issue"));
        assert!(!is_tool_name("jira"));
        assert!(!is_tool_name("Jira.create"));
        assert!(!is_tool_name(".x"));
    }

    #[test]
    fn personal_resolves_to_the_requesters_bot() {
        assert_eq!(resolve_bot("personal", "ana"), "pm-ana");
        assert_eq!(resolve_bot("personal", "ana.lee"), "pm-ana-lee");
        assert_eq!(resolve_bot("docs", "ana"), "docs");
    }

    #[test]
    fn the_cluster_defaults_to_the_first_page_then_the_id() {
        let mut t = EvalTask {
            id: "t".into(),
            ..EvalTask::default()
        };
        assert_eq!(t.cluster(), "t");
        t.expect.must_cite = vec!["https://a/x".into(), "https://a/y".into()];
        assert_eq!(t.cluster(), "https://a/x");
        t.cluster = Some("ports".into());
        assert_eq!(t.cluster(), "ports");
    }
}
