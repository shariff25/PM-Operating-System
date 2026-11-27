//! `gov`: Agent Governance's command line. `gov verify` is what an auditor runs with nothing
//! but files: the public keys, the witness's checkpoints and an export of receipts.
//! `gov policy check` and `gov policy sign` are the release step for policy (spec §7.6.1):
//! the first needs no key, the second runs wherever the root key is kept. No command here
//! opens a database or a network connection.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use gov_core::Scope;
use gov_ledger::verify::{verify, Check, Problem, Report};
use gov_ledger::{DirWitness, Export, TrustedKeys, Witness};

#[derive(Parser)]
#[command(name = "gov", about = "Agent Governance")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check an export of receipts against the witness's checkpoints.
    Verify {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        env: String,
        /// The trusted public keys: `{ "ledger": [...], "gates": { "<gate>": [...] },
        /// "authorities": { "<authority>": [...] } }`. With an Authority named, each gate's
        /// key must match a witnessed record of its enrolment in that Authority's chain.
        #[arg(long)]
        keys: PathBuf,
        /// The export, as the Ledger wrote it.
        #[arg(long)]
        export: PathBuf,
        /// The witness directory of `checkpoint-<number>.json` files.
        #[arg(long)]
        witness: PathBuf,
        /// How often the Ledger publishes a checkpoint, in seconds.
        #[arg(long, default_value_t = 60)]
        interval: u32,
        /// Now, in seconds since the epoch. Defaults to this machine's clock, so that a Ledger
        /// that has stopped publishing is always reported.
        #[arg(long)]
        now: Option<i64>,
        /// How far the gates' clock and the Ledger's may differ, in seconds.
        #[arg(long, default_value_t = 300)]
        clock_skew: u32,
        /// The retention the Ledger is meant to keep, in days: report receipts pruned sooner.
        #[arg(long)]
        retention_days: Option<u64>,
        /// With --now: report a gate whose chain has not moved for this many seconds.
        #[arg(long)]
        gate_quiet: Option<u32>,
    },
    /// Check and sign the policy bundle.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Build a connector map from a server's tool listing and its review, and see what a
    /// server has changed since.
    Map {
        #[command(subcommand)]
        command: MapCommand,
    },
}

