//! `gov-authority`: the Authority as a program, with one listener, for gates only (spec
//! §7.6.3, point 11). It answers a gate's signed request to renew, and ships its own admin
//! records to the Ledger. No admin action and no sign-in is served: those are library
//! calls on this machine until G4e.
//!
//! The database address comes from `GOV_AUTHORITY_DATABASE_URL`, never the command line.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use clap::{Parser, Subcommand};
use gov_authority::http::{router, Listener, SourceLimit};
use gov_authority::pg::PgStore;
use gov_authority::Authority;
use gov_core::lease::Checkpoint;
use gov_core::{open, Envelope, Id, KeySet, PublicKey, Scope};
use gov_ledger::{DirWitness, Witness};
use gov_run::{
    bind, bind_named, carries_secret, keygen, listen_address, new_secret, now, outbound_address,
    read_key,
};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "gov-authority",
    about = "Agent Governance: the Authority (renewals for gates only)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    /// Answer gates' renewals and ship the admin records to the Ledger.
    Serve(Box<Serve>),
}

#[derive(clap::Args)]
struct Serve {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    env: String,
    /// This Authority's name: its chain's, and the issuer of its bundles.
    #[arg(long)]
    name: String,
    #[arg(long, default_value = "127.0.0.1:8793")]
    listen: String,
    #[arg(long, default_value = "127.0.0.1:8794")]
    operator_listen: String,
    #[arg(long)]
    operator_secret_file: PathBuf,
    #[arg(long)]
    admin_key_file: PathBuf,
    /// The public halves of the keys this chain was signed with before, a file each.
    #[arg(long)]
    earlier_admin_key: Vec<PathBuf>,
    #[arg(long)]
    mandates_key_file: PathBuf,
    #[arg(long)]
    leases_key_file: PathBuf,
    /// The root key's public half. The private half is never on this machine.
    #[arg(long)]
    root_key: PathBuf,
    #[arg(long)]
    ledger: String,
    /// A file holding the Ledger's public keys, as a JSON list.
    #[arg(long)]
    ledger_keys: PathBuf,
    /// The witness directory. With it, a head of this chain that no checkpoint covers
    /// within `--cover-within` seconds is counted for the operator.
    #[arg(long)]
    witness: Option<PathBuf>,
    #[arg(long, default_value_t = 180)]
    cover_within: u64,
    #[arg(long, default_value_t = 5)]
    ship_interval: u64,
    #[arg(long, default_value_t = 20)]
    renewals_per_source_per_second: u32,
    #[arg(long)]
    plain_http_on_network: bool,
}

fn public_key(path: &PathBuf, what: &str) -> anyhow::Result<PublicKey> {
    serde_json::from_slice(&std::fs::read(path).with_context(|| format!("{what} cannot be read"))?)
        .with_context(|| format!("{what} is not a public key"))
}

#[derive(Default)]
struct Shipping {
    acked: AtomicU64,
    failures: AtomicU64,
    uncovered: AtomicU64,
}

/// The head a checkpoint in the witness last gave this chain, if any verifies.
fn witnessed_head(witness: &DirWitness, keys: &KeySet, scope: &Scope, chain: &Id) -> u64 {
    witness
        .all()
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|(_, bytes)| {
            let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
            let cp = open::<Checkpoint>(&envelope, keys, scope).ok()?.doc;
            Some(
                cp.heads
                    .iter()
                    .find(|h| &h.gate == chain)
                    .map_or(0, |h| h.seq),
            )
        })
        .unwrap_or(0)
}

