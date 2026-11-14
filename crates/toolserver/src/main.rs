use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use pmos_toolserver::audit::{AuditSink, JsonlAudit, MemoryAudit};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::config::Config;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::context::MemoryIndex;
use pmos_toolserver::memory::InMemoryStore;
use pmos_toolserver::{router, AppConfig, ToolService};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let catalog =
        Catalog::load(&config.content_root, Utc::now().date_naive()).context("loading content")?;
    let index = MemoryIndex::load(&config.content_root);
    let audit: Arc<dyn AuditSink> = match &config.audit_log {
        Some(path) => Arc::new(JsonlAudit::open(path).context("opening audit log")?),
        None => {
            tracing::warn!("PMOS_DEV=1: audit events are kept in memory only");
            Arc::new(MemoryAudit::default())
        }
    };
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
    ];
    tracing::warn!("serving fake Jira, Confluence and Slack connectors");

    let skills = catalog.skills().count();
    let svc = ToolService::new(
        catalog,
        Arc::new(index),
        Arc::new(InMemoryStore::default()),
        connectors,
        audit,
    )
    .map_err(anyhow::Error::msg)?;

    let cancel = CancellationToken::new();
    let app = router(
        Arc::new(svc),
        AppConfig {
            service_token: config.service_token.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(bind = %config.bind, skills, "pmos-toolserver listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}
