//! Has a category met its bar two weeks in a row (D10)? Read from the JSON reports.
//!
//! Runs are grouped by ISO week (Monday to Sunday). A week **meets the bar** only when every
//! run in it does, so a rerun after a model or harness change that falls below the bar counts
//! against that week. The category passes when the latest week with a run and the calendar
//! week right before it both meet the bar. A week with no run breaks the streak.
//!
//! Only reports that [`counts`] accepts take part (review finding N5): those whose gateway
//! reported the production role models (`benchmark.production_gateway` in `config.yaml`,
//! `deploy/gateway.yaml`), with no stub or testing-only upstream, and, for a persona's
//! category, on the persona's current version. [`evaluate`] itself judges whatever it's given.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use chrono::{Datelike, Duration, IsoWeek, NaiveDate};

use crate::report::{meets, Report};

/// One ISO week of runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Week {
    pub week: IsoWeek,
    /// (date, passed, total, met) per run, oldest first.
    pub runs: Vec<(NaiveDate, usize, usize, bool)>,
}

impl Week {
    pub fn met(&self) -> bool {
        !self.runs.is_empty() && self.runs.iter().all(|r| r.3)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub category: String,
    pub pass_bar: f64,
    /// Weeks with runs, oldest first.
    pub weeks: Vec<Week>,
    /// The latest week and the week before it both met the bar.
    pub two_weeks_met: bool,
    /// Consecutive weeks, ending at the latest, that met the bar.
    pub streak: usize,
}

/// Every report for `category` in `dir` (from the `.json` files), oldest first. A JSON file
/// that isn't a report is an error naming the file, so a broken report is never skipped
/// silently.
pub fn load_reports(dir: &Path, category: &str) -> Result<Vec<Report>, String> {
    Ok(load_named(dir, category)?
        .into_iter()
        .map(|(_, r)| r)
        .collect())
}

/// The category's latest report in `dir` (by date, then start time) with its file name.
pub fn latest_report(dir: &Path, category: &str) -> Result<Option<(String, Report)>, String> {
    Ok(load_named(dir, category)?.pop())
}

/// Every report in `dir`, of every category, each with its file name.
pub fn load_all(dir: &Path) -> Result<Vec<(String, Report)>, String> {
    load_where(dir, |_| true)
}

/// [`load_reports`], each with its file name.
pub fn load_named(dir: &Path, category: &str) -> Result<Vec<(String, Report)>, String> {
    load_where(dir, |r| r.category == category)
}

fn load_where(dir: &Path, keep: impl Fn(&Report) -> bool) -> Result<Vec<(String, Report)>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: can't read: {e}", dir.display())),
    };
    let mut reports = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: can't read: {e}", path.display()))?;
        let report: Report = serde_json::from_str(&text)
            .map_err(|e| format!("{}: not a pmos-evals report: {e}", path.display()))?;
        if keep(&report) {
            let name = entry.file_name().to_string_lossy().into_owned();
            reports.push((name, report));
        }
    }
    reports.sort_by_key(|(_, r)| (r.date, r.started_at));
    Ok(reports)
}

/// The production role models: role name to model id, from the `model` of each role in the
/// production gateway config that `config.yaml`'s `benchmark.production_gateway` names
/// (`deploy/gateway.yaml`). Only the model ids are read, never an upstream address.
pub fn production_models(evals_root: &Path) -> Result<BTreeMap<String, String>, String> {
    let config = crate::config::Config::load(evals_root)?;
    let path = evals_root.join(&config.benchmark.production_gateway);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: can't read the production gateway config: {e}",
            path.display()
        )
    })?;
    let v: serde_norway::Value =
        serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let roles = v
        .get("roles")
        .and_then(|r| r.as_mapping())
        .ok_or_else(|| format!("{}: no `roles`", path.display()))?;
    let mut out = BTreeMap::new();
    for (name, role) in roles {
        let (Some(name), Some(model)) = (name.as_str(), role.get("model").and_then(|m| m.as_str()))
        else {
            return Err(format!(
                "{}: a role without a name or `model`",
                path.display()
            ));
        };
        out.insert(name.to_string(), model.to_string());
    }
    if out.is_empty() {
        return Err(format!("{}: no roles", path.display()));
    }
    Ok(out)
}

