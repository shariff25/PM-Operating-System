use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use clap::{Args, Parser, Subcommand};
use pmos_evals::client::Runtime;
use pmos_evals::config::Config;
use pmos_evals::history::{evaluate, load_reports};
use pmos_evals::load::run_load;
use pmos_evals::run::{run_category, Follow, RunOptions};
use pmos_evals::spec::{category_names, load_category};

/// PM OS benchmarks: run a category through the bot runtime, grade it, and keep the history.
///
/// Exit status: 0 when the bar (or target) is met, 1 when it isn't, 2 on an error.
#[derive(Parser)]
#[command(name = "pmos-evals", version)]
struct Cli {
    /// The evals directory: categories, config.yaml and reports.
    #[arg(long, global = true, env = "PMOS_EVALS_ROOT", default_value = "evals")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the category and task files (every category when none is named).
    Check {
        #[arg(long)]
        category: Option<String>,
    },
    /// Run a category against the bot runtime and write its report.
    Run(RunArgs),
    /// Say whether a category met its bar two weeks in a row, from its reports.
    History {
        #[arg(long)]
        category: String,
        /// Where the reports are (default: <root>/reports).
        #[arg(long)]
        reports_dir: Option<PathBuf>,
    },
    /// Send N tasks at once and report p50 and p90 queue and total time against the targets.
    Load(LoadArgs),
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
    /// Why this run happened, recorded in the report ("weekly", "model swap to …").
    #[arg(long)]
    note: Option<String>,
    /// Run only this task (repeatable).
    #[arg(long = "task")]
    tasks: Vec<String>,
    /// The report date (default: today, UTC).
    #[arg(long)]
    date: Option<NaiveDate>,
    /// Where reports go (default: <root>/reports).
    #[arg(long)]
    reports_dir: Option<PathBuf>,
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

async fn real_main(cli: Cli) -> Result<bool> {
    let root = cli.root;
    match cli.command {
        Command::Check { category } => {
            let names = match category {
                Some(c) => vec![c],
                None => category_names(&root)?,
            };
            if names.is_empty() {
                bail!("no categories under {}", root.join("categories").display());
            }
            let mut ok = true;
            for name in names {
                match load_category(&root, &name) {
                    Ok(c) => println!(
                        "{}: {} tasks for bot `{}`, bar {:.0}%",
                        c.name,
                        c.tasks.len(),
                        c.bot,
                        c.pass_bar * 100.0
                    ),
                    Err(e) => {
                        eprint!("{name}: {e}");
                        ok = false;
                    }
                }
            }
            Config::load(&root).map_err(anyhow::Error::msg)?;
            if !ok {
                bail!("the eval files have problems (above)");
            }
            Ok(true)
        }
        Command::Run(args) => {
            let config = Config::load(&root).map_err(anyhow::Error::msg)?;
            let category = load_category(&root, &args.category)?;
            for id in &args.tasks {
                if !category.tasks.iter().any(|t| &t.id == id) {
                    bail!("--task {id}: no such task in {}", category.name);
                }
            }
            let r = resolve(&args.conn, &config, args.trace_token.as_deref())?;
            let opts = RunOptions {
                requested_by: r.requested_by.clone(),
                follow: r.follow.clone(),
                concurrency: args.concurrency.unwrap_or(config.runner.concurrency).max(1),
                date: args.date.unwrap_or_else(|| Utc::now().date_naive()),
                note: args.note,
                gateway_workarounds: std::env::var("PMOS_GATEWAY_WORKAROUNDS").ok(),
                only: args.tasks,
            };
            let report = run_category(&r.rt, &r.url, &category, &opts).await;
            let dir = args.reports_dir.unwrap_or_else(|| root.join("reports"));
            let path = pmos_evals::report::write(&dir, &report)
                .with_context(|| format!("writing the report into {}", dir.display()))?;
            println!(
                "{}: {} of {} passed ({:.0}%, bar {:.0}%): {}",
                report.category,
                report.passed(),
                report.total(),
                report.pass_rate() * 100.0,
                report.pass_bar * 100.0,
                if report.met_bar() {
                    "met"
                } else {
                    "below the bar"
                }
            );
            if let Some((tag, n)) = report.failures_by_tag().first() {
                println!("top bottleneck: {tag} ({n})");
            }
            println!("report: {}", path.display());
            Ok(report.met_bar())
        }
        Command::History {
            category,
            reports_dir,
        } => {
            let cat = load_category(&root, &category)?;
            let dir = reports_dir.unwrap_or_else(|| root.join("reports"));
            let reports = load_reports(&dir, &category).map_err(anyhow::Error::msg)?;
            let h = evaluate(&category, cat.pass_bar, &reports);
            print!("{}", h.to_text());
            Ok(h.two_weeks_met)
        }
        Command::Load(args) => {
            if args.tasks == 0 {
                bail!("--tasks must be at least 1");
            }
            let config = Config::load(&root).map_err(anyhow::Error::msg)?;
            let category = load_category(&root, &args.category)?;
            let r = resolve(&args.conn, &config, None)?;
            let asks: Vec<String> = category.tasks.iter().map(|t| t.ask.clone()).collect();
            let result = run_load(
                &r.rt,
                &category.bot,
                &r.requested_by,
                &asks,
                args.tasks,
                &r.follow,
            )
            .await;
            let targets = &config.latency_targets;
            print!("{}", result.to_markdown(&category.bot, targets));
            Ok(result.verdict(targets, args.enforce_targets))
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
