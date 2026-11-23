use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use clap::{Args, Parser, Subcommand};
use pmos_evals::client::Runtime;
use pmos_evals::config::Config;
use pmos_evals::gate::{baseline_from_git, decide};
use pmos_evals::history::{
    evaluate, latest_report, load_all, load_named, persona_hash, persona_version,
    production_models, split_counting,
};
use pmos_evals::isolate::Isolation;
use pmos_evals::judge::{self, load_calibration, load_judges, load_labels, Gateway};
use pmos_evals::load::run_load;
use pmos_evals::report::Report;
use pmos_evals::run::{run_category, Follow, JudgeRun, RunOptions};
use pmos_evals::spec::{category_names, check_draft_tools, load_category, resolve_bot};
use pmos_evals::stats::compare;
use pmos_evals::taxonomy::Taxonomy;

/// PM OS benchmarks: run a category through the bot runtime, grade it, and keep the history.
///
/// Exit status: 0 when the bar (or target, or gate) is met, 1 when it isn't, 2 on an error.
#[derive(Parser)]
#[command(name = "pmos-evals", version)]
struct Cli {
    /// The evals directory: categories, config.yaml, taxonomy.yaml, judges and reports.
    #[arg(long, global = true, env = "PMOS_EVALS_ROOT", default_value = "evals")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the categories and tasks, config.yaml, the taxonomy, the judges and every tag
    /// used (every category when none is named).
    Check {
        #[arg(long)]
        category: Option<String>,
        /// The content root, for checking `drafts` tools against its tools.yaml.
        #[arg(long, default_value = "content")]
        content: PathBuf,
    },
    /// Run a category against the bot runtime and write its report.
    Run(RunArgs),
    /// Run a category (or take a run's report) and compare it with a baseline: the check a
    /// change must pass. Prints a markdown summary.
    Gate(GateArgs),
    /// Compare two reports of one category, task by task.
    Compare {
        /// The earlier report (.json, or the .md next to it).
        baseline: PathBuf,
        /// The later report.
        candidate: PathBuf,
    },
    /// Say whether a category met its bar two weeks in a row, from the reports that count
    /// (run on the production role models; for a persona, on its current version).
    History {
        #[arg(long)]
        category: String,
        /// Where the reports are (default: <root>/reports).
        #[arg(long)]
        reports_dir: Option<PathBuf>,
        /// The content root, for a persona's current version.
        #[arg(long, default_value = "content")]
        content: PathBuf,
    },
    /// Send N tasks at once and report p50 and p90 queue and total time against the targets.
    Load(LoadArgs),
    /// LLM judges.
    Judge {
        #[command(subcommand)]
        command: JudgeCommand,
    },
    /// List the categories a change affects, one per line, from the changed paths (one per
    /// line, relative to the repository root) on stdin or in --changed.
    Affected {
        /// A file of changed paths; `-` or none reads stdin.
        #[arg(long)]
        changed: Option<PathBuf>,
        /// The content root.
        #[arg(long, default_value = "content")]
        content: PathBuf,
        /// The content root's path in the repository, as changed paths name it.
        #[arg(long, default_value = "content")]
        content_prefix: String,
        /// Who the tasks are for, for `bot: personal` (default: runner.requested_by).
        #[arg(long = "as", env = "PMOS_EVALS_AS")]
        requested_by: Option<String>,
    },
}

#[derive(Subcommand)]
enum JudgeCommand {
    /// Run a judge over its labels and write <name>.calibration.json.
    Calibrate {
        name: String,
        #[command(flatten)]
        gateway: GatewayArgs,
        /// The calibration date (default: today, UTC).
        #[arg(long)]
        date: Option<NaiveDate>,
    },
    /// Add a labelling pack to each judge's labels file: up to --per-judge lines picked from a
    /// run's --label-candidates, mixing what the judge said, each with `label` empty for a
    /// person to fill in. Every judge when none is named.
    Pack {
        name: Option<String>,
        /// The --label-candidates directory of one or more runs.
        #[arg(long)]
        from: PathBuf,
        #[arg(long, default_value_t = 20)]
        per_judge: usize,
    },
}

#[derive(Args)]
struct GatewayArgs {
    /// The gateway's OpenAI-compatible URL; `/v1` is added if missing.
    #[arg(long, env = "PMOS_GATEWAY_URL")]
    gateway_url: Option<String>,
    /// The gateway token for client `evals` (in the gateway's PMOS_GATEWAY_TOKENS).
    #[arg(long, env = "PMOS_EVALS_GATEWAY_TOKEN", hide_env_values = true)]
    gateway_token: Option<String>,
}