/// The `version:` in `<content>/personas/<persona>/PERSONA.md`'s front matter.
pub fn persona_version(content: &Path, persona: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(content.join("personas").join(persona).join("PERSONA.md")).ok()?;
    let front = text.strip_prefix("---")?.split("\n---").next()?;
    let v: serde_norway::Value = serde_norway::from_str(front).ok()?;
    match v.get("version")? {
        serde_norway::Value::String(s) => Some(s.clone()),
        serde_norway::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether `report` counts towards its category's bar (review finding N5): its gateway's
/// models were recorded ([`Report::gateway_models`]), are exactly the production role models
/// (`production`), role for role, came from neither the stub nor a testing-only upstream; and,
/// for a persona's category, the persona's version then (`report.persona_version`) is its
/// version now (`persona_now`). `Err` says why not. A report without the gateway's models
/// (every one before 2026-11-22) never counts.
pub fn counts(
    report: &Report,
    production: &BTreeMap<String, String>,
    persona_now: Option<&str>,
) -> Result<(), String> {
    let Some(m) = &report.gateway_models else {
        return Err("the gateway's models weren't recorded".into());
    };
    if m.stub {
        return Err("the gateway answered with its stub model".into());
    }
    if m.cloud_test {
        return Err("a testing-only upstream was on".into());
    }
    if m.upstreams.values().any(|u| u != "local") {
        return Err("a role was served by an upstream that isn't local".into());
    }
    if &m.roles != production {
        let differ: Vec<String> = production
            .keys()
            .chain(m.roles.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|r| production.get(*r) != m.roles.get(*r))
            .map(|r| {
                format!(
                    "{r}: `{}`, production `{}`",
                    m.roles.get(r).map(String::as_str).unwrap_or("none"),
                    production.get(r).map(String::as_str).unwrap_or("none")
                )
            })
            .collect();
        return Err(format!(
            "its models aren't the production role models ({})",
            differ.join("; ")
        ));
    }
    if let Some(now) = persona_now {
        match report.persona_version.as_deref() {
            Some(v) if v == now => {}
            Some(v) => {
                return Err(format!(
                    "it measured persona version `{v}`; the persona is now `{now}`"
                ))
            }
            None => return Err("the persona's version wasn't recorded".into()),
        }
    }
    Ok(())
}

/// The reports that count ([`counts`]), and `(file, date, why not)` for the rest. With no
/// production models (`Err`), none counts.
pub fn split_counting(
    reports: Vec<(String, Report)>,
    production: &Result<BTreeMap<String, String>, String>,
    persona_now: Option<&str>,
) -> (Vec<Report>, Vec<(String, NaiveDate, String)>) {
    let mut kept = Vec::new();
    let mut left = Vec::new();
    for (file, r) in reports {
        let verdict = match production {
            Ok(p) => counts(&r, p, persona_now),
            Err(e) => Err(format!("no production role models: {e}")),
        };
        match verdict {
            Ok(()) => kept.push(r),
            Err(why) => left.push((file, r.date, why)),
        }
    }
    (kept, left)
}

/// The personas (marketplace bots) whose benchmark passes (D10), for `pmos lint`'s
/// `persona-benchmark` rule: those named by at least one category under `evals_root`
/// (`persona: <key>` in its `category.yaml`), where **every** category naming it has a pass bar
/// and has met it two weeks in a row in `<evals_root>/reports`, counting only the reports that
/// [`counts`] accepts: run on the production role models, and on the persona's current version
/// in `content`. Fails closed: a category that doesn't load, has no bar, or whose reports
/// can't be read, a persona whose version can't be read, and production models that can't be
/// read all count as not met (`pmos-evals check` reports why), and a missing evals directory
/// gives no persona.
pub fn benchmarked_personas(
    evals_root: &Path,
    content: &Path,
) -> std::collections::BTreeSet<String> {
    let reports = evals_root.join("reports");
    let production = production_models(evals_root);
    let mut met: BTreeMap<String, bool> = BTreeMap::new();
    let Ok(names) = crate::spec::category_names(evals_root) else {
        return Default::default();
    };
    for name in names {
        let Ok(category) = crate::spec::load_category(evals_root, &name) else {
            // A category that doesn't load still counts against the persona it names.
            let named = std::fs::read_to_string(
                crate::spec::category_dir(evals_root, &name).join("category.yaml"),
            )
            .ok()
            .and_then(|t| serde_norway::from_str::<serde_norway::Value>(&t).ok())
            .and_then(|v| v.get("persona").and_then(|p| p.as_str()).map(String::from));
            if let Some(persona) = named {
                met.insert(persona, false);
            }
            continue;
        };
        let Some(persona) = category.persona else {
            continue;
        };
        let version = persona_version(content, &persona);
        let ok = production.is_ok()
            && version.is_some()
            && category.pass_bar.is_some_and(|bar| {
                load_named(&reports, &name)
                    .map(|r| {
                        let (kept, _) = split_counting(r, &production, version.as_deref());
                        evaluate(&name, bar, &kept).two_weeks_met
                    })
                    .unwrap_or(false)
            });
        let entry = met.entry(persona).or_insert(true);
        *entry = *entry && ok;
    }
    met.into_iter()
        .filter(|(_, ok)| *ok)
        .map(|(k, _)| k)
        .collect()
}

/// Judge the reports against `pass_bar` (the category's current bar, so raising the bar
/// applies to earlier runs too).
pub fn evaluate(category: &str, pass_bar: f64, reports: &[Report]) -> History {
    let mut weeks: Vec<Week> = Vec::new();
    for r in reports {
        let week = r.date.iso_week();
        let run = (
            r.date,
            r.passed(),
            r.total(),
            meets(r.passed(), r.total(), pass_bar),
        );
        match weeks.iter_mut().find(|w| w.week == week) {
            Some(w) => w.runs.push(run),
            None => weeks.push(Week {
                week,
                runs: vec![run],
            }),
        }
    }
    weeks.sort_by_key(|w| (w.week.year(), w.week.week()));
    let mut streak = 0;
    for (i, w) in weeks.iter().enumerate().rev() {
        if !w.met() {
            break;
        }
        streak += 1;
        let consecutive = i
            .checked_sub(1)
            .map(|p| weeks[p].week == previous_week(w.week));
        if consecutive != Some(true) {
            break;
        }
    }
    History {
        category: category.to_string(),
        pass_bar,
        two_weeks_met: streak >= 2,
        streak,
        weeks,
    }
}

fn previous_week(w: IsoWeek) -> IsoWeek {
    let monday = NaiveDate::from_isoywd_opt(w.year(), w.week(), chrono::Weekday::Mon)
        .expect("an IsoWeek always has a Monday");
    (monday - Duration::days(7)).iso_week()
}

impl History {
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let verdict = if self.two_weeks_met {
            "met its bar two weeks in a row"
        } else {
            "has not met its bar two weeks in a row"
        };
        let _ = writeln!(
            s,
            "{}: {verdict} (bar {:.0}%, streak {} week{}).",
            self.category,
            self.pass_bar * 100.0,
            self.streak,
            if self.streak == 1 { "" } else { "s" }
        );
        if self.weeks.is_empty() {
            let _ = writeln!(s, "No reports yet.");
        }
        for w in &self.weeks {
            let runs: Vec<String> = w
                .runs
                .iter()
                .map(|(d, p, t, m)| format!("{d} {p}/{t}{}", if *m { "" } else { " (below)" }))
                .collect();
            let _ = writeln!(
                s,
                "  {}-W{:02}: {}  {}",
                w.week.year(),
                w.week.week(),
                if w.met() { "met " } else { "below" },
                runs.join(", ")
            );
        }
        s
    }
}