#[derive(Subcommand)]
enum MapCommand {
    /// Join a server's tool listing and the review of it into a connector map. Refuses
    /// unless every listed tool is reviewed or set aside by name with a reason.
    Build {
        /// The server's answer to `tools/list`, as it gave it.
        #[arg(long)]
        listing: PathBuf,
        /// The review: what each tool is, and which are left out and why.
        #[arg(long)]
        review: PathBuf,
        /// Where the map is written.
        #[arg(long)]
        out: PathBuf,
    },
    /// Report tools a server has added, removed or changed since a map was reviewed. Exits
    /// with failure when anything has.
    Drift {
        /// The server's answer to `tools/list` now.
        #[arg(long)]
        listing: PathBuf,
        #[arg(long)]
        map: PathBuf,
        /// The review the map was built from, so tools it set aside are not reported as new.
        #[arg(long)]
        review: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Build the bundle from a policy directory and run every check a gate would, and each
    /// connector map's sample calls. Needs no key.
    Check {
        /// The policy directory, as checked out from the reviewed repository.
        #[arg(long)]
        dir: PathBuf,
        /// The time the bundle would carry, in seconds since the epoch. Defaults to now.
        #[arg(long)]
        issued_at: Option<i64>,
    },
    /// Check a policy directory, show what changed since the bundle before, and sign it
    /// with the root key. The directory must come from the reviewed repository, never from
    /// the Authority's machine.
    Sign {
        /// The policy directory, as checked out from the reviewed repository.
        #[arg(long)]
        dir: PathBuf,
        /// The root key's file. It signs bundles and nothing else.
        #[arg(long)]
        root_key: PathBuf,
        /// The bundle's number: 1 for the first, then one more each time.
        #[arg(long)]
        number: u64,
        /// The signed bundle before this one. Required for every number but 1.
        #[arg(long)]
        previous: Option<PathBuf>,
        /// The time the bundle carries, in seconds since the epoch. Given, not read from
        /// this machine's clock, so that a second person who builds the bundle from the same
        /// commit with the same number and time gets the same digest (for example the
        /// commit's own time).
        #[arg(long)]
        issued_at: i64,
        /// Where the signed bundle is written. It must not exist yet.
        #[arg(long)]
        out: PathBuf,
        /// Sign without asking. The changes are still printed. Only for a release step in
        /// which a person has already been shown these changes elsewhere and agreed to them.
        #[arg(long)]
        yes: bool,
    },
}

fn describe(problem: &Problem) -> String {
    match problem {
        Problem::UnknownGate => "no trusted key for this gate".into(),
        Problem::BadReceipt(code) => format!("a receipt does not verify ({code})"),
        Problem::BrokenChain => {
            "the chain is broken here: a receipt was removed, reordered, repeated or replaced"
                .into()
        }
        Problem::BadPruneRecord(code) => format!("the prune record does not verify ({code})"),
        Problem::BadCheckpoint(code) => format!("a checkpoint does not verify ({code})"),
        Problem::CheckpointMissing => "a checkpoint is missing from the witness".into(),
        Problem::ReceiptMissing => "a checkpoint covers a receipt the export does not hold".into(),
        Problem::CheckpointMismatch => {
            "a checkpoint names a different hash for this receipt".into()
        }
        Problem::HeadWentBack => {
            "a later checkpoint shows a shorter chain than an earlier one".into()
        }
        Problem::LedgerSilent => "no checkpoint for more than twice the interval".into(),
        Problem::GateQuiet => "the gate's chain has not moved for longer than allowed".into(),
        Problem::CheckpointBackdated => {
            "a checkpoint (the Ledger's clock) is dated earlier than a receipt it covers (the gate's clock) by more than --clock-skew: the Ledger back-dated it, or the two clocks disagree".into()
        }
        Problem::NoCheckpoint => {
            "the witness holds no checkpoint: nothing in the export is covered".into()
        }
        Problem::GateMissing => "a gate with trusted keys is not in the export".into(),
        Problem::GateNotWitnessed => {
            "the gate has receipts and the latest checkpoint does not list it".into()
        }
        Problem::ReceiptNotWitnessed => {
            "a receipt older than the latest checkpoint is covered by none".into()
        }
        Problem::PruneNotWitnessed => {
            "the prune record reaches past the witness, or ties to nothing".into()
        }
        Problem::PrunedTooSoon => "receipts were pruned sooner than the retention allows".into(),
        Problem::PruneUnchecked => {
            "the Ledger says it deleted receipts; pass --retention-days so that can be checked".into()
        }
        Problem::GateNeverSeen => {
            "a trusted gate has no receipt and no checkpoint entry: it never ran, or its history was erased before any checkpoint (if it never ran, leave it out of --keys)".into()
        }
        Problem::BadExport(code) => format!("the export cannot be read as one ({code})"),
        Problem::KeyNotEnrolled => {
            "a trusted key of this gate matches no witnessed record of its enrolment".into()
        }
        Problem::MandateNotIssued => {
            "a proven receipt names a mandate no witnessed record says was issued".into()
        }
        Problem::ReceiptAfterRetirement => {
            "a receipt is dated after its gate was disabled or its key revoked".into()
        }
    }
}

fn print(report: &Report) {
    println!(
        "{} receipts and {} checkpoints verified",
        report.receipts_verified, report.checkpoints_verified
    );
    for g in &report.gates {
        match g.verified {
            Some((first, last)) => println!(
                "gate {}: receipts {first} to {last} verified, {} not yet witnessed",
                g.gate, g.unwitnessed
            ),
            None => println!("gate {}: no receipt verified", g.gate),
        }
        if g.pruned_through > 0 {
            println!(
                "gate {}: the Ledger says it deleted receipts 1 to {}",
                g.gate, g.pruned_through
            );
        }
    }
    for f in &report.findings {
        let gate = f
            .gate
            .as_ref()
            .map(|g| format!(" gate {g}"))
            .unwrap_or_default();
        let seq = f.seq.map(|s| format!(" number {s}")).unwrap_or_default();
        println!("PROBLEM{gate}{seq}: {}", describe(&f.problem));
    }
    let pruned = report.gates.iter().any(|g| g.pruned_through > 0);
    if report.ok() && pruned {
        println!("OK: nothing altered or missing was found, and what the Ledger deleted (above) is older than the retention given. This does not show that a gate recorded everything it did.");
    } else if report.ok() {
        println!("OK: nothing altered, removed or missing was found. This does not show that a gate recorded everything it did.");
    } else {
        println!("FAILED: {} problem(s)", report.findings.len());
    }
}

fn map(command: MapCommand) -> anyhow::Result<bool> {
    use gov_run::maps;
    match command {
        MapCommand::Build {
            listing,
            review,
            out,
        } => {
            let listing = maps::read_listing(&listing)?;
            let review = maps::read_review(&review)?;
            let built = maps::build(&listing, &review)?;
            std::fs::write(&out, serde_json::to_vec_pretty(&built)?)
                .with_context(|| format!("{} cannot be written", out.display()))?;
            println!(
                "{}: {} tools mapped, {} set aside, of {} the server lists.",
                built.server,
                built.tools.len(),
                review.unmapped.len(),
                listing.len()
            );
            Ok(true)
        }
        MapCommand::Drift {
            listing,
            map,
            review,
        } => {
            let listing = maps::read_listing(&listing)?;
            let map = maps::read_map(&map)?;
            let set_aside = match review {
                Some(path) => maps::read_review(&path)?.unmapped.into_keys().collect(),
                None => Default::default(),
            };
            let drift = maps::drift(&listing, &map, &set_aside)?;
            for (what, tools) in [
                ("added", &drift.added),
                ("removed", &drift.removed),
                ("changed", &drift.changed),
            ] {
                for tool in tools {
                    println!("{what}: {tool}");
                }
            }
            if drift.none() {
                println!("No drift: {} tools as reviewed.", drift.unchanged);
            } else {
                println!(
                    "Drift: {} added, {} removed, {} changed, {} as reviewed. A changed or new tool is recorded as unknown until the map is reviewed again.",
                    drift.added.len(),
                    drift.removed.len(),
                    drift.changed.len(),
                    drift.unchanged
                );
            }
            Ok(drift.none())
        }
    }
}

fn policy(command: PolicyCommand) -> anyhow::Result<bool> {
    use gov_run::policy_dir;
    match command {
        PolicyCommand::Check { dir, issued_at } => {
            let checked = policy_dir::check(&dir, 1, issued_at.unwrap_or_else(gov_run::now))?;
            println!(
                "The policy passes its checks: {} rules, {} connector maps, {} sample calls.",
                checked.bundle.rules.len(),
                checked.bundle.connectors.len(),
                checked.samples
            );
            Ok(true)
        }
        PolicyCommand::Sign {
            dir,
            root_key,
            number,
            previous,
            issued_at,
            out,
            yes,
        } => {
            anyhow::ensure!(issued_at > 0, "--issued-at is seconds since the epoch");
            anyhow::ensure!(
                !out.exists(),
                "{} exists already; a signed bundle is never written over",
                out.display()
            );
            let root = gov_run::read_key(&root_key)?;
            let previous: Option<gov_core::Envelope> = previous
                .map(|path| {
                    let bytes = std::fs::read(&path).context("reading --previous")?;
                    serde_json::from_slice(&bytes).context("--previous is not a signed bundle")
                })
                .transpose()?;
            let prepared =
                policy_dir::prepare(&dir, &root.public(), number, previous.as_ref(), issued_at)?;
            println!(
                "Bundle {number} for {}/{}, checked ({} sample calls). Since the bundle before:",
                prepared.bundle.tenant, prepared.bundle.env, prepared.samples
            );
            for line in policy_dir::describe(&prepared.changes) {
                println!("  {line}");
            }
            if !yes {
                println!("Type yes to sign this with the root key:");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != "yes" {
                    println!("Not signed.");
                    return Ok(false);
                }
            }
            let (envelope, opened) = policy_dir::sign(&prepared, &root)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&out)
                .with_context(|| format!("{} cannot be created", out.display()))?;
            std::io::Write::write_all(&mut file, &serde_json::to_vec(&envelope)?)?;
            file.sync_all()?;
            println!(
                "Signed. Digest: {}\nCompare it with the digest the Authority reports when this is published.",
                opened.digest.as_str()
            );
            Ok(true)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<bool> {
    let Command::Verify {
        tenant,
        env,
        keys,
        export,
        witness,
        interval,
        now,
        retention_days,
        clock_skew,
        gate_quiet,
    } = cli.command
    else {
        return match cli.command {
            Command::Policy { command } => policy(command),
            Command::Map { command } => map(command),
            Command::Verify { .. } => unreachable!("matched above"),
        };
    };
    let now = now.or_else(|| {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        i64::try_from(since.as_secs()).ok()
    });
    let scope = Scope::new(&tenant, &env).map_err(|e| anyhow::anyhow!("--tenant or --env: {e}"))?;
    let keys: TrustedKeys =
        serde_json::from_slice(&std::fs::read(&keys).context("reading --keys")?)
            .context("--keys is not a trusted-keys file")?;
    let export: Export =
        serde_json::from_slice(&std::fs::read(&export).context("reading --export")?)
            .context("--export is not an export")?;
    if !witness.is_dir() {
        anyhow::bail!("--witness is not a directory");
    }
    let checkpoints = DirWitness::new(&witness).all()?;
    let report = verify(&Check {
        scope: &scope,
        keys: &keys,
        export: &export,
        checkpoints: &checkpoints,
        interval_secs: interval,
        now,
        gate_quiet_secs: gate_quiet,
        clock_skew_secs: clock_skew,
        retention_secs: retention_days.map(|d| d.saturating_mul(86_400)),
    });
    print(&report);
    Ok(report.ok())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("gov: {e:#}");
            ExitCode::from(2)
        }
    }
}
