//! Repeats, pass^k, the clustered standard error, and comparing two runs.
//!
//! **Pass rate** is over every attempt: passes ÷ (tasks × repeats). **pass^k** is the share of
//! tasks that passed every one of their repeats.
//!
//! **Clustered standard error.** Attempts of one task, and tasks with one source, are not
//! independent, so the standard error of the pass rate is cluster-robust. With `n` attempts
//! `y` (1 pass, 0 fail), mean `ȳ`, and `G` clusters where `S_g` is the sum of `y − ȳ` over the
//! attempts in cluster `g`:
//!
//! ```text
//! SE = sqrt( G / (G − 1) · Σ_g S_g² ) / n
//! ```
//!
//! A task's cluster is its `cluster`, else its first `must_cite` URL, else its id, so every
//! attempt of a task is in one cluster. When every cluster holds a single attempt (one run of
//! tasks that share nothing), or there is only one cluster, this is the plain binomial
//! `sqrt(ȳ(1 − ȳ)/n)` instead.
//!
//! **Paired difference.** For each task in both runs, `d = candidate pass rate − baseline pass
//! rate` (each over its own repeats). The mean `d̄` has the paired standard error `s_d / √m`
//! (`s_d` the sample standard deviation of the `m` differences), and a 95% interval of
//! `d̄ ± 1.96 · SE`. This is a normal approximation: with few tasks (under about 30) the true
//! interval is wider, so read a borderline interval with care. It needs at least two tasks.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::report::{Report, TaskReport};

/// z for a two-sided 95% interval.
pub const Z95: f64 = 1.96;

/// One task's attempts in one run.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSummary {
    pub id: String,
    pub cluster: String,
    pub passes: u32,
    pub runs: u32,
    pub regression: bool,
}

impl TaskSummary {
    pub fn rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.passes) / f64::from(self.runs)
        }
    }

    pub fn all_passed(&self) -> bool {
        self.runs > 0 && self.passes == self.runs
    }

    pub fn all_failed(&self) -> bool {
        self.runs > 0 && self.passes == 0
    }
}

/// The cluster a report line is in: its recorded cluster, else its task id (reports written
/// before clusters have none).
pub fn line_cluster(t: &TaskReport) -> String {
    t.cluster.clone().unwrap_or_else(|| t.id.clone())
}

/// Each task's passes out of its attempts, in the report's order.
pub fn summaries(report: &Report) -> Vec<TaskSummary> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, TaskSummary> = HashMap::new();
    for t in &report.tasks {
        let s = by_id.entry(t.id.clone()).or_insert_with(|| {
            order.push(t.id.clone());
            TaskSummary {
                id: t.id.clone(),
                cluster: line_cluster(t),
                passes: 0,
                runs: 0,
                regression: t.regression,
            }
        });
        s.runs += 1;
        s.passes += u32::from(t.passed);
        s.regression |= t.regression;
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// The share of tasks that passed every attempt; `None` with no tasks.
pub fn pass_hat_k(tasks: &[TaskSummary]) -> Option<f64> {
    (!tasks.is_empty())
        .then(|| tasks.iter().filter(|t| t.all_passed()).count() as f64 / tasks.len() as f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeMethod {
    Clustered,
    Binomial,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StdErr {
    pub se: f64,
    pub clusters: usize,
    pub method: SeMethod,
}

/// The standard error of the mean of `(cluster, passed)` observations (see the module docs).
/// `None` with no observations.
pub fn clustered_se<'a>(obs: impl IntoIterator<Item = (&'a str, bool)>) -> Option<StdErr> {
    let obs: Vec<(&str, f64)> = obs
        .into_iter()
        .map(|(c, p)| (c, if p { 1.0 } else { 0.0 }))
        .collect();
    let n = obs.len();
    if n == 0 {
        return None;
    }
    let mean = obs.iter().map(|o| o.1).sum::<f64>() / n as f64;
    let mut sums: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    for (c, y) in &obs {
        let e = sums.entry(c).or_default();
        e.0 += y - mean;
        e.1 += 1;
    }
    let g = sums.len();
    let singletons = sums.values().all(|(_, k)| *k == 1);
    if singletons || g < 2 {
        return Some(StdErr {
            se: (mean * (1.0 - mean) / n as f64).sqrt(),
            clusters: g,
            method: SeMethod::Binomial,
        });
    }
    let ss: f64 = sums.values().map(|(s, _)| s * s).sum();
    let gf = g as f64;
    Some(StdErr {
        se: (gf / (gf - 1.0) * ss).sqrt() / n as f64,
        clusters: g,
        method: SeMethod::Clustered,
    })
}

/// A report's standard error of its pass rate, clustered by each task's cluster.
pub fn report_se(report: &Report) -> Option<StdErr> {
    let clusters: Vec<String> = report.tasks.iter().map(line_cluster).collect();
    clustered_se(
        clusters
            .iter()
            .zip(&report.tasks)
            .map(|(c, t)| (c.as_str(), t.passed)),
    )
}

/// One task in both runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub id: String,
    pub baseline: TaskSummary,
    pub candidate: TaskSummary,
}

impl Paired {
    pub fn diff(&self) -> f64 {
        self.candidate.rate() - self.baseline.rate()
    }

    /// Passed every repeat before and fails every one now.
    pub fn broke(&self) -> bool {
        self.baseline.all_passed() && self.candidate.all_failed()
    }

    /// Failed every repeat before and passes every one now.
    pub fn fixed(&self) -> bool {
        self.baseline.all_failed() && self.candidate.all_passed()
    }
}

/// Two runs of one category, task by task.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub paired: Vec<Paired>,
    /// Tasks only in the candidate, or only in the baseline.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// The mean paired difference in pass rate, `None` with no tasks in common.
    pub mean: Option<f64>,
    /// The paired standard error, `None` with fewer than two tasks in common.
    pub se: Option<f64>,
}

