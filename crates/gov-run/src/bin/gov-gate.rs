//! `gov-gate`: the MCP gate as a program, in observe mode. An agent's MCP client is pointed
//! at it instead of at the server; every tool call leaves a receipt, and nothing is blocked.
//! It ships its receipts to the Ledger on a timer and trims its spool to the witness.
//!
//! The connector map and the mandate issuers' keys come from a policy bundle signed by the
//! root key (spec §7.6.1); the unsigned files of the first steps work only under a flag
//! for tests. The Ledger's public keys stay a local file: who witnesses is not for whoever
//! signs policy to choose (review finding GR-134).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use clap::{Parser, Subcommand};
use gov_core::{Id, KeySet, PublicKey, Scope};
use gov_gate::forward::{Forwarder, ForwarderConfig};
use gov_gate::link::HttpLedgerLink;
use gov_gate::sessions::SessionsConfig;
use gov_gate::{DirWitnessReader, FileSpool, Gate, GateConfig, Shipped, WitnessReader};
use gov_run::{
    bind, bind_named, carries_secret, keygen, listen_address, new_secret, now, outbound_address,
    read_key,
};
use serde_json::json;

#[derive(Parser)]
#[command(name = "gov-gate", about = "Agent Governance: the gate (observe mode)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Make the gate's signing key. Writes the private key to a new file only its owner can
    /// read, and prints the public key to enrol at the Ledger.
    Keygen {
        #[arg(long)]
        kid: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Stand between agents and one MCP server.
    Serve(Box<Serve>),
}

#[derive(clap::Args)]
struct Serve {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    env: String,
    /// This gate's name, as enrolled at the Ledger.
    #[arg(long)]
    gate: String,
    #[arg(long, default_value = "127.0.0.1:8791")]
    listen: String,
    /// Where an operator reads the gate's mode and counters. Never the address agents use:
    /// an agent that could read them would learn when its calls go unrecorded.
    #[arg(long, default_value = "127.0.0.1:8792")]
    operator_listen: String,
    #[arg(long)]
    key_file: PathBuf,
    /// The spool directory: receipts wait here until a witnessed checkpoint covers them.
    #[arg(long)]
    spool: PathBuf,
    /// The one MCP server this gate stands in front of.
    #[arg(long)]
    upstream: String,
    #[command(flatten)]
    policy: gov_run::policy_source::PolicyArgs,
    /// The Ledger's address.
    #[arg(long)]
    ledger: String,
    /// A file holding the Ledger's public keys, as a JSON list.
    #[arg(long)]
    ledger_keys: PathBuf,
    /// The witness directory, to trim the spool to. Without it nothing is ever trimmed.
    #[arg(long)]
    witness: Option<PathBuf>,
    /// Seconds between shipments.
    #[arg(long, default_value_t = 5)]
    ship_interval: u64,
    /// The most receipts held that no witnessed checkpoint covers.
    #[arg(long, default_value_t = 100_000)]
    spool_limit: usize,
    #[arg(long, default_value_t = 500)]
    batch: usize,
    /// Seconds the server, or the Ledger, has to answer.
    #[arg(long, default_value_t = 60)]
    call_timeout: u64,
    /// Where the secret for the operator address is written, in a new file only its owner
    /// can read. Every request to the operator address must carry it.
    #[arg(long)]
    operator_secret_file: PathBuf,
    /// Listen beyond this machine, and send plain HTTP beyond it, although nothing here
    /// speaks TLS.
    #[arg(long)]
    plain_http_on_network: bool,
}