async fn serve(s: Serve) -> anyhow::Result<()> {
    let addr = listen_address(&s.listen, s.plain_http_on_network)?;
    let operator_addr = listen_address(&s.operator_listen, s.plain_http_on_network)?;
    outbound_address("the Ledger", &s.ledger, s.plain_http_on_network)?;
    anyhow::ensure!(s.ship_interval > 0, "the ship interval is zero");
    let scope = Scope::new(&s.tenant, &s.env)
        .map_err(|_| anyhow::anyhow!("the tenant or the environment is not an identifier"))?;
    let name = Id::new(&s.name)
        .map_err(|_| anyhow::anyhow!("the Authority's name is not an identifier"))?;
    let earlier = s
        .earlier_admin_key
        .iter()
        .map(|p| public_key(p, "an earlier admin key"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ledger_keys: Vec<PublicKey> = serde_json::from_slice(&std::fs::read(&s.ledger_keys)?)
        .context("the Ledger's public keys cannot be read")?;
    anyhow::ensure!(!ledger_keys.is_empty(), "no public key for the Ledger");
    let ledger_keys = KeySet::new(ledger_keys);
    let url = std::env::var("GOV_AUTHORITY_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("GOV_AUTHORITY_DATABASE_URL is not set"))?;
    // A connection error can quote the address, password and all: it is not printed.
    let store = PgStore::connect(&url)
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database cannot be reached"))?;
    store
        .migrate()
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database could not be prepared"))?;
    let authority = Authority::new(
        store,
        scope.clone(),
        name.clone(),
        read_key(&s.admin_key_file)?,
        public_key(&s.root_key, "the root key")?,
    )
    .with_online_keys(
        read_key(&s.mandates_key_file)?,
        read_key(&s.leases_key_file)?,
    )
    .with_earlier_admin_keys(earlier);
    let operator_secret = new_secret(&s.operator_secret_file)?;
    let listener = Arc::new(Listener::new(
        authority,
        now,
        SourceLimit {
            per_second: s.renewals_per_source_per_second,
            ..SourceLimit::default()
        },
    ));
    let shipping = Arc::new(Shipping::default());

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let receipts = format!("{}/v1/receipts", s.ledger.trim_end_matches('/'));
    let (shipper, shipped) = (listener.clone(), shipping.clone());
    let witness = s.witness.as_deref().map(DirWitness::new);
    let cover_within = i64::try_from(s.cover_within).unwrap_or(i64::MAX);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(s.ship_interval));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first head seen that the witness did not cover, and when.
        let mut waiting: Option<(u64, i64)> = None;
        loop {
            tick.tick().await;
            loop {
                let acked = shipped.acked.load(Ordering::Relaxed);
                let shipment = match shipper.authority.shipment(acked, 500).await {
                    Ok(Some(shipment)) => shipment,
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("shipping: {e}");
                        break;
                    }
                };
                let sent = http
                    .post(&receipts)
                    .json(&json!({"request": shipment.request, "receipts": shipment.batch}))
                    .send()
                    .await;
                let ack = match sent {
                    Ok(r) if r.status().is_success() => r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| serde_json::from_value::<Envelope>(v["ack"].clone()).ok()),
                    Ok(r) => {
                        eprintln!("shipping: the Ledger answered {}", r.status().as_u16());
                        None
                    }
                    Err(_) => {
                        eprintln!("shipping: the Ledger cannot be reached");
                        None
                    }
                };
                let moved = match ack {
                    Some(ack) => shipper
                        .authority
                        .check_answer(&shipment, &ack, &ledger_keys)
                        .await
                        .ok(),
                    None => None,
                };
                match moved {
                    Some(to) if to > acked => shipped.acked.store(to, Ordering::Relaxed),
                    _ => {
                        shipped.failures.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            let Some(witness) = &witness else { continue };
            let acked = shipped.acked.load(Ordering::Relaxed);
            if witnessed_head(witness, &ledger_keys, &scope, &name) >= acked {
                waiting = None;
                continue;
            }
            let (_, since) = *waiting.get_or_insert((acked, now()));
            if now().saturating_sub(since) > cover_within {
                shipped.uncovered.fetch_add(1, Ordering::Relaxed);
                eprintln!("no witnessed checkpoint covers this Authority's chain through {acked}");
            }
        }
    });

    let (counted, shipped) = (listener.clone(), shipping);
    let health = get(move |headers: axum::http::HeaderMap| {
        let (l, s, secret) = (counted.clone(), shipped.clone(), operator_secret.clone());
        async move {
            if !carries_secret(&headers, "x-gov-operator-key", &secret) {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            Json(json!({
                "renewed": l.counters.renewed.load(Ordering::Relaxed),
                "refused": l.counters.refused.load(Ordering::Relaxed),
                "limited": l.counters.limited.load(Ordering::Relaxed),
                "failed": l.counters.failed.load(Ordering::Relaxed),
                "shipped_through": s.acked.load(Ordering::Relaxed),
                "ship_failures": s.failures.load(Ordering::Relaxed),
                "chain_not_witnessed": s.uncovered.load(Ordering::Relaxed),
            }))
            .into_response()
        }
    });
    let socket = bind(addr).await?;
    let operator = bind_named("operator", operator_addr).await?;
    tokio::spawn(async move {
        let _ = axum::serve(operator, axum::Router::new().route("/healthz", health)).await;
    });
    axum::serve(
        socket,
        router(listener).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
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