impl GatewayArgs {
    fn gateway(&self) -> Option<Gateway> {
        match (self.gateway_url.as_deref(), self.gateway_token.as_deref()) {
            (Some(u), Some(t)) if !u.is_empty() && !t.is_empty() => Some(Gateway::new(u, t)),
            _ => None,
        }
    }
}

#[derive(Args)]
struct Conn {
    /// The bot runtime (default: runner.bots_url in config.yaml).
    #[arg(long, env = "PMOS_BOTS_URL")]
    bots_url: Option<String>,
    /// The runtime token for client `evals`.
    #[arg(long, env = "PMOS_EVALS_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// The handle tasks are requested by (default: runner.requested_by in config.yaml).
    #[arg(long = "as", env = "PMOS_EVALS_AS")]
    requested_by: Option<String>,
    /// Seconds one task may take (default: runner.task_timeout_seconds).
    #[arg(long)]
    timeout_secs: Option<u64>,
    /// Milliseconds between polls (default: runner.poll_ms).
    #[arg(long)]
    poll_ms: Option<u64>,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long)]
    category: String,
    #[command(flatten)]
    conn: Conn,
    /// A `console` runtime token, so failures can be tagged from the trace (which client
    /// `evals` may not read).
    #[arg(long, env = "PMOS_EVALS_TRACE_TOKEN", hide_env_values = true)]
    trace_token: Option<String>,
    /// Tasks in flight at once (default: runner.concurrency).
    #[arg(long)]
    concurrency: Option<usize>,
    /// Runs per task (default: the category's `repeats`, else 1).
    #[arg(long)]
    repeats: Option<u32>,
    /// Why this run happened, recorded in the report ("weekly", "model swap to …"). Free
    /// text that no rule reads: the models are recorded from the gateway itself, so the note
    /// needn't (and shouldn't) name a provider's host.
    #[arg(long)]
    note: Option<String>,
    /// The content root the runtime runs on, for the persona's version (persona categories).
    #[arg(long, env = "PMOS_EVALS_CONTENT", default_value = "content")]
    content: PathBuf,
    /// Run only this task (repeatable).
    #[arg(long = "task")]
    tasks: Vec<String>,
    /// The report date (default: today, UTC).
    #[arg(long)]
    date: Option<NaiveDate>,
    /// Where reports go (default: <root>/reports).
    #[arg(long)]
    reports_dir: Option<PathBuf>,
    /// Don't ask the judges.
    #[arg(long)]
    no_judges: bool,
    #[command(flatten)]
    gateway: GatewayArgs,
    /// Isolated attempts: the runtime URL of each lane, comma-separated. Each attempt takes a
    /// free lane, gets a fresh tool server there (--fresh-cmd), and runs on that lane's
    /// runtime; as many run at once as there are lanes. Needs --fresh-cmd.
    #[arg(long, env = "PMOS_EVALS_LANES", value_delimiter = ',')]
    lanes: Vec<String>,
    /// With --lanes: the program run as `<program> <lane>` (lanes from 1) before each attempt;
    /// it exits 0 once that lane has a fresh tool server.
    #[arg(long, env = "PMOS_EVALS_FRESH_CMD")]
    fresh_cmd: Option<PathBuf>,
    /// Keep a labelling-pack candidate for every attempt a judge answered (what it saw, what
    /// it said, the label empty) in <dir>/<date>-<category>-<judge>.jsonl. They hold answer
    /// text: keep them out of evals/reports. `pmos-evals judge pack` picks from them.
    #[arg(long)]
    label_candidates: Option<PathBuf>,
}

/// `--lanes` and `--fresh-cmd`, both or neither.
fn isolation(args: &RunArgs, token: &str, trace_token: Option<&str>) -> Result<Option<Isolation>> {
    let urls: Vec<String> = args
        .lanes
        .iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    let fresh = args
        .fresh_cmd
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty());
    match (urls.is_empty(), fresh) {
        (true, None) => Ok(None),
        (true, Some(_)) => {
            bail!("--fresh-cmd needs --lanes (PMOS_EVALS_LANES): the runtime of each lane")
        }
        (false, None) => bail!(
            "--lanes needs --fresh-cmd (PMOS_EVALS_FRESH_CMD): how a lane gets a fresh tool server"
        ),
        (false, Some(program)) => {
            if let Some(c) = args.concurrency.filter(|c| *c != urls.len()) {
                eprintln!(
                    "note: --concurrency {c} ignored: isolated attempts run one per lane, {} at once",
                    urls.len()
                );
            }
            Ok(Some(Isolation {
                lanes: urls
                    .iter()
                    .map(|u| Runtime::new(u, token, trace_token))
                    .collect(),
                urls,
                fresh: pmos_evals::isolate::command(program.clone()),
                how: format!("`{}` before each", file_name(program)),
            }))
        }
    }
}

