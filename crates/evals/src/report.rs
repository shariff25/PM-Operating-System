//! Reports: `evals/reports/<date>-<category>.md` for people and a `.json` next to it for
//! `history`, `compare` and `gate`.
//!
//! A report has one line per attempt: with `repeats: 3` each task has three lines, numbered by
//! `repeat`. Reports written before repeats have one line per task and read as `repeats: 1`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::grade::Check;
use crate::judge::{JudgeStatus, JudgeVerdict};
use crate::stats::{compare, pass_hat_k, report_se, summaries, SeMethod};
use crate::tag::{Tag, Tagged};

fn one() -> u32 {
    1
}

/// One run of one category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub category: String,
    /// The bot the tasks went to (for `bot: personal`, the resolved `pm-<handle>`).
    pub bot: String,
    pub date: NaiveDate,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// `None`: the category has no bar yet and reports only.
    #[serde(default)]
    pub pass_bar: Option<f64>,
    pub runtime_url: String,
    pub requested_by: String,
    /// `PMOS_GATEWAY_WORKAROUNDS` in the runner's environment, as the person running it
    /// declared it. The runner can't see the gateway's own setting.
    #[serde(default)]
    pub gateway_workarounds: Option<String>,
    /// Free text saying why this run happened ("weekly", "model swap to …").
    #[serde(default)]
    pub note: Option<String>,
    /// How many times each task ran.
    #[serde(default = "one")]
    pub repeats: u32,
    /// How attempts were kept apart, when they were ([`crate::isolate`]): "a fresh tool server
    /// for every attempt …". `None`: every attempt shared one tool server, so a draft or record
    /// one attempt left could change a later one (reports before 2026-11-22 are all so).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    /// The judges that applied, and whether each was blocking in this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judges: Vec<JudgeStatus>,
    /// Why judges that apply weren't asked (no gateway configured, `--no-judges`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judges_skipped: Option<String>,
    pub tasks: Vec<TaskReport>,
}

/// One turn of a thread attempt. The report keeps no answer text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnReport {
    /// The turn's place in the thread, from 1.
    pub turn: u32,
    pub ask: String,
    /// The turn has an `expect`, so it was graded.
    pub graded: bool,
    /// The turn was never asked, because an earlier turn didn't settle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    pub passed: bool,
    pub task_id: Option<String>,
    pub status: Option<String>,
    pub failures: Vec<Check>,
    pub tag: Option<Tagged>,
    pub queue_secs: Option<f64>,
    pub total_secs: Option<f64>,
}

impl TurnReport {
    /// `pass`, `FAIL`, `not graded` or `not run`.
    pub fn verdict(&self) -> &'static str {
        if self.skipped {
            "not run"
        } else if !self.graded && self.passed {
            "not graded"
        } else if self.passed {
            "pass"
        } else {
            "FAIL"
        }
    }
}

/// One attempt's line in a report. For a thread task, it is the whole thread: `failures` and
/// `tag` are those of the first failed turn, `failed_turn` says which, and `turns` has each.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskReport {
    pub id: String,
    pub ask: String,
    /// Which attempt this is, from 1.
    #[serde(default = "one")]
    pub repeat: u32,
    /// The task's cluster for the standard error; reports from before clusters have none,
    /// and the task id is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    /// The task is marked `regression: true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub regression: bool,
    pub task_id: Option<String>,
    pub status: Option<String>,
    pub passed: bool,
    pub failures: Vec<Check>,
    pub tag: Option<Tagged>,
    pub queue_secs: Option<f64>,
    pub total_secs: Option<f64>,
    pub attempts: u32,
    /// Whether the trace could be read for tagging.
    pub trace_read: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judges: Vec<JudgeVerdict>,
    /// A thread task: each turn's result, in order. Empty for a single `ask`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<TurnReport>,
    /// A thread task that failed: the first turn that failed, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_turn: Option<u32>,
}

