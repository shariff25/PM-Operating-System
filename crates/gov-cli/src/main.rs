//! `gov`: Agent Governance's command line. For now one command, `gov verify`, which an
//! auditor runs with nothing but files: the public keys, the witness's checkpoints and an
//! export of receipts. It opens no database and no network connection.

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
    } = cli.command;
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
