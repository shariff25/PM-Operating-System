use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use pmos_toolserver::audit::{AuditSink, JsonlAudit, TeeAudit};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::config::Config;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::context::load_docs;
use pmos_toolserver::pg::PgStore;
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// How often expired sessions and grants are deleted.
const PRUNE_EVERY: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let catalog =
        Catalog::load(&config.content_root, Utc::now().date_naive()).context("loading content")?;
    let cancel = CancellationToken::new();

    let mut stores = match &config.database_url {
        Some(url) => {
            let pg = PgStore::connect(url)
                .await
                .context("connecting to Postgres")?;
            pg.migrate().await.context("applying migrations")?;
            let report = pg
                .reindex(&load_docs(&config.content_root))
                .await
                .context("indexing the context library")?;
            tracing::info!(
                indexed = report.indexed,
                removed = report.removed,
                "context indexed"
            );
            spawn_pruner(pg.clone(), cancel.clone());
            pg.stores()
        }
        None => {
            tracing::warn!("PMOS_DEV=1 and no PMOS_DATABASE_URL: every store is in memory");
            Stores::in_memory(&config.content_root).0
        }
    };
    if let Some(path) = &config.audit_log {
        let file: Arc<dyn AuditSink> =
            Arc::new(JsonlAudit::open(path).context("opening audit log")?);
        stores.audit = Arc::new(TeeAudit(vec![stores.audit.clone(), file]));
    }

    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
    ];
    tracing::warn!("serving fake Jira, Confluence and Slack connectors");

    let skills = catalog.skills().count();
    let svc = ToolService::new(catalog, stores, connectors).map_err(anyhow::Error::msg)?;
    let app = router(
        Arc::new(svc),
        AppConfig {
            service_token: config.service_token.clone(),
            content_root: config.content_root.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        skills,
        postgres = config.database_url.is_some(),
        "pmos-toolserver listening"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}

fn spawn_pruner(pg: PgStore, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    if let Err(e) = pg.prune(Utc::now()).await {
                        tracing::warn!(error = %e, "pruning expired sessions and grants failed");
                    }
                }
            }
        }
    });
}