#[derive(Args)]
struct GateArgs {
    #[command(flatten)]
    run: RunArgs,
    /// The baseline report (default: the category's latest report at --baseline-ref).
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// The git ref whose reports hold the baseline.
    #[arg(long, env = "PMOS_EVALS_BASELINE_REF", default_value = "origin/main")]
    baseline_ref: String,
    /// Take the baseline from the latest report in this directory instead of a git ref (such
    /// as the base branch's evals/reports, unpacked with `git archive`).
    #[arg(long, conflicts_with = "baseline")]
    baseline_reports: Option<PathBuf>,
    /// Gate this report instead of running the category.
    #[arg(long)]
    candidate: Option<PathBuf>,
}

#[derive(Args)]
struct LoadArgs {
    /// How many tasks to send at once.
    #[arg(long)]
    tasks: usize,
    /// The category whose bot and asks are used.
    #[arg(long, default_value = "docs-qa")]
    category: String,
    /// Also fail on a missed latency target. Off by default: latency is reported, and only
    /// tasks that don't finish fail the test.
    #[arg(long)]
    enforce_targets: bool,
    #[command(flatten)]
    conn: Conn,
}

struct Resolved {
    url: String,
    rt: Runtime,
    requested_by: String,
    follow: Follow,
}

fn resolve(conn: &Conn, config: &Config, trace_token: Option<&str>) -> Result<Resolved> {
    let Some(token) = conn.token.as_deref().filter(|t| !t.is_empty()) else {
        bail!("no runtime token: pass --token or set PMOS_EVALS_TOKEN (the `evals` client's token in PMOS_BOTS_TOKENS)");
    };
    let url = conn
        .bots_url
        .clone()
        .unwrap_or_else(|| config.runner.bots_url.clone());
    let requested_by = conn
        .requested_by
        .clone()
        .unwrap_or_else(|| config.runner.requested_by.clone());
    let follow = Follow {
        poll: Duration::from_millis(conn.poll_ms.unwrap_or(config.runner.poll_ms).max(1)),
        timeout: Duration::from_secs(
            conn.timeout_secs
                .unwrap_or(config.runner.task_timeout_seconds),
        ),
    };
    Ok(Resolved {
        rt: Runtime::new(&url, token, trace_token),
        url,
        requested_by,
        follow,
    })
}

