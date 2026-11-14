//! `pmos`: the PM OS command line for skill authors (SUR-2).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use chrono::{Local, NaiveDate};
use clap::{Parser, Subcommand, ValueEnum};
use pmos_contract::{lint, LintOptions, Severity};

#[derive(Parser)]
#[command(name = "pmos", version, about = "PM OS command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check skills, context, reviewers and templates against the content contract.
    Lint {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// Date used for staleness checks (YYYY-MM-DD). Defaults to today.
        #[arg(long)]
        today: Option<NaiveDate>,
        /// Days after last review before a context file is stale.
        #[arg(long, default_value_t = pmos_contract::lint::DEFAULT_STALE_AFTER_DAYS)]
        stale_after_days: i64,
        /// Treat warnings as errors.
        #[arg(long)]
        deny_warnings: bool,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Lint {
            root,
            today,
            stale_after_days,
            deny_warnings,
            format,
        } => {
            let opts = LintOptions {
                today: today.unwrap_or_else(|| Local::now().date_naive()),
                stale_after_days,
            };
            let report = lint(&root, &opts);
            let errors = report.errors().count();
            let warnings = report.warnings().count();

            match format {
                Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                Format::Text => {
                    for d in &report.diagnostics {
                        let level = match d.severity {
                            Severity::Error => "error",
                            Severity::Warning => "warning",
                        };
                        println!("{level}[{}] {}: {}", d.code, d.path, d.message);
                    }
                    println!(
                        "{} files checked, {} skills, {errors} errors, {warnings} warnings",
                        report.files_checked,
                        report.skills.len()
                    );
                }
            }

            let failed = errors > 0 || (deny_warnings && warnings > 0);
            Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
