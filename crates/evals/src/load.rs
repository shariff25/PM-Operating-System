//! The load test: N tasks at once, and their p50 and p90 queue and total times against the
//! latency targets in `evals/config.yaml`.

use std::fmt::Write as _;

use tokio::task::JoinSet;

use crate::client::{status, Runtime};
use crate::config::{LatencyTargets, Target};
use crate::outcome::Outcome;
use crate::run::{follow_task, Follow};

/// The nearest-rank percentile: the smallest value with at least `p`% of the values at or
/// below it. `None` for no values.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

#[derive(Debug, Clone)]
pub struct LoadResult {
    pub tasks: usize,
    pub outcomes: Vec<Outcome>,
}

/// One measured line: the value, its target, and whether it is within it.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    pub name: &'static str,
    pub value: Option<f64>,
    pub target: Target,
}

impl Measured {
    /// Within target. A value that couldn't be measured is not.
    pub fn met(&self) -> bool {
        self.value.is_some_and(|v| v <= self.target.seconds)
    }
}

impl LoadResult {
    pub fn queue_times(&self) -> Vec<f64> {
        self.outcomes.iter().filter_map(|o| o.queue_secs).collect()
    }

    /// Total times of the tasks that settled.
    pub fn total_times(&self) -> Vec<f64> {
        self.outcomes.iter().filter_map(|o| o.total_secs).collect()
    }

    /// Tasks that didn't finish: not created, timed out, failed or cancelled.
    pub fn unfinished(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| {
                o.runtime_error.is_some()
                    || o.timed_out
                    || o.status_is(status::FAILED)
                    || o.status_is(status::CANCELLED)
            })
            .count()
    }

    pub fn measure(&self, targets: &LatencyTargets) -> Vec<Measured> {
        let q = self.queue_times();
        let t = self.total_times();
        vec![
            Measured {
                name: "queue p50",
                value: percentile(&q, 50.0),
                target: targets.queue_p50.clone(),
            },
            Measured {
                name: "queue p90",
                value: percentile(&q, 90.0),
                target: targets.queue_p90.clone(),
            },
            Measured {
                name: "total p50",
                value: percentile(&t, 50.0),
                target: targets.total_p50.clone(),
            },
            Measured {
                name: "total p90",
                value: percentile(&t, 90.0),
                target: targets.total_p90.clone(),
            },
        ]
    }

    /// Every target met and every task finished.
    pub fn passed(&self, targets: &LatencyTargets) -> bool {
        self.unfinished() == 0 && self.measure(targets).iter().all(Measured::met)
    }

    pub fn to_markdown(&self, bot: &str, targets: &LatencyTargets) -> String {
        let mut s = String::new();
        let verdict = if self.passed(targets) {
            "**Within the targets.**"
        } else {
            "**Missed a target.**"
        };
        let _ = writeln!(
            s,
            "# Load test: {} concurrent tasks for `{bot}`\n",
            self.tasks
        );
        let _ = writeln!(
            s,
            "{verdict} {} of {} tasks finished.\n",
            self.tasks - self.unfinished(),
            self.tasks
        );
        let _ = writeln!(
            s,
            "| Measure | Seconds | Target | Met | Target source |\n| --- | --- | --- | --- | --- |"
        );
        for m in self.measure(targets) {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} |",
                m.name,
                m.value
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "-".into()),
                m.target.seconds,
                if m.met() { "yes" } else { "**no**" },
                m.target.source.replace('|', "\\|")
            );
        }
        let problems: Vec<String> = self
            .outcomes
            .iter()
            .filter_map(|o| {
                let id = o.task_id.as_deref().unwrap_or("(not created)");
                if let Some(e) = &o.runtime_error {
                    Some(format!("- `{id}`: {e}"))
                } else if o.timed_out {
                    Some(format!(
                        "- `{id}`: timed out while `{}`",
                        o.status.as_deref().unwrap_or("?")
                    ))
                } else if o.status_is(status::FAILED) || o.status_is(status::CANCELLED) {
                    Some(format!(
                        "- `{id}`: {}: {}",
                        o.status.as_deref().unwrap_or("?"),
                        o.error.as_deref().unwrap_or("no reason given")
                    ))
                } else {
                    None
                }
            })
            .collect();
        if !problems.is_empty() {
            let _ = writeln!(s, "\n## Unfinished tasks\n\n{}", problems.join("\n"));
        }
        s
    }
}

/// Send `n` tasks at once, the asks taken in turn from `asks`, and follow them all.
pub async fn run_load(
    rt: &Runtime,
    bot: &str,
    requested_by: &str,
    asks: &[String],
    n: usize,
    follow: &Follow,
) -> LoadResult {
    let mut set = JoinSet::new();
    for i in 0..n {
        let rt = rt.clone();
        let bot = bot.to_string();
        let who = requested_by.to_string();
        let ask = asks[i % asks.len()].clone();
        let follow = follow.clone();
        set.spawn(async move { follow_task(&rt, &bot, &who, &ask, &follow).await });
    }
    let mut outcomes = Vec::with_capacity(n);
    while let Some(o) = set.join_next().await {
        outcomes.push(o.expect("a task follower never panics"));
    }
    LoadResult { tasks: n, outcomes }
}
