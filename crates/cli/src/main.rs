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
    /// Members in `team.yaml`. Everyone after the first platform owners normally joins
    /// through the bot (docs/onboarding-spec.md); this is for those first entries and tests.
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
}

#[derive(Subcommand)]
enum MemberCommand {
    /// Add a member to `team.yaml` and write their personal bot `pm-<handle>`. Review the
    /// change and commit it like any other content change.
    Add {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// PM OS handle: lowercase letters, digits and single hyphens.
        #[arg(long)]
        handle: String,
        /// Slack member id, such as U012AB3CD.
        #[arg(long)]
        slack_id: String,
        /// Atlassian account email.
        #[arg(long)]
        email: Option<String>,
        /// GitHub login in your GitHub org.
        #[arg(long)]
        github: Option<String>,
        /// Roles, comma-separated: pm, skill-author, reviewer, auditor, admin.
        #[arg(long, default_value = "pm", value_delimiter = ',')]
        roles: Vec<String>,
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
        Command::Member {
            command:
                MemberCommand::Add {
                    root,
                    handle,
                    slack_id,
                    email,
                    github,
                    roles,
                },
        } => member_add(&root, handle, slack_id, email, github, &roles),
    }
}

fn member_add(
    root: &Path,
    handle: String,
    slack_id: String,
    email: Option<String>,
    github: Option<String>,
    roles: &[String],
) -> Result<ExitCode> {
    use pmos_contract::model::{valid_email, valid_slack_id, Role, TeamFile, TeamMember};
    use pmos_contract::onboarding::{
        bot_path, render_personal_bot, render_team_yaml, valid_github_login, valid_new_handle,
    };
    let fail = |m: String| -> Result<ExitCode> {
        eprintln!("error: {m}");
        Ok(ExitCode::FAILURE)
    };
    if !valid_new_handle(&handle) {
        return fail(format!(
            "`{handle}` is not a usable handle: lowercase letters, digits and single hyphens, at most {}",
            pmos_contract::onboarding::MAX_PROPOSED_HANDLE
        ));
    }
    if !valid_slack_id(&slack_id) {
        return fail(format!("`{slack_id}` is not a Slack member id (U… or W…)"));
    }
    if let Some(e) = email.as_deref().filter(|e| !valid_email(e)) {
        return fail(format!("`{e}` is not an email address"));
    }
    if let Some(g) = github.as_deref().filter(|g| !valid_github_login(g)) {
        return fail(format!("`{g}` is not a GitHub login"));
    }
    let mut parsed = Vec::new();
    for r in roles {
        match serde_norway::from_str::<Role>(r.trim()) {
            Ok(role) if !parsed.contains(&role) => parsed.push(role),
            Ok(_) => {}
            Err(_) => return fail(format!("unknown role `{r}`: use pm, skill-author, reviewer, auditor or admin")),
        }
    }
    let path = root.join("team.yaml");
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let mut team =
        TeamFile::from_yaml(&original).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    if team.by_handle(&handle).is_some() || team.by_slack_id(&slack_id).is_some() {
        return fail(format!(
            "`{handle}` or `{slack_id}` is already in team.yaml"
        ));
    }
    team.members.push(TeamMember {
        handle: handle.clone(),
        slack_id,
        email,
        github,
        roles: parsed,
    });
    std::fs::write(&path, render_team_yaml(&original, &team.members))?;
    let bot = root.join(bot_path(&pmos_contract::onboarding::personal_bot_name(
        &handle,
    )));
    if !bot.exists() {
        std::fs::create_dir_all(bot.parent().expect("bot path has a folder"))?;
        std::fs::write(&bot, render_personal_bot(&handle))?;
    }
    println!(
        "added `{handle}` to {} and wrote {}; run `pmos policy generate` for its sandbox policy",
        path.display(),
        bot.display()
    );
    Ok(ExitCode::SUCCESS)
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