/// The sub-tag of an attempt lost to the model provider or the infrastructure: the model
/// endpoint unreachable or busy after the gateway's retries, a restart, or the worker or its
/// sandbox going away (rule `interrupted`, `docs/evals.md`). Not the bot's doing, so the
/// headline shows it apart; the bar still counts it as a failure.
pub const INFRA_LOSS: &str = "harness/interrupted";

/// One task's line in [`Summary`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSummaryLine {
    pub id: String,
    pub passes: u32,
    pub attempts: u32,
    /// `passes / attempts`.
    pub pass_rate: f64,
    /// Every attempt passed.
    pub all_attempts_passed: bool,
    /// Attempts lost to the provider or infrastructure ([`INFRA_LOSS`]).
    pub infra_losses: u32,
}

/// The run's numbers, written into the `.json` beside the lines as `summary` (computed from
/// them, never read back). What each means is in `docs/evals.md`, "Reading a report".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub attempts: usize,
    pub passed: usize,
    /// The average: passes over every attempt. The bar is judged on this.
    pub pass_rate: f64,
    pub tasks: usize,
    /// Tasks where every attempt passed (pass^k).
    pub tasks_all_attempts_passed: usize,
    /// `tasks_all_attempts_passed / tasks`: the stricter number, what a PM who asks k times
    /// sees go right every time.
    pub all_attempts_pass_rate: f64,
    /// Attempts lost to the provider or infrastructure ([`INFRA_LOSS`]).
    pub infra_losses: usize,
    /// The pass rate over the attempts that weren't lost that way: what the bot did when it
    /// got to answer. For information only; the bar uses `pass_rate`.
    pub pass_rate_excluding_infra: Option<f64>,
    /// The bar and whether `pass_rate` met it; `None` with no bar.
    pub pass_bar: Option<f64>,
    pub met_bar: Option<bool>,
    pub by_task: Vec<TaskSummaryLine>,
}

/// The `.json` as written: the report, then its summary.
#[derive(Serialize)]
struct Written<'a> {
    #[serde(flatten)]
    report: &'a Report,
    summary: Summary,
}

/// Whether `passed` of `total` meets `bar`.
pub fn meets(passed: usize, total: usize, bar: f64) -> bool {
    total > 0 && passed as f64 + 1e-9 >= bar * total as f64
}

impl Report {
    /// Attempts that passed.
    pub fn passed(&self) -> usize {
        self.tasks.iter().filter(|t| t.passed).count()
    }

    /// Attempts in all.
    pub fn total(&self) -> usize {
        self.tasks.len()
    }

    pub fn pass_rate(&self) -> f64 {
        if self.tasks.is_empty() {
            0.0
        } else {
            self.passed() as f64 / self.total() as f64
        }
    }

    /// Whether the pass rate over every attempt meets the bar; `None` with no bar.
    pub fn met_bar(&self) -> Option<bool> {
        self.pass_bar
            .map(|bar| meets(self.passed(), self.total(), bar))
    }

    /// Attempts lost to the provider or infrastructure ([`INFRA_LOSS`]).
    pub fn infra_losses(&self) -> usize {
        self.tasks.iter().filter(|t| is_infra_loss(t)).count()
    }

    /// The run's numbers: the average, the all-attempts-pass rate, and the losses that
    /// weren't the bot's.
    pub fn summary(&self) -> Summary {
        let tasks = summaries(self);
        let all = tasks.iter().filter(|t| t.all_passed()).count();
        let infra = self.infra_losses();
        let counted = self.total() - infra;
        Summary {
            attempts: self.total(),
            passed: self.passed(),
            pass_rate: self.pass_rate(),
            tasks: tasks.len(),
            tasks_all_attempts_passed: all,
            all_attempts_pass_rate: pass_hat_k(&tasks).unwrap_or(0.0),
            infra_losses: infra,
            pass_rate_excluding_infra: (counted > 0).then(|| self.passed() as f64 / counted as f64),
            pass_bar: self.pass_bar,
            met_bar: self.met_bar(),
            by_task: tasks
                .iter()
                .map(|t| TaskSummaryLine {
                    id: t.id.clone(),
                    passes: t.passes,
                    attempts: t.runs,
                    pass_rate: t.rate(),
                    all_attempts_passed: t.all_passed(),
                    infra_losses: self
                        .tasks
                        .iter()
                        .filter(|l| l.id == t.id && is_infra_loss(l))
                        .count() as u32,
                })
                .collect(),
        }
    }