async fn serve(s: Serve) -> anyhow::Result<()> {
    let addr = listen_address(&s.listen, s.plain_http_on_network)?;
    let operator_addr = listen_address(&s.operator_listen, s.plain_http_on_network)?;
    outbound_address("the upstream server", &s.upstream, s.plain_http_on_network)?;
    outbound_address("the Ledger", &s.ledger, s.plain_http_on_network)?;
    anyhow::ensure!(s.ship_interval > 0, "the ship interval is zero");
    anyhow::ensure!(
        s.batch > 0 && s.batch <= 1000,
        "a batch is 1 to 1000 receipts"
    );
    let scope = Scope::new(&s.tenant, &s.env)
        .map_err(|_| anyhow::anyhow!("the tenant or the environment is not an identifier"))?;
    let gate =
        Id::new(&s.gate).map_err(|_| anyhow::anyhow!("the gate's name is not an identifier"))?;
    let key = read_key(&s.key_file)?;
    let policy = s.policy.load(&scope)?;
    let ledger_keys: Vec<PublicKey> = serde_json::from_slice(&std::fs::read(&s.ledger_keys)?)
        .context("the Ledger's public keys cannot be read")?;
    anyhow::ensure!(!ledger_keys.is_empty(), "no public key for the Ledger");
    // With issuers' keys the gate opens sessions for the shim and records a proven agent
    // as proven. A mandate is held to the template it names in the bundle this gate holds,
    // so the key mandates are signed with is worth no more than the templates allow.
    let sessions = (!policy.mandate_issuers.is_empty()).then(|| {
        let mut config = SessionsConfig::new(
            scope.clone(),
            gate.clone(),
            KeySet::new(policy.mandate_issuers.clone()),
        );
        config.templates = policy.templates.clone();
        config
    });
    let operator_secret = new_secret(&s.operator_secret_file)?;
    let config = GateConfig {
        scope,
        gate,
        ledger_keys: KeySet::new(ledger_keys),
        spool_limit: s.spool_limit,
        batch: s.batch,
        policy_version: Some(policy.version.clone()),
    };
    let spool = FileSpool::open(&s.spool).map_err(|e| anyhow::anyhow!("the spool: {e}"))?;
    let gate = Gate::open(config, key, spool, now()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let forwarder = Forwarder::new(
        gate,
        ForwarderConfig {
            authority: None,
            upstream: s.upstream,
            map: policy.map,
            call_timeout: Duration::from_secs(s.call_timeout),
            sessions,
            max_in_flight: gov_gate::forward::MAX_IN_FLIGHT,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let link = HttpLedgerLink::new(&s.ledger).map_err(|e| anyhow::anyhow!("{e}"))?;
    let socket = bind(addr).await?;

    // Ship, then trim, on a timer. What goes wrong is said on standard error, which holds
    // receipt numbers and reasons and never a request, a header or an answer.
    let shipper = forwarder.clone();
    let witness: Option<Arc<dyn WitnessReader>> = s
        .witness
        .map(|dir| Arc::new(DirWitnessReader::new(dir)) as Arc<dyn WitnessReader>);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(s.ship_interval));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            loop {
                match shipper.ship(&link).await {
                    Ok(Shipped::Advanced(_)) => continue,
                    Ok(Shipped::NothingToSend) => break,
                    Ok(other) => {
                        eprintln!("shipping: {other:?}");
                        break;
                    }
                    Err(e) => {
                        eprintln!("shipping: {e}");
                        break;
                    }
                }
            }
            if let Some(witness) = &witness {
                match shipper.trim(witness.clone()).await {
                    Ok(t) if t.bad_checkpoints.is_empty() => {}
                    Ok(t) => eprintln!("checkpoints that do not verify: {:?}", t.bad_checkpoints),
                    Err(e) => eprintln!("trimming: {e}"),
                }
            }
        }
    });

    // What an operator must be able to see: observe mode protects nothing, and what it did
    // not record must not be silent.
    let counted = forwarder.clone();
    let health = get(move |headers: axum::http::HeaderMap| {
        let c = counted.clone();
        let secret = operator_secret.clone();
        async move {
            // An agent on this machine can reach this address too; without the operator's
            // secret it learns nothing (review finding GR-98).
            if !carries_secret(&headers, "x-gov-operator-key", &secret) {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            Json(json!({
                "mode": "observe",
                "enforced": false,
                "recorded": c.counters.recorded.load(Ordering::Relaxed),
                "not_recorded": c.counters.not_recorded.load(Ordering::Relaxed),
                "unmapped": c.counters.unmapped.load(Ordering::Relaxed),
                "unmapped_by_tool": c.counters.unmapped_by_tool.counts(),
                // Requests to open a session that were refused, by why (GR-155).
                "sessions_refused": c.sessions_refused(),
                "ship_no_progress": c.counters.ship_no_progress.load(Ordering::Relaxed),
                "proven": c.counters.proven.load(Ordering::Relaxed),
                "proof_failed": c.counters.proof_failed.load(Ordering::Relaxed),
            }))
            .into_response()
        }
    });
    let operator = bind_named("operator", operator_addr).await?;
    tokio::spawn(async move {
        let _ = axum::serve(operator, axum::Router::new().route("/healthz", health)).await;
    });
    axum::serve(socket, forwarder.router()).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen { kid, out } => {
            println!("{}", serde_json::to_string(&keygen(&kid, &out)?)?);
            Ok(())
        }
        Command::Serve(s) => serve(*s).await,
    }
}
