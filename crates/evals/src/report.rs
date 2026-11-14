//! Reports: `evals/reports/<date>-<category>.md` for people and a `.json` next to it for
//! `history`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::grade::Check;
use crate::tag::{Tag, Tagged};

/// One run of one category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub category: String,
    pub bot: String,
    pub date: NaiveDate,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub pass_bar: f64,
    pub runtime_url: String,
    pub requested_by: String,
    /// `PMOS_GATEWAY_WORKAROUNDS` in the runner's environment, as the person running it
    /// declared it. The runner can't see the gateway's own setting.
    #[serde(default)]
    pub gateway_workarounds: Option<String>,
    /// Free text saying why this run happened ("weekly", "model swap to …").
    #[serde(default)]
    pub note: Option<String>,
    pub tasks: Vec<TaskReport>,
}

/// One task's line in a report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskReport {
    pub id: String,
    pub ask: String,
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
}

/// Whether `passed` of `total` meets `bar`.
pub fn meets(passed: usize, total: usize, bar: f64) -> bool {
    total > 0 && passed as f64 + 1e-9 >= bar * total as f64
}

impl Report {
    pub fn passed(&self) -> usize {
        self.tasks.iter().filter(|t| t.passed).count()
    }

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

    pub fn met_bar(&self) -> bool {
        meets(self.passed(), self.total(), self.pass_bar)
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

    /// The report as markdown.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let passed = self.passed();
        let total = self.total();
        let verdict = if self.met_bar() {
            "**Met the bar.**"
        } else {
            "**Below the bar.**"
        };
        let _ = writeln!(s, "# Benchmark: {} ({})\n", self.category, self.date);
        let _ = writeln!(
            s,
            "{verdict} {passed} of {total} tasks passed ({}), against a bar of {}.\n",
            pct(self.pass_rate()),
            pct(self.pass_bar)
        );
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
            "| Ran | {} to {} UTC |",
            self.started_at.format("%Y-%m-%d %H:%M:%S"),
            self.finished_at.format("%H:%M:%S")
        );
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
                let ids: Vec<String> = self
                    .tasks
                    .iter()
                    .filter(|t| t.tag.as_ref().map(|x| x.tag) == Some(*tag))
                    .map(|t| format!("`{}`", t.id))
                    .collect();
                let _ = writeln!(s, "| {tag} | {n} | {} |", ids.join(", "));
            }
        }

        let _ = writeln!(s, "\n## Tasks\n");
        let _ = writeln!(
            s,
            "| Task | Result | Tag | Status | Queue (s) | Total (s) | Why |\n| --- | --- | --- | --- | --- | --- | --- |"
        );
        for t in &self.tasks {
            let result = if t.passed { "pass" } else { "**FAIL**" };
            let tag = t
                .tag
                .as_ref()
                .map(|x| format!("{} (`{}`)", x.tag, x.rule))
                .unwrap_or_default();
            let why = if t.passed {
                String::new()
            } else {
                t.failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            };
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

        let failures: Vec<&TaskReport> = self.tasks.iter().filter(|t| !t.passed).collect();
        if !failures.is_empty() {
            let _ = writeln!(s, "\n## Failure details\n");
            for t in failures {
                let _ = writeln!(s, "### `{}`\n", t.id);
                let _ = writeln!(s, "- **Asked:** {}", t.ask);
                if let Some(id) = &t.task_id {
                    let _ = writeln!(s, "- **Task:** `{id}` (attempts: {})", t.attempts);
                }
                if let Some(tagged) = &t.tag {
                    let _ = writeln!(
                        s,
                        "- **Tag:** {} by rule `{}`: {}",
                        tagged.tag,
                        tagged.rule,
                        cell(&tagged.reason)
                    );
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

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

fn secs(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into())
}

/// Text that stays inside one markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Write the markdown and JSON reports into `dir`, as `<date>-<category>.md` and `.json`.
/// A second run on the same day gets `-2`, then `-3`, so no run is overwritten. Returns the
/// markdown path.
pub fn write(dir: &Path, report: &Report) -> std::io::Result<PathBuf> {
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
    let body = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(&json, body + "\n")?;
    std::fs::write(&md, report.to_markdown())?;
    Ok(md)
}