impl Comparison {
    /// The 95% interval for the mean paired difference.
    pub fn interval(&self) -> Option<(f64, f64)> {
        let (m, se) = (self.mean?, self.se?);
        Some((m - Z95 * se, m + Z95 * se))
    }

    /// The interval lies wholly below 0: the candidate is worse beyond noise.
    pub fn worse(&self) -> bool {
        self.interval().is_some_and(|(_, hi)| hi < -1e-12)
    }

    pub fn broke(&self) -> impl Iterator<Item = &Paired> {
        self.paired.iter().filter(|p| p.broke())
    }

    pub fn fixed(&self) -> impl Iterator<Item = &Paired> {
        self.paired.iter().filter(|p| p.fixed())
    }

    /// The comparison as markdown, without a heading.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        match (self.mean, self.interval()) {
            (Some(m), Some((lo, hi))) => {
                let _ = writeln!(
                    s,
                    "Mean paired difference in pass rate: **{}** (95% interval {} to {}, {} tasks in both runs).",
                    pts(m),
                    pts(lo),
                    pts(hi),
                    self.paired.len()
                );
            }
            (Some(m), None) => {
                let _ = writeln!(
                    s,
                    "Mean paired difference in pass rate: **{}** (one task in both runs, so no interval).",
                    pts(m)
                );
            }
            _ => {
                let _ = writeln!(s, "No task is in both runs.");
            }
        }
        let broke: Vec<String> = self.broke().map(|p| format!("`{}`", p.id)).collect();
        let fixed: Vec<String> = self.fixed().map(|p| format!("`{}`", p.id)).collect();
        let _ = writeln!(
            s,
            "\n- **Flipped to failing:** {}",
            if broke.is_empty() {
                "none".to_string()
            } else {
                broke.join(", ")
            }
        );
        let _ = writeln!(
            s,
            "- **Flipped to passing:** {}",
            if fixed.is_empty() {
                "none".to_string()
            } else {
                fixed.join(", ")
            }
        );
        if !self.added.is_empty() {
            let _ = writeln!(s, "- **New tasks:** {}", ticks(&self.added));
        }
        if !self.removed.is_empty() {
            let _ = writeln!(s, "- **Tasks no longer run:** {}", ticks(&self.removed));
        }
        let changed: Vec<&Paired> = self
            .paired
            .iter()
            .filter(|p| p.diff().abs() > 1e-12)
            .collect();
        if !changed.is_empty() {
            let _ = writeln!(
                s,
                "\n| Task | Before | Now | Difference |\n| --- | --- | --- | --- |"
            );
            for p in changed {
                let _ = writeln!(
                    s,
                    "| `{}` | {}/{} | {}/{} | {} |",
                    p.id,
                    p.baseline.passes,
                    p.baseline.runs,
                    p.candidate.passes,
                    p.candidate.runs,
                    pts(p.diff())
                );
            }
        }
        s
    }
}

fn ticks(ids: &[String]) -> String {
    ids.iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A difference in pass rate as signed percentage points.
pub fn pts(x: f64) -> String {
    let v = x * 100.0;
    if v.abs() < 0.05 {
        "0.0 pts".into()
    } else {
        format!("{v:+.1} pts")
    }
}

/// Compare a candidate run with a baseline, task by task.
pub fn compare(baseline: &Report, candidate: &Report) -> Comparison {
    let base = summaries(baseline);
    let cand = summaries(candidate);
    let base_by_id: HashMap<&str, &TaskSummary> = base.iter().map(|t| (t.id.as_str(), t)).collect();
    let cand_ids: Vec<&str> = cand.iter().map(|t| t.id.as_str()).collect();
    let mut paired = Vec::new();
    let mut added = Vec::new();
    for c in &cand {
        match base_by_id.get(c.id.as_str()) {
            Some(b) => paired.push(Paired {
                id: c.id.clone(),
                baseline: (*b).clone(),
                candidate: c.clone(),
            }),
            None => added.push(c.id.clone()),
        }
    }
    let removed = base
        .iter()
        .filter(|b| !cand_ids.contains(&b.id.as_str()))
        .map(|b| b.id.clone())
        .collect();
    let d: Vec<f64> = paired.iter().map(Paired::diff).collect();
    let m = d.len();
    let mean = (m > 0).then(|| d.iter().sum::<f64>() / m as f64);
    let se = mean.filter(|_| m >= 2).map(|mean| {
        let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (m as f64 - 1.0);
        (var / m as f64).sqrt()
    });
    Comparison {
        paired,
        added,
        removed,
        mean,
        se,
    }
}
