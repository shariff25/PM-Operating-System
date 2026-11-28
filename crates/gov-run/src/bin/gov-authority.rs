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
    /// Name the installation's first admin and the account they sign in with. Possible
    /// once, on this machine; the record says nobody was signed in.
    FirstAdmin {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        person: String,
        /// The directory (tenant) id people sign in from, and the person's object id in it.
        #[arg(long)]
        entra_tenant_id: String,
        #[arg(long)]
        oid: String,
    },
    /// Replace the key admin records are signed with. Run with the program stopped; run
    /// again after a failure and it goes on from where it was.
    ReplaceAdminKey(Box<Replace>),
}

/// What every command needs to be this Authority.
#[derive(clap::Args)]
struct Which {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    env: String,
    #[arg(long)]
    name: String,
    #[arg(long)]
    admin_key_file: PathBuf,
    #[arg(long)]
    root_key: PathBuf,
}

#[derive(clap::Args)]
struct Replace {
    #[command(flatten)]
    which: Which,
    /// The new key's file, made with `keygen`. `--admin-key-file` is the old one.
    #[arg(long)]
    new_key_file: PathBuf,
    /// The admin who does this.
    #[arg(long = "as")]
    person: String,
    #[arg(long)]
    ledger: String,
    #[arg(long)]
    ledger_keys: PathBuf,
    #[arg(long)]
    plain_http_on_network: bool,
    /// For tests, with fake data: stop after this step (`record` or `shipped`).
    #[arg(long, hide = true)]
    stop_after: Option<String>,
}

/// The listener for people (spec §7.6.4). All of these or none.
#[derive(clap::Args)]
struct People {
    /// A loopback address: a proxy that speaks TLS stands in front of it.
    #[arg(long, requires_all = ["admin_origin", "entra_tenant_id", "entra_client_id", "entra_client_secret_file"])]
    admin_listen: Option<String>,
    /// This listener's public origin, as people's browsers see it.
    #[arg(long)]
    admin_origin: Option<String>,
    #[arg(long)]
    entra_tenant_id: Option<String>,
    #[arg(long)]
    entra_client_id: Option<String>,
    /// Microsoft's, unless a test names another.
    #[arg(long)]
    entra_authority: Option<String>,
    #[arg(long)]
    entra_client_secret_file: Option<PathBuf>,
    /// The header the proxy puts each caller's address in.
    #[arg(long)]
    client_address_header: Option<String>,
    #[arg(long, default_value_t = 50)]
    sign_ins_per_second: u32,
    #[arg(long, default_value_t = 5)]
    sign_ins_per_client_per_second: u32,
}

