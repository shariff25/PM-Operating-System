//! The eval gate: `pmos-evals gate`, the check a change to a bot's role, skills or model must
//! pass. It compares a candidate run with a baseline (by default the category's latest report
//! on `main`) and fails when any of these hold:
//!
//! 1. a `regression: true` task flips to failing (passed every repeat, now fails every one);
//! 2. the 95% interval for the mean paired difference lies wholly below 0;
//! 3. a deterministic check that passed in the baseline now fails, on any task: the check
//!    failed in none of the task's baseline attempts (each graded in full, so not cut short by
//!    a runtime error, a timeout or no answer) and fails in every candidate attempt;
//! 4. a blocking judge fails a task it passed in the baseline: "yes" in every baseline attempt
//!    and "no" in every candidate attempt.
//!
//! With no baseline report there is nothing to compare, and the gate passes.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use crate::grade::Check;
use crate::report::{Report, TaskReport};
use crate::stats::{compare, pass_hat_k, pts, report_se, summaries, Comparison};

/// Why the gate failed.
#[derive(Debug, Clone, PartialEq)]
pub enum FailReason {
    RegressionFlipped { task: String },
    Worse { mean: f64, lo: f64, hi: f64 },
    CheckFlipped { task: String, check: String },
    JudgeFlipped { task: String, judge: String },
}

