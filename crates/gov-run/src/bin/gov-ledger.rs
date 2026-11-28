//! `gov-ledger`: the Ledger as a program. It takes gates' shipments on a listener, publishes
//! a checkpoint to the witness on a timer, and writes the export an auditor checks with
//! `gov verify`.
//!
//! The database address comes from `GOV_LEDGER_DATABASE_URL` and never from the command
//! line, where every account on the machine could read its password.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use gov_core::{Id, PublicKey, Scope};
use gov_ledger::http::{router, Listener};
use gov_ledger::pg::PgStore;
use gov_ledger::{DirWitness, Ledger};
use gov_run::{bind, keygen, listen_address, now, read_key};

#[derive(Parser)]
#[command(name = "gov-ledger", about = "Agent Governance: the Ledger")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Which {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    env: String,
}

#[derive(Subcommand)]
enum Command {
    /// Make a signing key. Writes the private key to a new file only its owner can read, and
    /// prints the public key.
    Keygen {
        #[arg(long)]
        kid: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Enrol a gate's public key, once. Until there is an Authority, this is how the Ledger
    /// learns which gates exist.
    Enrol {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        gate: String,
        /// A file holding the gate's public key, as `keygen` printed it.
        #[arg(long)]
        public_key: PathBuf,
    },
    /// Enrol an Authority's admin-record key, once, at installation. Gates' keys then
    /// follow from that Authority's records.
    EnrolAuthority {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        authority: String,
        #[arg(long)]
        public_key: PathBuf,
    },
    /// Take shipments and publish checkpoints.
    Serve {
        #[command(flatten)]
        which: Which,
        #[arg(long, default_value = "127.0.0.1:8790")]
        listen: String,
        #[arg(long)]
        key_file: PathBuf,
        /// The name the Ledger signs as.
        #[arg(long, default_value = "ledger")]
        issuer: String,
        /// The witness: a directory the Ledger can add to and should not be able to change.
        #[arg(long)]
        witness: PathBuf,
        /// Seconds between checkpoints.
        #[arg(long, default_value_t = 60)]
        checkpoint_interval: u64,
        /// Listen beyond this machine although the listener is plain HTTP.
        #[arg(long)]
        plain_http_on_network: bool,
    },
    /// Write the export and the public keys, for `gov verify`.
    Export {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        keys_out: PathBuf,
    },
}

async fn ledger(which: &Which) -> anyhow::Result<Ledger<PgStore>> {
    let scope = Scope::new(&which.tenant, &which.env)
        .map_err(|_| anyhow::anyhow!("the tenant or the environment is not an identifier"))?;
    let url = std::env::var("GOV_LEDGER_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("GOV_LEDGER_DATABASE_URL is not set"))?;
    // A connection error can quote the address, password and all: it is not printed.
    let store = PgStore::connect(&url)
        .await
        .map_err(|_| anyhow::anyhow!("the Ledger's database cannot be reached"))?;
    store
        .migrate()
        .await
        .map_err(|_| anyhow::anyhow!("the Ledger's database could not be prepared"))?;
    Ok(Ledger::new(store, scope))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen { kid, out } => {
            println!("{}", serde_json::to_string(&keygen(&kid, &out)?)?);
        }
        Command::Enrol {
            which,
            gate,
            public_key,
        } => {
            let gate = Id::new(&gate)
                .map_err(|_| anyhow::anyhow!("the gate's name is not an identifier"))?;
            let key: PublicKey = serde_json::from_slice(&std::fs::read(&public_key)?)
                .context("the file does not hold a public key")?;
            ledger(&which)
                .await?
                .enrol_gate_key(&gate, &key)
                .await
                .map_err(|e| anyhow::anyhow!("the key was not enrolled: {e}"))?;
            println!("enrolled key {} for gate {gate}", key.kid);
        }
        Command::EnrolAuthority {
            which,
            authority,
            public_key,
        } => {
            let authority = Id::new(&authority)
                .map_err(|_| anyhow::anyhow!("the Authority's name is not an identifier"))?;
            let key: PublicKey = serde_json::from_slice(&std::fs::read(&public_key)?)
                .context("the file does not hold a public key")?;
            ledger(&which)
                .await?
                .enrol_admin_key(&authority, &key)
                .await
                .map_err(|e| anyhow::anyhow!("the key was not enrolled: {e}"))?;
            println!("enrolled key {} for Authority {authority}", key.kid);
        }
        Command::Serve {
            which,
            listen,
            key_file,
            issuer,
            witness,
            checkpoint_interval,
            plain_http_on_network,
        } => {
            let addr = listen_address(&listen, plain_http_on_network)?;
            let key = read_key(&key_file)?;
            let issuer =
                Id::new(&issuer).map_err(|_| anyhow::anyhow!("the issuer is not an identifier"))?;
            anyhow::ensure!(checkpoint_interval > 0, "the checkpoint interval is zero");
            std::fs::create_dir_all(&witness)?;
            let witness = DirWitness::new(&witness);
            // Before anything is served: what this Ledger notes of each gate key is written
            // again from the Authorities' chains it holds (review finding GR-131).
            let ledger = ledger(&which).await?;
            ledger
                .rebuild_notes()
                .await
                .map_err(|e| anyhow::anyhow!("the Authorities' chains cannot be read: {e}"))?;
            let listener = Arc::new(Listener {
                health: Default::default(),
                ledger,
                key,
                issuer,
                clock: now,
            });
            let socket = bind(addr).await?;
            let timer = listener.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(checkpoint_interval));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    // A checkpoint that fails is said out loud and tried again: an auditor's
                    // `gov verify` reports a Ledger that has gone silent.
                    let at = now();
                    match timer
                        .ledger
                        .checkpoint(&timer.key, &timer.issuer, at, &witness)
                        .await
                    {
                        Ok(_) => {
                            timer.health.checkpoint_failures.store(0, Ordering::Relaxed);
                            timer.health.last_checkpoint_at.store(at, Ordering::Relaxed);
                        }
                        Err(e) => {
                            timer
                                .health
                                .checkpoint_failures
                                .fetch_add(1, Ordering::Relaxed);
                            eprintln!("checkpoint not published: {e}");
                        }
                    }
                }
            });
            axum::serve(socket, router(listener)).await?;
        }
        Command::Export {
            which,
            key_file,
            out,
            keys_out,
        } => {
            let key = read_key(&key_file)?;
            let ledger = ledger(&which).await?;
            let export = ledger.export().await.map_err(|e| anyhow::anyhow!("{e}"))?;
            let keys = ledger
                .keys(&key)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            std::fs::write(&out, serde_json::to_vec(&export)?)?;
            std::fs::write(&keys_out, serde_json::to_vec(&keys)?)?;
            println!("wrote {} and {}", out.display(), keys_out.display());
            // The keys file comes from the Ledger being audited (review finding GR-84).
            println!(
                "{} lists the keys THIS Ledger says it trusts. It is a convenience, not evidence: an auditor checks an export against the customer's own record of the keys that were enrolled.",
                keys_out.display()
            );
        }
    }
    Ok(())
}
