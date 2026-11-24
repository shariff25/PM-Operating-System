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
    /// Check skills, context, reviewers, templates, roles, bots and the bot marketplace's
    /// personas against the content contract.
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
        /// The evals directory whose categories and reports decide which marketplace bots are
        /// benchmarked (`persona-benchmark`, D10). Default: `evals` beside the content root.
        /// A missing directory benchmarks none, so every offered bot must be a Preview.
        #[arg(long)]
        evals: Option<PathBuf>,
    },
    /// Each bot's OpenShell sandbox policy (docs/sandbox-policies.md).
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Bots in the content.
    Bot {
        #[command(subcommand)]
        command: BotCommand,
    },
    /// Checks on the content checkout as a whole.
    Content {
        #[command(subcommand)]
        command: ContentCommand,
    },
}

#[derive(Subcommand)]
enum ContentCommand {
    /// Fake-data mode's content check (docs/openshell-any-host-spec.md §3.6): the content
    /// root has a TEST-CONTENT marker and no file under it is marked `sensitivity: restricted`.
    /// Exits 1 with one line per cause; `pmos-admin sandbox` runs it before it uploads anything
    /// under PMOS_MODEL_SOURCE=hosted-test.
    CheckTest {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
    },
}

#[derive(Subcommand)]
enum BotCommand {
    /// Write the personal bot `pm-<handle>` (`bots/pm-<handle>/BOT.md`) and nothing else: the
    /// person themselves is added to the PM OS user database on the host
    /// (`pmos-admin users …`, docs/runbooks/users.md). Review and commit it like any other
    /// content change, then run `pmos policy generate` for its sandbox policy.
    AddPersonal {
        /// Path to the content root.
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// The person's PM OS handle: lowercase letters, digits and single hyphens.
        #[arg(long)]
        handle: String,
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
    /// Prove every bot's policy against the hand-written boundary with OpenShell's pinned
    /// policy prover (docs/openshell-prover-and-sandbox-audit-spec.md §A.3), beside
    /// `pmos policy check`. Only `proved` passes. Exits 0 when every bot is proved and nothing
    /// disagrees with the check, 1 otherwise (a missing or unpinned prover included), 2 when it
    /// could not run.
    Prove {
        /// Path to the content root (for `pmos policy check`'s rules beside each proof).
        #[arg(long, default_value = "content")]
        root: PathBuf,
        /// Path to the policies: every `bots/<bot>.yaml` is proved.
        #[arg(long, default_value = "policies")]
        policies: PathBuf,
        /// The boundary (CI passes the base branch's copy).
        #[arg(long, default_value = pmos_policy::prove::BOUNDARY_PATH)]
        boundary: PathBuf,
        /// The prover binary. Default: `openshell-prover` on PATH.
        #[arg(long)]
        prover: Option<PathBuf>,
        /// The pin: the source commit, the version, and optional release SHA-256s.
        #[arg(long, default_value = pmos_policy::prove::LOCK_PATH)]
        lock: PathBuf,
        /// The build record a source build of the pinned commit wrote (the commit, the version
        /// and the binary's SHA-256; spec §A.7). Its commit and version must be the lock's, and
        /// the binary must have its SHA-256.
        #[arg(long)]
        build_record: Option<PathBuf>,
        /// Prove this one file instead (a sandbox's effective policy on the host); needs --bot.
        #[arg(long, requires = "bot")]
        candidate: Option<PathBuf>,
        /// The bot the --candidate belongs to.
        #[arg(long, requires = "candidate")]
        bot: Option<String>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// The boundary-alone rule (spec §A.6): a change to policies/boundary/ may change no other
    /// file. Reads the changed paths, one per line (`git diff --name-only base...head`), from
    /// --changed-files (`-` for standard input). Exits 1 when the rule is broken.
    BoundaryAlone {
        #[arg(long)]
        changed_files: PathBuf,
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
            evals,
        } => {
            // `team-file-retired` is an error here, as in CI (`LintOptions::new`), and so is
            // an offered marketplace bot that is neither benchmarked nor a Preview.
            let evals = evals.unwrap_or_else(|| evals_beside(&root));
            let opts = LintOptions {
                stale_after_days,
                benchmarked: Some(pmos_evals::history::benchmarked_personas(&evals, &root)),
                ..LintOptions::new(today.unwrap_or_else(|| Local::now().date_naive()))
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
        Command::Policy {
            command:
                PolicyCommand::Prove {
                    root,
                    policies,
                    boundary,
                    prover,
                    lock,
                    build_record,
                    candidate,
                    bot,
                    format,
                },
        } => Ok(policy_prove(&ProveArgs {
            root,
            policies,
            boundary,
            prover,
            lock,
            build_record,
            candidate: candidate.zip(bot),
            format,
        })),
        Command::Policy {
            command: PolicyCommand::BoundaryAlone { changed_files },
        } => boundary_alone(&changed_files),
        Command::Bot {
            command: BotCommand::AddPersonal { root, handle },
        } => bot_add_personal(&root, &handle),
        Command::Content {
            command: ContentCommand::CheckTest { root },
        } => Ok(content_check_test(&root)),
    }
}

/// `pmos content check-test`: the content check of fake-data mode.
fn content_check_test(root: &Path) -> ExitCode {
    match pmos_contract::model_source::check_test_content(root) {
        Ok(()) => {
            println!(
                "{}: test content (a TEST-CONTENT marker, no file marked restricted)",
                root.display()
            );
            ExitCode::SUCCESS
        }
        Err(causes) => {
            for c in causes {
                eprintln!(
                    "error: {}",
                    pmos_contract::model_source::fake_data_refusal(&c)
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// `pmos bot add-personal`: a person's personal bot, and nothing else. The person is added to
/// the user database on the host (`pmos-admin users …`); there is no team file (slice U7).
fn bot_add_personal(root: &Path, handle: &str) -> Result<ExitCode> {
    use pmos_contract::onboarding::{
        bot_path, personal_bot_name, render_personal_bot, valid_new_handle, MAX_PROPOSED_HANDLE,
    };
    if !valid_new_handle(handle) {
        eprintln!(
            "error: `{handle}` is not a usable handle: lowercase letters, digits and single hyphens, at most {MAX_PROPOSED_HANDLE}"
        );
        return Ok(ExitCode::FAILURE);
    }
    if !root.is_dir() {
        eprintln!("error: no content root at {}", root.display());
        return Ok(ExitCode::FAILURE);
    }
    let bot = root.join(bot_path(&personal_bot_name(handle)));
    if bot.exists() {
        println!("{} is already there; nothing changed", bot.display());
        return Ok(ExitCode::SUCCESS);
    }
    std::fs::create_dir_all(bot.parent().expect("bot path has a folder"))?;
    std::fs::write(&bot, render_personal_bot(handle))?;
    println!(
        "wrote {}; run `pmos policy generate` for its sandbox policy",
        bot.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `evals` beside the content root (`content` and `evals` are siblings in the repository).
fn evals_beside(root: &Path) -> PathBuf {
    let parent = match root.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    parent.join("evals")
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
                "{} files checked, {} skills, {} personas, {errors} errors, {warnings} warnings",
                report.files_checked,
                report.skills.len(),
                report.personas.len()
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

struct ProveArgs {
    root: PathBuf,
    policies: PathBuf,
    boundary: PathBuf,
    prover: Option<PathBuf>,
    lock: PathBuf,
    build_record: Option<PathBuf>,
    candidate: Option<(PathBuf, String)>,
    format: Format,
}

/// `openshell-prover` on PATH, or a path that doesn't exist (which is `not_available`).
fn prover_on_path() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join("openshell-prover"))
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("openshell-prover (not found on PATH)"))
}

/// `pmos policy prove`.
fn policy_prove(a: &ProveArgs) -> ExitCode {
    use pmos_policy::prove::{self, Candidate, Lock, Prover};
    use std::collections::BTreeSet;
    let lock = match Lock::read(&a.lock) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: the prover pin can't be read: {e}");
            return ExitCode::from(2);
        }
    };
    let build_record = match &a.build_record {
        None => None,
        Some(path) => match Lock::read(path) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("error: the prover's build record can't be read: {e}");
                return ExitCode::from(2);
            }
        },
    };
    if !a.boundary.is_file() {
        eprintln!("error: no boundary at {}", a.boundary.display());
        return ExitCode::from(2);
    }
    let mapped = prove::all_mapped_rules();
    let today = Local::now().date_naive();
    let candidates: Vec<Candidate> = match &a.candidate {
        Some((path, bot)) => {
            if !pmos_contract::bots::valid_name(bot) {
                eprintln!("error: `{bot}` is not a bot name");
                return ExitCode::from(2);
            }
            // The check's rules on this one file, for this bot as the content defines it.
            let failed: BTreeSet<String> = match (
                std::fs::read_to_string(path),
                pmos_policy::generate(&a.root, today),
            ) {
                (Ok(text), Ok(g)) => {
                    let ctx = g
                        .bots
                        .iter()
                        .find(|c| c.bot == *bot)
                        .cloned()
                        .unwrap_or_else(|| pmos_policy::BotContext::unknown(bot));
                    pmos_policy::check_policy(&ctx, &path.display().to_string(), &text)
                        .into_iter()
                        .map(|v| v.rule.to_string())
                        .filter(|r| mapped.contains(r.as_str()))
                        .collect()
                }
                _ => BTreeSet::new(),
            };
            vec![Candidate {
                bot: bot.clone(),
                path: path.clone(),
                label: path.display().to_string(),
                check_failed: failed,
            }]
        }
        None => {
            let found = match prove::candidates(&a.policies) {
                Ok(f) if !f.is_empty() => f,
                Ok(_) => {
                    eprintln!("error: no policies under {}/bots", a.policies.display());
                    return ExitCode::from(2);
                }
                Err(e) => {
                    eprintln!("error: {}/bots: {e}", a.policies.display());
                    return ExitCode::from(2);
                }
            };
            let report = pmos_policy::check(&a.root, &a.policies, today);
            found
                .into_iter()
                .map(|(bot, path)| {
                    let suffix = format!("bots/{bot}.yaml");
                    let nested = format!("/{suffix}");
                    let failed = report
                        .violations
                        .iter()
                        .filter(|v| v.path == suffix || v.path.ends_with(&nested))
                        .map(|v| v.rule.to_string())
                        .filter(|r| mapped.contains(r.as_str()))
                        .collect();
                    Candidate {
                        label: path.display().to_string(),
                        bot,
                        path,
                        check_failed: failed,
                    }
                })
                .collect()
        }
    };
    let scratch = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: no scratch directory: {e}");
            return ExitCode::from(2);
        }
    };
    let mut prover = Prover::new(a.prover.clone().unwrap_or_else(prover_on_path), lock);
    prover.build_record = build_record;
    let report = prove::prove_all(&prover, &a.boundary, &candidates, scratch.path());
    match a.format {
        Format::Json => match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        Format::Text => print!("{}", prove::render_text(&report)),
    }
    exit(!report.passed())
}

/// `pmos policy boundary-alone`.
fn boundary_alone(changed_files: &Path) -> Result<ExitCode> {
    let text = if changed_files == Path::new("-") {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        std::fs::read_to_string(changed_files)
            .with_context(|| format!("reading {}", changed_files.display()))?
    };
    let changed: Vec<&str> = text.lines().collect();
    match pmos_policy::prove::boundary_alone(&changed) {
        Ok(()) => {
            println!("boundary-alone: ok");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("error[boundary-alone] {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}