#[derive(clap::Args)]
struct Serve {
    #[command(flatten)]
    people: People,
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
    // Held for as long as this program runs: the command that replaces the admin-record
    // key refuses to run meanwhile, and a second instance does not start.
    let _serving = store
        .serving_lock(&scope)
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database cannot be reached"))?
        .context(
            "another instance of this Authority, or the command that replaces its key, is running",
        )?;
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
    let authority = Arc::new(authority);
    let people = match &s.people.admin_listen {
        None => None,
        Some(listen) => {
            let at: SocketAddr = listen
                .parse()
                .ok()
                .filter(|a: &SocketAddr| a.ip().is_loopback())
                .context("the admin listener is on a literal loopback address, behind a proxy that speaks TLS")?;
            let need = |v: &Option<String>| v.clone().unwrap_or_default();
            let tenant = gov_signin::Tenant::from_settings(
                [
                    "--entra-tenant-id",
                    "--entra-client-id",
                    "--entra-authority",
                ],
                &need(&s.people.entra_tenant_id),
                &need(&s.people.entra_client_id),
                s.people.entra_authority.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            let secret = s
                .people
                .entra_client_secret_file
                .as_deref()
                .context("the client secret's file is not named")?;
            let admin = gov_authority::admin_http::Admin::new(
                authority.clone(),
                gov_signin::Verifier::new(tenant).map_err(|e| anyhow::anyhow!("{e}"))?,
                gov_authority::admin_http::AdminConfig {
                    origin: need(&s.people.admin_origin),
                    client_secret: gov_run::read_secret(secret)?,
                    client_header: s.people.client_address_header.clone(),
                    sign_ins_per_second: s.people.sign_ins_per_second,
                    sign_ins_per_client_per_second: s.people.sign_ins_per_client_per_second,
                    fake_data: std::env::var("GOV_DATA_CLASS").as_deref() == Ok("fake"),
                },
                now,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            Some((at, Arc::new(admin)))
        }
    };
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
        let mut cover = gov_run::Cover::default();
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
                    Ok(r) if r.status().is_success() => gov_run::body_up_to(r, 64 * 1024)
                        .await
                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
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
            let witnessed = witnessed_head(witness, &ledger_keys, &scope, &name);
            if cover.overdue(acked, witnessed, now(), cover_within) {
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
    if let Some((at, admin)) = people {
        let for_people = bind_named("admin", at).await?;
        tokio::spawn(async move {
            let _ = axum::serve(for_people, gov_authority::admin_http::router(admin)).await;
        });
    }
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

async fn store() -> anyhow::Result<PgStore> {
    let url = std::env::var("GOV_AUTHORITY_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("GOV_AUTHORITY_DATABASE_URL is not set"))?;
    let store = PgStore::connect(&url)
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database cannot be reached"))?;
    store
        .migrate()
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database could not be prepared"))?;
    Ok(store)
}

fn names(which: &Which) -> anyhow::Result<(Scope, Id)> {
    Ok((
        Scope::new(&which.tenant, &which.env)
            .map_err(|_| anyhow::anyhow!("the tenant or the environment is not an identifier"))?,
        Id::new(&which.name)
            .map_err(|_| anyhow::anyhow!("the Authority's name is not an identifier"))?,
    ))
}

fn person(text: &str) -> anyhow::Result<Id> {
    Id::new(text).map_err(|_| anyhow::anyhow!("the person's name is not an identifier"))
}

async fn first_admin(
    which: Which,
    who: String,
    tenant_id: String,
    oid: String,
) -> anyhow::Result<()> {
    let (scope, name) = names(&which)?;
    let who = person(&who)?;
    let tid = gov_signin::guid_setting("--entra-tenant-id", &tenant_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let oid = oid.trim().to_ascii_lowercase();
    anyhow::ensure!(gov_signin::is_guid(&oid), "the object id is a GUID");
    let authority = Authority::new(
        store().await?,
        scope,
        name,
        read_key(&which.admin_key_file)?,
        public_key(&which.root_key, "the root key")?,
    );
    let at = now();
    authority
        .bootstrap(&who, at)
        .await
        .map_err(|e| anyhow::anyhow!("no first admin was named: {e}"))?;
    authority
        .link_person(&who, &who, &gov_authority::Account { tid, oid }, at)
        .await
        .map_err(|e| anyhow::anyhow!("the first admin was named and not linked: {e}"))?;
    println!("{who} is the first admin, linked to the account given");
    Ok(())
}

/// Replace the admin-record key in three steps, each safe to repeat (spec §7.6.4, point
/// 9): the record; the record at the Ledger and the new key taken there; the old file gone.
async fn replace_admin_key(r: Replace) -> anyhow::Result<()> {
    use gov_core::enrol::key_digest;
    outbound_address("the Ledger", &r.ledger, r.plain_http_on_network)?;
    let (scope, name) = names(&r.which)?;
    let who = person(&r.person)?;
    let root = public_key(&r.which.root_key, "the root key")?;
    let ledger_keys: Vec<PublicKey> = serde_json::from_slice(&std::fs::read(&r.ledger_keys)?)
        .context("the Ledger's public keys cannot be read")?;
    let ledger_keys = KeySet::new(ledger_keys);
    let next = read_key(&r.new_key_file)?;
    let next_public = next.public();
    let stop = |step: &str| r.stop_after.as_deref() == Some(step);
    anyhow::ensure!(
        r.stop_after.is_none() || std::env::var("GOV_DATA_CLASS").as_deref() == Ok("fake"),
        "--stop-after is for tests with fake data"
    );
    let store = store().await?;
    let lock = store
        .serving_lock(&scope)
        .await
        .map_err(|_| anyhow::anyhow!("the Authority's database cannot be reached"))?
        .context("the Authority's program is running: stop it first")?;

    use gov_authority::Store;
    let named = store
        .admin_key(&scope)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let recorded = named.is_some_and(|n| key_digest(&next_public).is_ok_and(|d| d == n.digest));
    let old = r
        .which
        .admin_key_file
        .exists()
        .then(|| read_key(&r.which.admin_key_file));
    let old = old.transpose()?;
    if !recorded {
        let old = old
            .as_ref()
            .context("the old key's file is gone and no record names the new key")?;
        let mut authority = Authority::new(
            store.clone(),
            scope.clone(),
            name.clone(),
            read_key(&r.which.admin_key_file)?,
            root.clone(),
        );
        authority
            .replace_admin_key(&who, read_key(&r.new_key_file)?, now())
            .await
            .map_err(|e| anyhow::anyhow!("the key was not replaced: {e}"))?;
        // The old key's public half is kept beside it: the program needs it to read the
        // chain, and an auditor to check it.
        let kept = r.which.admin_key_file.with_extension("pub.json");
        std::fs::write(&kept, serde_json::to_vec(&old.public())?)?;
        println!("recorded: the chain names the new key");
    }
    if stop("record") {
        return Ok(());
    }

    // The Ledger learns the new key from the record, so the shipment that carries it is
    // asked for under the old key. A Ledger that already took the record answers the old
    // key nothing: then only the new key is left to offer.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let base = r.ledger.trim_end_matches('/').to_string();
    if let Some(old) = old {
        let authority = Authority::new(store.clone(), scope, name, next, root)
            .with_replaced_key(old)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut acked = 0;
        while let Some(shipment) = authority
            .shipment(acked, 500)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
        {
            if shipment.request.kid == next_public.kid {
                break;
            }
            let sent = http
                .post(format!("{base}/v1/receipts"))
                .json(&json!({"request": shipment.request, "receipts": shipment.batch}))
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("the Ledger cannot be reached; run this again"))?;
            if sent.status().as_u16() == 401 {
                break;
            }
            anyhow::ensure!(
                sent.status().is_success(),
                "the Ledger answered {}; run this again",
                sent.status().as_u16()
            );
            let ack = gov_run::body_up_to(sent, 64 * 1024)
                .await
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| serde_json::from_value::<Envelope>(v["ack"].clone()).ok())
                .context("the Ledger's answer cannot be read; run this again")?;
            let to = authority
                .check_answer(&shipment, &ack, &ledger_keys)
                .await
                .map_err(|e| anyhow::anyhow!("the Ledger's answer does not hold: {e}"))?;
            anyhow::ensure!(to > acked, "the Ledger took nothing; run this again");
            acked = to;
        }
    }
    if stop("shipped") {
        return Ok(());
    }
    let taken = http
        .post(format!("{base}/v1/keys"))
        .json(&json!({"key": next_public}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("the Ledger cannot be reached; run this again"))?;
    anyhow::ensure!(
        taken.status().is_success(),
        "the Ledger did not take the new key ({}); run this again",
        taken.status().as_u16()
    );
    println!("the Ledger holds the new key");

    if r.which.admin_key_file.exists() {
        let aside = r.which.admin_key_file.with_extension("replaced");
        std::fs::rename(&r.which.admin_key_file, &aside)?;
        std::fs::remove_file(&aside)?;
    }
    lock.release().await;
    println!(
        "done: start the program with --admin-key-file {} and --earlier-admin-key {}",
        r.new_key_file.display(),
        r.which.admin_key_file.with_extension("pub.json").display()
    );
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
        Command::FirstAdmin {
            which,
            person,
            entra_tenant_id,
            oid,
        } => first_admin(which, person, entra_tenant_id, oid).await,
        Command::ReplaceAdminKey(r) => replace_admin_key(*r).await,
    }
}
