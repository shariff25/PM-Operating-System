//! `gov-shim`: started by an agent's runtime, beside the agent (spec §5.3.1). The agent's MCP
//! client is pointed at it; it forwards to the gate and proves, on every tool call, that the
//! agent's key is held here.
//!
//! It listens on this machine only, always. Its secret is written at start to a file only
//! its owner can read; the runtime reads it and sends it as `x-gov-shim-key`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use gov_core::{Envelope, Id, Scope};
use gov_run::shim::{Follow, Shim, ShimConfig};
use gov_run::{bind, listen_address, new_secret, outbound_address, read_key};

#[derive(Parser)]
#[command(
    name = "gov-shim",
    about = "Agent Governance: the shim beside an agent"
)]
struct Cli {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    env: String,
    /// The gate's name, as enrolled at the Ledger.
    #[arg(long)]
    gate: String,
    /// The gate's address.
    #[arg(long)]
    gate_url: String,
    /// An address on this machine. The shim never listens beyond it.
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: String,
    /// The agent's private key.
    #[arg(long)]
    key_file: PathBuf,
    /// The agent's mandate, as issued.
    #[arg(long)]
    mandate: PathBuf,
    /// The policy: the same bundle the gate runs on. A shim on another bundle fails its
    /// proofs for every tool whose map differs.
    #[command(flatten)]
    policy: gov_run::policy_source::PolicyArgs,
    /// Where the shim's secret is written, in a new file only its owner can read.
    #[arg(long)]
    secret_file: PathBuf,
    /// Seconds the gate has to answer.
    #[arg(long, default_value_t = 60)]
    call_timeout: u64,
    /// Send plain HTTP to a gate on another machine, although nothing here speaks TLS.
    #[arg(long)]
    plain_http_on_network: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // No flag widens this: a port that signs is never offered to another machine.
    let addr = listen_address(&cli.listen, false)?;
    outbound_address("the gate", &cli.gate_url, cli.plain_http_on_network)?;
    let scope = Scope::new(&cli.tenant, &cli.env)
        .map_err(|_| anyhow::anyhow!("the tenant or the environment is not an identifier"))?;
    let gate =
        Id::new(&cli.gate).map_err(|_| anyhow::anyhow!("the gate's name is not an identifier"))?;
    let key = read_key(&cli.key_file)?;
    let mandate: Envelope = serde_json::from_slice(&std::fs::read(&cli.mandate)?)
        .context("the mandate cannot be read")?;
    let policy = cli.policy.load(&scope)?;
    let map = policy.map;
    // On the signed path the shim follows its gate to a later bundle, by the same rules
    // it started by.
    let follow = policy.signed.map(|signed| Follow {
        root: signed.root,
        held: signed.held,
        state: Some(signed.state),
        file: Some(signed.file),
        server: signed.server,
    });
    // The secret is in its file before the shim says it is listening: whoever reads that
    // line can read the secret at once.
    let secret = new_secret(&cli.secret_file)?;
    let socket = bind(addr).await?;
    let shim = Shim::new(
        ShimConfig {
            follow,
            scope,
            gate,
            gate_url: cli.gate_url,
            key,
            mandate,
            map,
            secret,
            call_timeout: Duration::from_secs(cli.call_timeout),
        },
        socket.local_addr()?,
    )?;
    axum::serve(socket, shim.router()).await?;
    Ok(())
}
