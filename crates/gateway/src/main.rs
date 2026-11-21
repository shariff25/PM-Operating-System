use std::sync::Arc;

use anyhow::{Context, Result};
use pmos_gateway::config::{Config, Models, UpstreamKind};
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let text = std::fs::read_to_string(&config.models_path)
        .with_context(|| format!("reading {}", config.models_path.display()))?;
    let models = Models::parse(&text, config.cloud_test).map_err(anyhow::Error::msg)?;
    let usage =
        Arc::new(UsageLog::new(config.usage_log.as_deref()).context("opening the usage log")?);
    if config.stub {
        tracing::warn!("PMOS_GATEWAY_STUB=1: answering with the stub model, no model server");
    }
    if !config.workarounds_on {
        tracing::warn!("PMOS_GATEWAY_WORKAROUNDS=off: every workaround is switched off");
    }
    config.check_keys(&models).map_err(anyhow::Error::msg)?;
    if models
        .roles
        .values()
        .any(|r| r.kind == UpstreamKind::ClaudeTest)
    {
        tracing::warn!(
            "TESTING ONLY: a role uses the Claude test upstream, so its prompts leave this machine; \
             use test data and test accounts only"
        );
    }
    for role in models
        .roles
        .values()
        .filter(|r| r.kind == UpstreamKind::RemoteOpenAi)
    {
        tracing::warn!(
            role = %role.alias(),
            upstream = %role.upstream_host(),
            "TESTING ONLY: a remote test upstream is on (an OpenAI-compatible API on the internet), \
             so this role's prompts leave this machine; use test data and test accounts only"
        );
    }
    for role in models.roles.values() {
        tracing::info!(role = %role.alias(), model = %role.model, upstream = %role.upstream, "model role");
    }
    let app = router(GatewayConfig {
        models,
        clients: config.clients.clone(),
        usage,
        stub: config.stub,
        workarounds_on: config.workarounds_on,
        anthropic_key: config.anthropic_key.clone(),
        remote_key: config.remote_key.clone(),
        retry: config.retry,
        timeouts: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(bind = %config.bind, "pmos-gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            pmos_runtime::shutdown_signal().await;
            tracing::info!("shutting down: finishing in-flight requests");
        })
        .await?;
    Ok(())
}