    /// Failures per tag, the biggest bottleneck first; ties in [`Tag::ALL`] order.
    pub fn failures_by_tag(&self) -> Vec<(Tag, usize)> {
        let mut counts: BTreeMap<Tag, usize> = BTreeMap::new();
        for t in &self.tasks {
            if let Some(tagged) = &t.tag {
                *counts.entry(tagged.tag).or_default() += 1;
            }
        }
        let mut v: Vec<(Tag, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| {
                let pos = |t: Tag| Tag::ALL.iter().position(|x| *x == t);
                pos(a.0).cmp(&pos(b.0))
            })
        });
        v
    }

    /// Failures per sub-tag, the most first, then by name.
    pub fn failures_by_sub_tag(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for t in &self.tasks {
            if let Some(sub) = t.tag.as_ref().and_then(|x| x.sub.clone()) {
                *counts.entry(sub).or_default() += 1;
            }
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Task ids whose failures have `pred`'s tag, each once, in report order.
    fn ids_where(&self, pred: impl Fn(&Tagged) -> bool) -> String {
        let mut ids: Vec<&str> = Vec::new();
        for t in &self.tasks {
            if t.tag.as_ref().is_some_and(&pred) && !ids.contains(&t.id.as_str()) {
                ids.push(&t.id);
            }
        }
        ids.iter()
            .map(|i| format!("`{i}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The report as markdown.
    pub fn to_markdown(&self) -> String {
        self.to_markdown_with(None)
    }

    /// The report as markdown, compared with `previous` (its file name and report) if given.
    pub fn to_markdown_with(&self, previous: Option<(&str, &Report)>) -> String {
        let mut s = String::new();
        let passed = self.passed();
        let total = self.total();
        let tasks = summaries(self);
        let unit = if self.repeats > 1 {
            format!(
                "attempts passed ({}) over {} tasks run {} times each",
                pct(self.pass_rate()),
                tasks.len(),
                self.repeats
            )
        } else {
            format!("tasks passed ({})", pct(self.pass_rate()))
        };
        let _ = writeln!(s, "# Benchmark: {} ({})\n", self.category, self.date);
        match (self.met_bar(), self.pass_bar) {
            (Some(met), Some(bar)) => {
                let verdict = if met {
                    "**Met the bar.**"
                } else {
                    "**Below the bar.**"
                };
                let _ = writeln!(
                    s,
                    "{verdict} {passed} of {total} {unit}, against a bar of {}.\n",
                    pct(bar)
                );
            }
            _ => {
                let _ = writeln!(
                    s,
                    "**Reports only.** {passed} of {total} {unit}; the category has no pass bar yet.\n"
                );
            }
        }
        let summary = self.summary();
        if self.repeats > 1 {
            let _ = writeln!(
                s,
                "**All attempts passed** for {} of {} tasks ({}): the share of tasks that passed every one of their {} attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.\n",
                summary.tasks_all_attempts_passed,
                summary.tasks,
                pct(summary.all_attempts_pass_rate),
                self.repeats
            );
        }
        if summary.infra_losses > 0 {
            let without = match summary.pass_rate_excluding_infra {
                Some(r) => format!(
                    "without them, {passed} of {} attempts passed ({})",
                    total - summary.infra_losses,
                    pct(r)
                ),
                None => "no attempt was left to grade".to_string(),
            };
            let _ = writeln!(
                s,
                "**Lost to the model provider or infrastructure:** {} attempt{} (`{INFRA_LOSS}`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; {without}. They still count as failures for the bar.\n",
                summary.infra_losses,
                if summary.infra_losses == 1 { "" } else { "s" },
            );
        }
        let _ = writeln!(s, "| | |\n| --- | --- |");
        let _ = writeln!(s, "| Bot | `{}` |", self.bot);
        let _ = writeln!(s, "| Runtime | {} |", self.runtime_url);
        let _ = writeln!(s, "| Asked as | `{}` |", self.requested_by);
        let _ = writeln!(
            s,
            "| Gateway workarounds | {} |",
            match &self.gateway_workarounds {
                Some(v) => format!("`{v}` (declared by the runner's environment)"),
                None => "not declared".to_string(),
            }
        );
        if let Some(note) = &self.note {
            let _ = writeln!(s, "| Note | {} |", cell(note));
        }
        let _ = writeln!(
            s,
            "| Isolation | {} |",
            match &self.isolation {
                Some(how) => cell(how),
                None => "none: every attempt shared one tool server, so a draft one attempt left could change a later one".to_string(),
            }
        );
        let _ = writeln!(
            s,
            "| Ran | {} to {} UTC |",
            self.started_at.format("%Y-%m-%d %H:%M:%S"),
            self.finished_at.format("%H:%M:%S")
        );
        if let Some(se) = report_se(self) {
            let how = match se.method {
                SeMethod::Clustered => format!("clustered over {} clusters", se.clusters),
                SeMethod::Binomial => "binomial".to_string(),
            };
            let _ = writeln!(
                s,
                "| Pass rate (average over attempts) | {} ± {} (standard error, {how}) |",
                pct1(self.pass_rate()),
                pct1(se.se)
            );
        }
        if self.repeats > 1 {
            if let Some(k) = pass_hat_k(&tasks) {
                let all = tasks.iter().filter(|t| t.all_passed()).count();
                let _ = writeln!(
                    s,
                    "| All attempts passed (pass^{}) | {} ({all} of {} tasks passed every run) |",
                    self.repeats,
                    pct(k),
                    tasks.len()
                );
            }
        }
        let unread = self
            .tasks
            .iter()
            .filter(|t| !t.passed && !t.trace_read)
            .count();
        if unread > 0 {
            let _ = writeln!(
                s,
                "| Traces | not readable for {unread} failure(s); those were tagged from status and events only |"
            );
        }

        let _ = writeln!(s, "\n## Failures by tag\n");
        let by_tag = self.failures_by_tag();
        let failed = total - passed;
        if by_tag.is_empty() {
            let _ = writeln!(s, "No failures.");
        } else {
            let (top, n) = by_tag[0];
            let _ = writeln!(
                s,
                "Top bottleneck: **{top}** ({n} of {failed} failure{}).\n",
                if failed == 1 { "" } else { "s" }
            );
            let _ = writeln!(s, "| Tag | Failures | Tasks |\n| --- | --- | --- |");
            for (tag, n) in &by_tag {
                let ids = self.ids_where(|x| x.tag == *tag);
                let _ = writeln!(s, "| {tag} | {n} | {ids} |");
            }
            let subs = self.failures_by_sub_tag();
            if !subs.is_empty() {
                let _ = writeln!(s, "\n| Sub-tag | Failures | Tasks |\n| --- | --- | --- |");
                for (sub, n) in &subs {
                    let ids = self.ids_where(|x| x.sub.as_deref() == Some(sub.as_str()));
                    let _ = writeln!(s, "| `{sub}` | {n} | {ids} |");
                }
            }
        }

        if !self.judges.is_empty() || self.judges_skipped.is_some() {
            let _ = writeln!(s, "\n## Judges\n");
            if let Some(why) = &self.judges_skipped {
                let _ = writeln!(s, "Not asked: {}.\n", cell(why));
            }
            if !self.judges.is_empty() {
                let _ = writeln!(
                    s,
                    "| Judge | Status | Yes | No | Unknown | No verdict |\n| --- | --- | --- | --- | --- | --- |"
                );
                for j in &self.judges {
                    let verdicts = self
                        .tasks
                        .iter()
                        .flat_map(|t| t.judges.iter())
                        .filter(|v| v.name == j.name);
                    let (mut yes, mut no, mut unknown, mut none) = (0, 0, 0, 0);
                    for v in verdicts {
                        match v.verdict {
                            Some(true) => yes += 1,
                            Some(false) => no += 1,
                            None if v.unknown => unknown += 1,
                            None => none += 1,
                        }
                    }
                    let _ = writeln!(
                        s,
                        "| `{}` | {} | {yes} | {no} | {unknown} | {none} |",
                        j.name,
                        cell(&j.reason)
                    );
                }
            }
        }

        let _ = writeln!(s, "\n## Tasks\n");
        if self.repeats > 1 {
            let _ = writeln!(
                s,
                "Passes counts the attempts that passed; All passed says whether every attempt did.\n"
            );
            let _ = writeln!(
                s,
                "| Task | Passes | All passed | Failure tags | Why (latest failure) |\n| --- | --- | --- | --- | --- |"
            );
            for t in &tasks {
                let mine: Vec<&TaskReport> = self.tasks.iter().filter(|l| l.id == t.id).collect();
                let mut tags: BTreeMap<String, usize> = BTreeMap::new();
                for l in &mine {
                    if let Some(x) = &l.tag {
                        *tags.entry(tag_label(x)).or_default() += 1;
                    }
                }
                let tags = tags
                    .iter()
                    .map(|(k, n)| format!("{k} ×{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let why = mine
                    .iter()
                    .rev()
                    .find(|l| !l.passed)
                    .map(|l| why(l))
                    .unwrap_or_default();
                let passes = if t.all_passed() {
                    format!("{}/{}", t.passes, t.runs)
                } else {
                    format!("**{}/{}**", t.passes, t.runs)
                };
                let _ = writeln!(
                    s,
                    "| `{}` | {passes} | {} | {} | {} |",
                    t.id,
                    if t.all_passed() { "yes" } else { "no" },
                    cell(&tags),
                    cell(&why)
                );
            }
        } else {
            let _ = writeln!(
                s,
                "| Task | Result | Tag | Status | Queue (s) | Total (s) | Why |\n| --- | --- | --- | --- | --- | --- | --- |"
            );
            for t in &self.tasks {
                let result = if t.passed { "pass" } else { "**FAIL**" };
                let tag = t
                    .tag
                    .as_ref()
                    .map(|x| format!("{} (`{}`)", tag_label(x), x.rule))
                    .unwrap_or_default();
                let why = if t.passed { String::new() } else { why(t) };
                let _ = writeln!(
                    s,
                    "| `{}` | {result} | {tag} | {} | {} | {} | {} |",
                    t.id,
                    t.status.as_deref().unwrap_or("-"),
                    secs(t.queue_secs),
                    secs(t.total_secs),
                    cell(&why)
                );
            }
        }

        let threads: Vec<&TaskReport> = self.tasks.iter().filter(|t| !t.turns.is_empty()).collect();
        if !threads.is_empty() {
            let _ = writeln!(s, "\n## Threads\n");
            let _ = writeln!(
                s,
                "Each turn is its own runtime task in one conversation. A thread passes only if every graded turn does.\n"
            );
            for t in threads {
                let result = if t.passed { "pass" } else { "**FAIL**" };
                if self.repeats > 1 {
                    let _ = writeln!(s, "- `{}`, run {}: {result}", t.id, t.repeat);
                } else {
                    let _ = writeln!(s, "- `{}`: {result}", t.id);
                }
                for line in turn_lines(t) {
                    let _ = writeln!(s, "  - {}", cell(&line));
                }
            }
        }

        if let Some((name, prev)) = previous {
            let _ = writeln!(s, "\n## Compared with the previous run\n");
            let _ = writeln!(s, "Previous run: `{name}` ({}).\n", prev.date);
            s.push_str(&compare(prev, self).to_markdown());
        }

        let failures: Vec<&TaskReport> = self.tasks.iter().filter(|t| !t.passed).collect();
        if !failures.is_empty() {
            let _ = writeln!(s, "\n## Failure details\n");
            for t in failures {
                if self.repeats > 1 {
                    let _ = writeln!(s, "### `{}`, run {} of {}\n", t.id, t.repeat, self.repeats);
                } else {
                    let _ = writeln!(s, "### `{}`\n", t.id);
                }
                let _ = writeln!(s, "- **Asked:** {}", t.ask);
                if let Some(id) = &t.task_id {
                    let _ = writeln!(s, "- **Task:** `{id}` (attempts: {})", t.attempts);
                }
                if let Some(tagged) = &t.tag {
                    let _ = writeln!(
                        s,
                        "- **Tag:** {} by rule `{}`: {}",
                        tag_label(tagged),
                        tagged.rule,
                        cell(&tagged.reason)
                    );
                }
                if let Some(n) = t.failed_turn {
                    let _ = writeln!(s, "- **Failed at:** turn {n} of {}", t.turns.len());
                }
                for f in &t.failures {
                    let _ = writeln!(s, "- {}", cell(&f.to_string()));
                }
                let _ = writeln!(s);
            }
        }
        s
    }
}

fn is_infra_loss(t: &TaskReport) -> bool {
    !t.passed
        && t.tag
            .as_ref()
            .is_some_and(|x| x.sub.as_deref() == Some(INFRA_LOSS))
}

/// A tag as a report shows it: the sub-tag when there is one, else the bottleneck.
fn tag_label(t: &Tagged) -> String {
    t.sub.clone().unwrap_or_else(|| t.tag.to_string())
}

fn why(t: &TaskReport) -> String {
    let checks = t
        .failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    match t.failed_turn {
        Some(n) => format!("turn {n}: {checks}"),
        None => checks,
    }
}

/// One line per turn of a thread attempt.
fn turn_lines(t: &TaskReport) -> Vec<String> {
    t.turns
        .iter()
        .map(|u| {
            let mut line = format!("turn {}: {}", u.turn, u.verdict());
            if let Some(x) = &u.tag {
                line.push_str(&format!(" ({})", tag_label(x)));
            }
            if let Some(st) = &u.status {
                line.push_str(&format!(", {st}"));
            }
            if let Some(n) = &u.total_secs {
                line.push_str(&format!(", {n:.1} s"));
            }
            if !u.failures.is_empty() {
                let f = u
                    .failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                line.push_str(&format!(": {f}"));
            }
            line
        })
        .collect()
}

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

fn pct1(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

fn secs(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into())
}

/// Text that stays inside one markdown table cell.
pub fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Write the markdown and JSON reports into `dir`, as `<date>-<category>.md` and `.json`.
/// A second run on the same day gets `-2`, then `-3`, so no run is overwritten. Returns the
/// markdown path.
pub fn write(dir: &Path, report: &Report) -> std::io::Result<PathBuf> {
    write_with(dir, report, None)
}

/// [`write`], with the markdown compared against `previous` (its file name and report).
pub fn write_with(
    dir: &Path,
    report: &Report,
    previous: Option<(&str, &Report)>,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let base = format!("{}-{}", report.date.format("%Y-%m-%d"), report.category);
    let mut n = 1;
    let (md, json) = loop {
        let stem = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let md = dir.join(format!("{stem}.md"));
        let json = dir.join(format!("{stem}.json"));
        if !md.exists() && !json.exists() {
            break (md, json);
        }
        n += 1;
    };
    let written = Written {
        report,
        summary: report.summary(),
    };
    let body = serde_json::to_string_pretty(&written).map_err(std::io::Error::other)?;
    std::fs::write(&json, body + "\n")?;
    std::fs::write(&md, report.to_markdown_with(previous))?;
    Ok(md)
}

/// Read one report from its `.json`, or from the `.md` next to it.
pub fn read(path: &Path) -> Result<Report, String> {
    let json = if path.extension().and_then(|x| x.to_str()) == Some("md") {
        path.with_extension("json")
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&json)
        .map_err(|e| format!("{}: can't read: {e}", json.display()))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("{}: not a pmos-evals report: {e}", json.display()))
}
