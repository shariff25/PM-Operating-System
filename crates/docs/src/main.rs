use std::sync::Arc;

use anyhow::{Context, Result};
use pmos_docs::app::{router, AppConfig, Runtime};
use pmos_docs::config::Config;
use pmos_docs::store::{DocsStore, MemoryStore, PgStore};
use pmos_docs::sync::Syncer;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env().map_err(anyhow::Error::msg)?;

    let store: Arc<dyn DocsStore> = match &config.database_url {
        Some(url) => {
            let pg = PgStore::connect(url)
                .await
                .context("connecting to Postgres")?;
            pg.migrate().await.context("applying migrations")?;
            Arc::new(pg)
        }
        None => {
            tracing::warn!("PMOS_DEV=1 and no PMOS_DOCS_DATABASE_URL: the index is in memory");
            Arc::new(MemoryStore::default())
        }
    };
    let runtime = Arc::new(Runtime::new(
        store.clone(),
        Syncer::new(config.sync.clone(), store),
    ));

    // Sync on start, then on schedule.
    let cancel = CancellationToken::new();
    {
        let runtime = runtime.clone();
        let every = config.sync_every;
        let cancel = cancel.clone();
        tokio::spawn(async move {
            runtime.sync_once().await;
            let Some(every) = every else { return };
            let mut tick = tokio::time::interval(every);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tick.tick() => { runtime.sync_once().await; }
                }
            }
        });
    }

    let app = router(
        runtime,
        AppConfig {
            token: config.token.clone(),
            base: config.sync.base.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(bind = %config.bind, source = %config.sync.base, "pmos-docs listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}