/// Run a category as `run` and `gate` do, and write its report, compared with `previous`.
/// Returns the report and its markdown path.
async fn run_and_write(
    root: &Path,
    args: RunArgs,
    previous: Option<(String, Report)>,
) -> Result<(Report, PathBuf)> {
    let config = Config::load(root).map_err(anyhow::Error::msg)?;
    let category = load_category(root, &args.category)?;
    for id in &args.tasks {
        if !category.tasks.iter().any(|t| &t.id == id) {
            bail!("--task {id}: no such task in {}", category.name);
        }
    }
    if args.repeats == Some(0) {
        bail!("--repeats must be at least 1");
    }
    let r = resolve(&args.conn, &config, args.trace_token.as_deref())?;
    let isolation = isolation(
        &args,
        args.conn.token.as_deref().unwrap_or_default(),
        args.trace_token.as_deref(),
    )?;
    // Which models the gateway serves, asked before the run (and again after it, below), so
    // the report records them without anyone typing them (N5). They are asked of the runtime
    // under test (every lane's, when isolated), which asks its own gateway: the gateway the
    // bots under test used, not whichever one PMOS_GATEWAY_URL names here (review finding R3).
    let under_test: Vec<Runtime> = match &isolation {
        Some(i) => i.lanes.clone(),
        None => vec![r.rt.clone()],
    };
    let models_before = runtime_models(&under_test).await;
    let persona_version = category.persona.as_deref().map(|p| {
        let v = pmos_evals::history::persona_version(&args.content, p);
        if v.is_none() {
            eprintln!(
                "warning: no `version` for persona `{p}` in {}",
                args.content
                    .join("personas")
                    .join(p)
                    .join("PERSONA.md")
                    .display()
            );
        }
        v
    });
    // What counts is the persona's files, by hash, not the version they claim.
    let persona_hash = category.persona.as_deref().map(|p| {
        let h = pmos_evals::history::persona_hash(&args.content, p);
        if let Err(e) = &h {
            eprintln!("warning: can't hash persona `{p}`'s files ({e}); this run won't count towards its bar");
        }
        h.ok()
    });
    let applicable: Vec<_> = load_judges(root)
        .map_err(|e| anyhow::anyhow!(e.join("\n")))?
        .into_iter()
        .filter(|j| j.applies_to(&category.name))
        .collect();
    let (judges, judges_skipped) = if applicable.is_empty() {
        (None, None)
    } else if args.no_judges {
        (None, Some("--no-judges".to_string()))
    } else if let Some(gateway) = args.gateway.gateway() {
        let mut judges = Vec::new();
        for j in applicable {
            let cal = load_calibration(root, &j).map_err(anyhow::Error::msg)?;
            let status = judge::status(&j, cal.as_ref());
            judges.push((j, status));
        }
        let labels = args
            .label_candidates
            .as_ref()
            .map(|_| pmos_evals::run::LabelSink::default());
        (
            Some(JudgeRun {
                gateway,
                judges,
                labels,
            }),
            None,
        )
    } else {
        eprintln!("warning: judges apply to {} but no gateway is configured (PMOS_GATEWAY_URL and PMOS_EVALS_GATEWAY_TOKEN); not asking them", category.name);
        (
            None,
            Some("no gateway configured (PMOS_GATEWAY_URL and PMOS_EVALS_GATEWAY_TOKEN)".into()),
        )
    };
    let opts = RunOptions {
        requested_by: r.requested_by.clone(),
        follow: r.follow.clone(),
        concurrency: args.concurrency.unwrap_or(config.runner.concurrency).max(1),
        date: args.date.unwrap_or_else(|| Utc::now().date_naive()),
        note: args.note,
        gateway_workarounds: std::env::var("PMOS_GATEWAY_WORKAROUNDS").ok(),
        only: args.tasks,
        repeats: args.repeats.unwrap_or(category.repeats),
        decline_phrases: config.decline_phrases.clone(),
        judges,
        judges_skipped,
        isolation,
    };
    let dir = args.reports_dir.unwrap_or_else(|| root.join("reports"));
    let previous = match previous {
        Some(p) => Some(p),
        None => latest_report(&dir, &category.name).map_err(anyhow::Error::msg)?,
    };
    let mut report = run_category(&r.rt, &r.url, &category, &opts).await;
    report.persona_version = persona_version.flatten();
    report.persona_hash = persona_hash.flatten();
    report.gateway_models = match models_before {
        Err(e) => {
            eprintln!("warning: couldn't learn which models the runtime's gateway serves ({e}); the report won't count towards a bar");
            None
        }
        Ok(before) => match runtime_models(&under_test).await {
            Ok(after) if after == before => Some(before),
            Ok(_) => {
                eprintln!("warning: the gateway's models changed during the run; the report records none and won't count towards a bar");
                None
            }
            Err(e) => {
                eprintln!("warning: couldn't learn which models the runtime's gateway serves after the run ({e}); the report won't count towards a bar");
                None
            }
        },
    };
    if let Some(m) = &report.gateway_models {
        if m.class != "production" {
            eprintln!(
                "note: the runtime's gateway is a `{}` gateway, so this run counts towards no bar",
                m.class
            );
        }
    }
    if let (Some(out), Some(sink)) = (
        args.label_candidates.as_ref(),
        opts.judges.as_ref().and_then(|j| j.labels.as_ref()),
    ) {
        let lines = std::mem::take(&mut *sink.lock().unwrap_or_else(|p| p.into_inner()));
        let written = judge::write_candidates(out, &report.date, &category.name, &lines)
            .with_context(|| format!("writing label candidates into {}", out.display()))?;
        for (path, n) in written {
            eprintln!("label candidates: {n} in {}", path.display());
        }
    }
    let path = pmos_evals::report::write_with(
        &dir,
        &report,
        previous.as_ref().map(|(n, r)| (n.as_str(), r)),
    )
    .with_context(|| format!("writing the report into {}", dir.display()))?;
    Ok((report, path))
}

