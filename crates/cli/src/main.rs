//! `pmos`: the PM OS command line for skill authors (SUR-2) and platform owners.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
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
    /// Each bot's OpenShell sandbox policy (docs/sandbox-policies.md).
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Write policies/bots/<bot>.yaml, <bot>.sandbox.yaml and the provider profiles from the
    /// bots and roles in the content.
    Generate {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// Where to write the policies.
        #[arg(long, default_value = "policies")]
        out: PathBuf,
    },
    /// Check every policy file against the PM OS rules, and that generated files are current.
    /// Exits non-zero on any violation.
    Check {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// Path to the policies.
        #[arg(long, default_value = "policies")]
        policies: PathBuf,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

fn exit(failed: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
            run_lint(&root, &opts, deny_warnings, format)
        }
        Command::Policy {
            command: PolicyCommand::Generate { root, out },
        } => policy_generate(&root, &out),
        Command::Policy {
            command:
                PolicyCommand::Check {
                    root,
                    policies,
                    format,
                },
        } => policy_check(&root, &policies, format),
    }
}

fn run_lint(
    root: &Path,
    opts: &LintOptions,
    deny_warnings: bool,
    format: Format,
) -> Result<ExitCode> {
    let report = lint(root, opts);
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

    Ok(exit(errors > 0 || (deny_warnings && warnings > 0)))
}

fn policy_generate(root: &Path, out: &Path) -> Result<ExitCode> {
    let generated = match pmos_policy::generate(root, Local::now().date_naive()) {
        Ok(g) => g,
        Err(errors) => {
            for d in &errors {
                println!("error[{}] {}: {}", d.code, d.path, d.message);
            }
            println!(
                "the content has {} lint errors; fix them (`pmos lint`) before generating policies",
                errors.len()
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    let summary = pmos_policy::write(out, &generated)
        .with_context(|| format!("writing policies under {}", out.display()))?;
    for (verb, files) in [("wrote", &summary.written), ("removed", &summary.removed)] {
        for rel in files {
            println!("{verb} {}", out.join(rel).display());
        }
    }
    println!(
        "{} bots, {} files written, {} unchanged, {} removed",
        generated.bots.len(),
        summary.written.len(),
        summary.unchanged.len(),
        summary.removed.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn policy_check(root: &Path, policies: &Path, format: Format) -> Result<ExitCode> {
    let report = pmos_policy::check(root, policies, Local::now().date_naive());
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        Format::Text => {
            for v in &report.violations {
                println!("error[{}] {}: {}", v.rule, v.path, v.message);
            }
            println!(
                "{} files checked, {} bots, {} violations",
                report.files_checked,
                report.bots.len(),
                report.violations.len()
            );
        }
    }
    Ok(exit(!report.passed()))
}
