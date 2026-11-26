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
        /// The trusted public keys: `{ "ledger": [...], "gates": { "<gate>": [...] } }`.
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
        /// Now, in seconds since the epoch, to also report a Ledger that has stopped since.
        #[arg(long)]
        now: Option<i64>,
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
    if report.ok() {
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
        gate_quiet,
    } = cli.command;
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