/// What the gateway of every runtime under test serves, as each runtime reports it
/// (`GatewayModels`, review finding R3). `Err` when one can't say, or two say different
/// things (the lanes must share one gateway for a report to say which models it measured).
async fn runtime_models(
    runtimes: &[Runtime],
) -> std::result::Result<pmos_evals::report::GatewayModels, String> {
    let mut seen: Option<pmos_evals::report::GatewayModels> = None;
    for rt in runtimes {
        let m = rt.gateway_models().await?;
        match &seen {
            Some(first) if *first != m => {
                return Err("the runtimes under test report different gateways".into())
            }
            Some(_) => {}
            None => seen = Some(m),
        }
    }
    seen.ok_or_else(|| "no runtime under test".to_string())
}

fn read_report(path: &Path) -> Result<Report> {
    pmos_evals::report::read(path).map_err(anyhow::Error::msg)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `pmos-evals check`'s line for a category: "staff-craft: 8 tasks for bot `personal`, no
/// bar (reports only), level staff".
fn check_line(c: &pmos_evals::spec::Category) -> String {
    format!(
        "{}: {} tasks for bot `{}`, {}{}{}{}",
        c.name,
        c.tasks.len(),
        c.bot,
        match c.pass_bar {
            Some(b) => format!("bar {:.0}%", b * 100.0),
            None => "no bar (reports only)".into(),
        },
        if c.repeats > 1 {
            format!(", {} repeats", c.repeats)
        } else {
            String::new()
        },
        match &c.level {
            Some(level) => format!(", level {level}"),
            None => String::new(),
        },
        match &c.persona {
            Some(persona) => format!(", as persona `{persona}`"),
            None => String::new(),
        }
    )
}

/// A category's `persona` must be one of the content's personas, offered, so its tasks pass
/// the runtime's own check (`docs/bot-marketplace-spec.md` §10). The catalog is the lint's
/// (`report.personas`: personas whose files pass), read once into `catalog`.
fn check_persona(
    root: &Path,
    category: &str,
    persona: &str,
    content: &Path,
    catalog: &mut Option<Vec<pmos_contract::personas::Persona>>,
) -> Vec<String> {
    let personas = catalog.get_or_insert_with(|| {
        let today = chrono::Utc::now().date_naive();
        pmos_contract::lint(content, &pmos_contract::LintOptions::for_service(today)).personas
    });
    let shown = root
        .join("categories")
        .join(category)
        .join("category.yaml")
        .display()
        .to_string();
    match personas.iter().find(|p| p.name == persona) {
        None => vec![format!(
            "{shown}: `persona` `{persona}` is not a persona in {} (personas/<key>/PERSONA.md)",
            content.display()
        )],
        Some(p) if !p.offered => vec![format!(
            "{shown}: `persona` `{persona}` is not offered (`offered: false`), so the runtime refuses its tasks"
        )],
        Some(_) => Vec::new(),
    }
}

fn check(root: &Path, category: Option<String>, content: &Path) -> Result<bool> {
    let mut problems: Vec<String> = Vec::new();
    let all = category_names(root)?;
    let names = match category {
        Some(c) => vec![c],
        None => all.clone(),
    };
    if names.is_empty() {
        bail!("no categories under {}", root.join("categories").display());
    }
    let config = Config::load(root);
    if let Err(e) = &config {
        problems.push(e.clone());
    }
    // The production role models every counted run is matched against (N5).
    if config.is_ok() {
        match production_models(root) {
            Ok(m) => println!("production role models: {}", m.describe()),
            Err(e) => problems.push(e),
        }
    }
    let registry = std::fs::read_to_string(content.join("tools.yaml"))
        .ok()
        .and_then(|t| pmos_contract::model::ToolRegistry::from_yaml(&t).ok());
    if registry.is_none() {
        eprintln!(
            "note: no {}; drafts tools not checked against the registry",
            content.join("tools.yaml").display()
        );
    }
    let taxonomy = match Taxonomy::load(root) {
        Ok(t) => Some(t),
        Err(e) => {
            problems.extend(e);
            None
        }
    };
    let mut used: Vec<(String, String)> = Vec::new();
    let mut declines = false;
    // The content's personas, linted once, when a category first names one.
    let mut catalog: Option<Vec<pmos_contract::personas::Persona>> = None;
    for name in &names {
        match load_category(root, name) {
            Ok(c) => {
                println!("{}", check_line(&c));
                if let Some(persona) = &c.persona {
                    problems.extend(check_persona(root, &c.name, persona, content, &mut catalog));
                    if persona_version(content, persona).is_none() {
                        problems.push(format!(
                            "{}: persona `{persona}` has no `version` in its PERSONA.md (reports show it)",
                            c.name
                        ));
                    }
                    if let Err(e) = persona_hash(content, persona) {
                        problems.push(format!(
                            "{}: persona `{persona}`'s files can't be hashed ({e}), so no run of this category could count towards its bar",
                            c.name
                        ));
                    }
                }
                if let Some(reg) = &registry {
                    problems.extend(check_draft_tools(root, &c, reg));
                }
                for t in &c.tasks {
                    declines |= t.expect.must_decline;
                    for tag in &t.tags {
                        used.push((format!("{}/tasks/{}.yaml", c.name, t.id), tag.clone()));
                    }
                }
            }
            Err(e) => problems.extend(e.0),
        }
    }
    if declines {
        if let Ok(c) = &config {
            if c.decline_phrases.is_empty() {
                problems.push(format!(
                    "{}: `must_decline` tasks need `decline_phrases`",
                    root.join("config.yaml").display()
                ));
            }
        }
    }
    match load_judges(root) {
        Ok(judges) => {
            for j in judges {
                for c in &j.applies_to {
                    if !all.contains(c) {
                        problems.push(format!(
                            "judges/{}.yaml: applies_to `{c}` is not a category",
                            j.name
                        ));
                    }
                }
                used.push((format!("judges/{}.yaml", j.name), j.failure_mode.clone()));
                let labels = load_labels(root, &j).unwrap_or_default();
                let n = labels.iter().filter(|l| l.label.answer().is_some()).count();
                let count = |h: judge::Human| labels.iter().filter(|l| l.label == h).count();
                let (unknown, pending) =
                    (count(judge::Human::Unknown), count(judge::Human::Pending));
                let extra = match (unknown, pending) {
                    (0, 0) => String::new(),
                    _ => format!(" ({unknown} labelled unknown, {pending} waiting for a person)"),
                };
                let cal = load_calibration(root, &j).ok().flatten();
                println!(
                    "judge {}: {n} labels{extra}, {}",
                    j.name,
                    judge::status(&j, cal.as_ref()).reason
                );
            }
        }
        Err(e) => problems.extend(e),
    }
    match load_all(&root.join("reports")) {
        Ok(reports) => {
            for (file, r) in reports {
                let subs: BTreeSet<String> = r
                    .tasks
                    .iter()
                    .filter_map(|t| t.tag.as_ref().and_then(|x| x.sub.clone()))
                    .collect();
                for s in subs {
                    used.push((format!("reports/{file}"), s));
                }
            }
        }
        Err(e) => problems.push(e),
    }
    if let Some(t) = &taxonomy {
        for (place, tag) in used {
            if !t.contains(&tag) {
                problems.push(format!(
                    "{place}: tag `{tag}` is not in {}",
                    root.join("taxonomy.yaml").display()
                ));
            }
        }
    }
    if !problems.is_empty() {
        eprintln!(
            "{} problem{} in the eval files:",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
        for p in &problems {
            eprintln!("  - {p}");
        }
        bail!("the eval files have problems (above)");
    }
    Ok(true)
}

async fn real_main(cli: Cli) -> Result<bool> {
    let root = cli.root;
    match cli.command {
        Command::Check { category, content } => check(&root, category, &content),
        Command::Run(args) => {
            let (report, path) = run_and_write(&root, args, None).await?;
            let rate = format!(
                "{} of {} passed ({:.0}%",
                report.passed(),
                report.total(),
                report.pass_rate() * 100.0
            );
            match (report.pass_bar, report.met_bar()) {
                (Some(bar), Some(met)) => println!(
                    "{}: {rate}, bar {:.0}%): {}",
                    report.category,
                    bar * 100.0,
                    if met { "met" } else { "below the bar" }
                ),
                _ => println!("{}: {rate}): reports only, no pass bar", report.category),
            }
            if let Some((tag, n)) = report.failures_by_tag().first() {
                println!("top bottleneck: {tag} ({n})");
            }
            println!("report: {}", path.display());
            Ok(report.met_bar().unwrap_or(true))
        }
        Command::Gate(args) => {
            let category = args.run.category.clone();
            let reports_dir = args
                .run
                .reports_dir
                .clone()
                .unwrap_or_else(|| root.join("reports"));
            let (baseline, source) = match (&args.baseline, &args.baseline_reports) {
                (Some(p), _) => (
                    Some((file_name(p), read_report(p)?)),
                    format!("`{}`", p.display()),
                ),
                (None, Some(dir)) => (
                    latest_report(dir, &category).map_err(anyhow::Error::msg)?,
                    format!("`{}`", dir.display()),
                ),
                (None, None) => (
                    baseline_from_git(&reports_dir, &args.baseline_ref, &category)
                        .map_err(anyhow::Error::msg)?,
                    format!("`{}`", args.baseline_ref),
                ),
            };
            if let Some((name, b)) = &baseline {
                if b.category != category {
                    bail!(
                        "the baseline {name} is for `{}`, not `{category}`",
                        b.category
                    );
                }
            }
            let (candidate, name) = match &args.candidate {
                Some(p) => (read_report(p)?, file_name(p)),
                None => {
                    let (r, path) = run_and_write(&root, args.run, baseline.clone()).await?;
                    eprintln!("report: {}", path.display());
                    (r, file_name(&path.with_extension("json")))
                }
            };
            if candidate.category != category {
                bail!(
                    "the candidate {name} is for `{}`, not `{category}`",
                    candidate.category
                );
            }
            let result = decide(baseline, &source, candidate, &name);
            print!("{}", result.to_markdown());
            Ok(result.passed())
        }
        Command::Compare {
            baseline,
            candidate,
        } => {
            let (a, b) = (read_report(&baseline)?, read_report(&candidate)?);
            if a.category != b.category {
                bail!(
                    "{} is `{}` and {} is `{}`; compare runs of one category",
                    file_name(&baseline),
                    a.category,
                    file_name(&candidate),
                    b.category
                );
            }
            println!(
                "# Compare: {} ({}) → {} ({})\n",
                file_name(&baseline),
                a.date,
                file_name(&candidate),
                b.date
            );
            print!("{}", compare(&a, &b).to_markdown());
            Ok(true)
        }
        Command::History {
            category,
            reports_dir,
            content,
        } => {
            let cat = load_category(&root, &category)?;
            let Some(bar) = cat.pass_bar else {
                println!("{category}: has no pass bar (reports only), so it can't meet one.");
                return Ok(false);
            };
            let dir = reports_dir.unwrap_or_else(|| root.join("reports"));
            let reports = load_named(&dir, &category).map_err(anyhow::Error::msg)?;
            let production = production_models(&root);
            let persona_now = cat
                .persona
                .as_deref()
                .map(|p| match persona_hash(&content, p) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        println!(
                            "{category}: persona `{p}`'s files in {} can't be hashed ({e}), so no run counts.",
                            content.display()
                        );
                        None
                    }
                });
            let (kept, left) = match &persona_now {
                Some(None) => (Vec::new(), Vec::new()),
                Some(Some(v)) => split_counting(reports, &production, Some(v)),
                None => split_counting(reports, &production, None),
            };
            let h = evaluate(&category, bar, &kept);
            print!("{}", h.to_text());
            if !left.is_empty() {
                println!("Not counted ({}):", left.len());
                for (file, date, why) in &left {
                    println!("  {date} {file}: {why}");
                }
            }
            Ok(h.two_weeks_met)
        }
        Command::Load(args) => {
            if args.tasks == 0 {
                bail!("--tasks must be at least 1");
            }
            let config = Config::load(&root).map_err(anyhow::Error::msg)?;
            let category = load_category(&root, &args.category)?;
            let r = resolve(&args.conn, &config, None)?;
            let bot = resolve_bot(&category.bot, &r.requested_by);
            let asks: Vec<String> = category
                .tasks
                .iter()
                .map(|t| t.first_ask().to_string())
                .collect();
            let result = run_load(
                &r.rt,
                &bot,
                &r.requested_by,
                &asks,
                pmos_evals::run::AskAs {
                    level: category.level.as_deref(),
                    persona: category.persona.as_deref(),
                },
                args.tasks,
                &r.follow,
            )
            .await;
            let targets = &config.latency_targets;
            print!("{}", result.to_markdown(&bot, targets));
            Ok(result.verdict(targets, args.enforce_targets))
        }
        Command::Judge {
            command:
                JudgeCommand::Pack {
                    name,
                    from,
                    per_judge,
                },
        } => {
            let judges = load_judges(&root).map_err(|e| anyhow::anyhow!(e.join("\n")))?;
            let chosen: Vec<_> = judges
                .into_iter()
                .filter(|j| name.as_ref().is_none_or(|n| &j.name == n))
                .collect();
            if chosen.is_empty() {
                bail!("no judge `{}`", name.unwrap_or_default());
            }
            for j in chosen {
                let p = judge::pack(&root, &j, &from, per_judge).map_err(anyhow::Error::msg)?;
                println!(
                    "{}: added {} line(s) to {} ({} the judge said no to, {} yes, {} unknown; {} already there), each waiting for a person's label",
                    j.name,
                    p.no + p.yes + p.unknown,
                    j.labels_path(&root).display(),
                    p.no,
                    p.yes,
                    p.unknown,
                    p.already
                );
            }
            Ok(true)
        }
        Command::Judge {
            command:
                JudgeCommand::Calibrate {
                    name,
                    gateway,
                    date,
                },
        } => {
            let judges = load_judges(&root).map_err(|e| anyhow::anyhow!(e.join("\n")))?;
            let Some(j) = judges.into_iter().find(|j| j.name == name) else {
                bail!(
                    "no judge `{name}` in {}",
                    judge::judges_dir(&root).display()
                );
            };
            let labels = load_labels(&root, &j).map_err(anyhow::Error::msg)?;
            if !labels.iter().any(|l| l.label.answer().is_some()) {
                let pending = labels
                    .iter()
                    .filter(|l| l.label == judge::Human::Pending)
                    .count();
                bail!(
                    "{}: no labels yet ({pending} line(s) waiting for a person); a person sets `label` to true, false or \"unknown\" first (docs/evals.md, Labelling a judge)",
                    j.labels_path(&root).display()
                );
            }
            let Some(gw) = gateway.gateway() else {
                bail!("no gateway: set PMOS_GATEWAY_URL and PMOS_EVALS_GATEWAY_TOKEN (the `evals` client's token in PMOS_GATEWAY_TOKENS)");
            };
            let mut replies = Vec::new();
            for (i, l) in labels.iter().enumerate() {
                // Only labels a person answered yes or no are asked about.
                if l.label.answer().is_none() {
                    replies.push(judge::Reply::Unparsed);
                    continue;
                }
                let v = gw
                    .ask(&j, &l.input, &l.output)
                    .await
                    .map_err(|e| anyhow::anyhow!("label {}: {e}", i + 1))?;
                replies.push(v);
            }
            let truth: Vec<judge::Human> = labels.iter().map(|l| l.label).collect();
            let date = date.unwrap_or_else(|| Utc::now().date_naive());
            let cal = judge::calibrate(&j, date, &truth, &replies);
            let path = j.calibration_path(&root);
            std::fs::write(&path, serde_json::to_string_pretty(&cal)? + "\n")
                .with_context(|| format!("writing {}", path.display()))?;
            let rate =
                |r: Option<f64>| r.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into());
            println!(
                "{}: n {} ({} yes, {} no), TPR {}, TNR {}, {} unparsed ({} unknown); left out: {} labelled unknown, {} not labelled yet",
                j.name,
                cal.n,
                cal.positives,
                cal.negatives,
                rate(cal.tpr),
                rate(cal.tnr),
                cal.unparsed,
                cal.judge_unknown,
                cal.labels_unknown,
                cal.labels_pending
            );
            println!("{}", judge::status(&j, Some(&cal)).reason);
            println!("wrote {}", path.display());
            Ok(true)
        }
        Command::Affected {
            changed,
            content,
            content_prefix,
            requested_by,
        } => {
            let text = match changed.as_deref() {
                Some(p) if p != Path::new("-") => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
                _ => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let paths: Vec<String> = text.lines().map(str::to_string).collect();
            let requested_by = match requested_by {
                Some(r) => r,
                None => {
                    Config::load(&root)
                        .map_err(anyhow::Error::msg)?
                        .runner
                        .requested_by
                }
            };
            let names = pmos_evals::affected::affected(
                &root,
                &content,
                &content_prefix,
                &requested_by,
                &paths,
            )
            .map_err(anyhow::Error::msg)?;
            for n in names {
                println!("{n}");
            }
            Ok(true)
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match real_main(cli).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("pmos-evals: {e:#}");
            ExitCode::from(2)
        }
    }
}
