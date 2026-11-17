use std::sync::Arc;

use anyhow::{Context, Result};
use pmos_bots::catalog::{Catalog, Reloader, RELOAD_EVERY};
use pmos_bots::clients::{Gateway, ToolServer};
use pmos_bots::config::Config;
use pmos_bots::pg::PgStore;
use pmos_bots::store::{BotsStore, MemoryStore};
use pmos_bots::{router_with, Runtime};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env().map_err(anyhow::Error::msg)?;

    // A content tree with lint errors stops the start: no task should run against a bot
    // definition CI would reject. After that, a content change is picked up without a
    // restart; one that fails the lint keeps the catalog already loaded.
    let reloader = Reloader::new(&config.content_root);
    let catalog = Catalog::load(&config.content_root, chrono::Utc::now().date_naive())
        .map_err(anyhow::Error::msg)?;

    let store: Arc<dyn BotsStore> = match &config.database_url {
        Some(url) => {
            let pg = PgStore::connect(&url.0)
                .await
                .context("connecting to Postgres")?;
            pg.migrate().await.context("applying migrations")?;
            Arc::new(pg)
        }
        None => {
            tracing::warn!(
                "PMOS_DEV=1 and no PMOS_BOTS_DATABASE_URL: tasks and routines are in memory and go on restart"
            );
            Arc::new(MemoryStore::default())
        }
    };

    let stop = CancellationToken::new();
    let rt = Arc::new(Runtime::new(
        store,
        Arc::new(catalog),
        ToolServer::new(&config.toolserver_url, &config.service_token.0),
        Gateway::new(&config.gateway_url, &config.gateway_token.0),
        stop.clone(),
    ));
    // Their first pass runs now: tasks interrupted by the last shutdown are recovered and a
    // routine missed while down runs once.
    let mut loops = rt.spawn_loops();
    loops.push(reloader.spawn(rt.clone(), RELOAD_EVERY));

    tracing::info!(
        mode = config.assertions.mode().as_str(),
        keys = ?config.assertions.key_ids(),
        "user assertions from web and console"
    );
    if let Some(warning) = config.assertions.insecure_warning() {
        tracing::error!("{warning}");
    }
    let app = router_with(
        rt,
        config.clients.clone(),
        config.evals_as.clone(),
        config.assertions.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(bind = %config.bind, "pmos-bots listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            pmos_runtime::shutdown_signal().await;
            tracing::info!("shutting down: ending long-polls and finishing in-flight requests");
            stop.cancel();
        })
        .await?;
    for l in loops {
        let _ = l.await;
    }
    Ok(())
}