impl std::fmt::Display for FailReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailReason::RegressionFlipped { task } => write!(
                f,
                "regression task `{task}` flipped to failing (passed every repeat in the baseline, fails every one now)"
            ),
            FailReason::Worse { mean, lo, hi } => write!(
                f,
                "the pass rate fell beyond noise: mean paired difference {}, 95% interval {} to {}, wholly below 0",
                pts(*mean),
                pts(*lo),
                pts(*hi)
            ),
            FailReason::CheckFlipped { task, check } => write!(
                f,
                "`{task}`: check `{check}` passed in the baseline and now fails"
            ),
            FailReason::JudgeFlipped { task, judge } => write!(
                f,
                "`{task}`: blocking judge `{judge}` passed it in the baseline and now fails it"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GateResult {
    pub category: String,
    /// The baseline's file name and where it came from, when there was one.
    pub baseline: Option<(String, Report)>,
    pub baseline_source: String,
    pub candidate: Report,
    pub candidate_name: String,
    pub comparison: Option<Comparison>,
    pub reasons: Vec<FailReason>,
}

impl GateResult {
    pub fn passed(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// Whether an attempt was graded in full: every check ran.
fn graded_in_full(t: &TaskReport) -> bool {
    !t.failures.iter().any(|c| {
        matches!(
            c,
            Check::Runtime { .. } | Check::TimedOut { .. } | Check::NoAnswer
        )
    })
}

fn failed_keys(t: &TaskReport) -> BTreeSet<String> {
    t.failures.iter().filter_map(Check::key).collect()
}

/// Deterministic checks that passed in every baseline attempt and fail in every candidate one.
fn check_flips(baseline: &[&TaskReport], candidate: &[&TaskReport]) -> Vec<String> {
    if baseline.is_empty() || candidate.is_empty() || !baseline.iter().all(|t| graded_in_full(t)) {
        return Vec::new();
    }
    let failed_before: HashSet<String> = baseline.iter().flat_map(|t| failed_keys(t)).collect();
    let mut now = candidate.iter().map(|t| failed_keys(t));
    let first = now.next().unwrap_or_default();
    let everywhere: BTreeSet<String> =
        now.fold(first, |acc, k| acc.intersection(&k).cloned().collect());
    everywhere
        .into_iter()
        .filter(|k| !failed_before.contains(k))
        .collect()
}

/// A judge's verdicts on a task's attempts: `Some(v)` when every attempt has the verdict `v`.
fn unanimous(lines: &[&TaskReport], judge: &str) -> Option<bool> {
    let verdicts: Vec<Option<bool>> = lines
        .iter()
        .filter_map(|t| t.judges.iter().find(|v| v.name == judge))
        .map(|v| v.verdict)
        .collect();
    if verdicts.is_empty() || verdicts.len() != lines.len() {
        return None;
    }
    let first = verdicts[0]?;
    verdicts.iter().all(|v| *v == Some(first)).then_some(first)
}

/// Decide the gate for a candidate run against an optional baseline.
pub fn decide(
    baseline: Option<(String, Report)>,
    baseline_source: &str,
    candidate: Report,
    candidate_name: &str,
) -> GateResult {
    let mut reasons = Vec::new();
    let mut comparison = None;
    if let Some((_, base)) = &baseline {
        let c = compare(base, &candidate);
        for p in c.broke() {
            if p.candidate.regression {
                reasons.push(FailReason::RegressionFlipped { task: p.id.clone() });
            }
        }
        if c.worse() {
            let (lo, hi) = c.interval().expect("worse() implies an interval");
            reasons.push(FailReason::Worse {
                mean: c.mean.unwrap_or_default(),
                lo,
                hi,
            });
        }
        let blocking: Vec<&str> = candidate
            .judges
            .iter()
            .filter(|j| j.blocking)
            .map(|j| j.name.as_str())
            .collect();
        for p in &c.paired {
            let b: Vec<&TaskReport> = base.tasks.iter().filter(|t| t.id == p.id).collect();
            let n: Vec<&TaskReport> = candidate.tasks.iter().filter(|t| t.id == p.id).collect();
            for check in check_flips(&b, &n) {
                reasons.push(FailReason::CheckFlipped {
                    task: p.id.clone(),
                    check,
                });
            }
            for judge in &blocking {
                if unanimous(&b, judge) == Some(true) && unanimous(&n, judge) == Some(false) {
                    reasons.push(FailReason::JudgeFlipped {
                        task: p.id.clone(),
                        judge: (*judge).to_string(),
                    });
                }
            }
        }
        comparison = Some(c);
    }
    GateResult {
        category: candidate.category.clone(),
        baseline,
        baseline_source: baseline_source.to_string(),
        candidate,
        candidate_name: candidate_name.to_string(),
        comparison,
        reasons,
    }
}

fn numbers(r: &Report) -> (String, String) {
    let rate = match report_se(r) {
        Some(se) => format!("{:.1}% ± {:.1}%", r.pass_rate() * 100.0, se.se * 100.0),
        None => "-".into(),
    };
    let k = pass_hat_k(&summaries(r))
        .map(|k| format!("{:.0}% (k = {})", k * 100.0, r.repeats))
        .unwrap_or_else(|| "-".into());
    (rate, k)
}

impl GateResult {
    /// The gate's summary as markdown, for a PR comment or the job summary.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let verdict = if self.passed() {
            "passed"
        } else {
            "**FAILED**"
        };
        let _ = writeln!(s, "## Eval gate: `{}` {verdict}\n", self.category);
        let c = &self.candidate;
        let _ = writeln!(
            s,
            "Candidate: `{}` ({}, bot `{}`, {} attempts).",
            self.candidate_name,
            c.date,
            c.bot,
            c.total()
        );
        let Some((name, base)) = &self.baseline else {
            let _ = writeln!(
                s,
                "\nNo baseline: `{}` has no report on {}. Nothing to compare, so the gate passes; merge a report to set one.\n",
                self.category, self.baseline_source
            );
            let (rate, k) = numbers(c);
            let _ = writeln!(s, "- Pass rate: {rate}\n- pass^k: {k}");
            return s;
        };
        let _ = writeln!(
            s,
            "Baseline: `{name}` from {} ({}, {} attempts).\n",
            self.baseline_source,
            base.date,
            base.total()
        );
        let (br, bk) = numbers(base);
        let (cr, ck) = numbers(c);
        let _ = writeln!(s, "| | Baseline | Candidate |\n| --- | --- | --- |");
        let _ = writeln!(s, "| Pass rate (± standard error) | {br} | {cr} |");
        let _ = writeln!(s, "| pass^k | {bk} | {ck} |");
        if let Some(bar) = c.pass_bar {
            let met = |r: &Report| match r.met_bar() {
                Some(true) => "met",
                _ => "below",
            };
            let _ = writeln!(
                s,
                "| Bar {:.0}% (not a gate condition) | {} | {} |",
                bar * 100.0,
                met(base),
                met(c)
            );
        }
        if self.reasons.is_empty() {
            let _ = writeln!(s, "\nNo gate condition failed.");
        } else {
            let _ = writeln!(s, "\n**Fails because:**\n");
            for r in &self.reasons {
                let _ = writeln!(s, "- {r}");
            }
        }
        if let Some(cmp) = &self.comparison {
            let _ = writeln!(s, "\n### Compared with the baseline\n");
            s.push_str(&cmp.to_markdown());
        }
        if !c.judges.is_empty() || c.judges_skipped.is_some() {
            let _ = writeln!(s, "\n### Judges\n");
            if let Some(why) = &c.judges_skipped {
                let _ = writeln!(s, "- Not asked: {why}.");
            }
            for j in &c.judges {
                let no = c
                    .tasks
                    .iter()
                    .flat_map(|t| &t.judges)
                    .filter(|v| v.name == j.name && v.verdict == Some(false))
                    .count();
                let _ = writeln!(s, "- `{}`: {}; said no {no} time(s).", j.name, j.reason);
            }
        }
        s
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("can't run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The category's latest report in `reports_dir` as it is at the git ref `git_ref`, with its
/// file name. `None` when that ref has no report for the category.
pub fn baseline_from_git(
    reports_dir: &Path,
    git_ref: &str,
    category: &str,
) -> Result<Option<(String, Report)>, String> {
    if !reports_dir.is_dir() {
        return Err(format!(
            "{}: not a directory; pass --baseline <report> instead",
            reports_dir.display()
        ));
    }
    let top = git(reports_dir, &["rev-parse", "--show-toplevel"])?;
    let top = Path::new(top.trim());
    let prefix = git(reports_dir, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim();
    git(top, &["rev-parse", "--verify", "--quiet", &format!("{git_ref}^{{commit}}")]).map_err(
        |_| {
            format!(
                "git ref `{git_ref}` not found; fetch it (git fetch origin main) or pass --baseline <report>"
            )
        },
    )?;
    let mut args = vec!["ls-tree", "--name-only", git_ref];
    if !prefix.is_empty() {
        args.push(prefix);
    }
    let listing = git(top, &args)?;
    let mut reports = Vec::new();
    for path in listing.lines().filter(|p| p.ends_with(".json")) {
        let name = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        if !name.contains(category) {
            continue;
        }
        let text = git(top, &["show", &format!("{git_ref}:{path}")])?;
        let report: Report = serde_json::from_str(&text)
            .map_err(|e| format!("{git_ref}:{path}: not a pmos-evals report: {e}"))?;
        if report.category == category {
            reports.push((name.to_string(), report));
        }
    }
    reports.sort_by_key(|(_, r)| (r.date, r.started_at));
    Ok(reports.pop())
}
