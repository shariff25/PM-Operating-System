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
    let mut models = Models::parse(&text, config.cloud_test).map_err(anyhow::Error::msg)?;
    for w in models.lookalike_warnings(&config.clients) {
        tracing::warn!("{w}");
    }
    let usage =
        Arc::new(UsageLog::new(config.usage_log.as_deref()).context("opening the usage log")?);
    if config.stub {
        tracing::warn!("PMOS_GATEWAY_STUB=1: answering with the stub model, no model server");
    }
    if !config.workarounds_on {
        tracing::warn!("PMOS_GATEWAY_WORKAROUNDS=off: every workaround is switched off");
    }
    config.check_keys(&models).map_err(anyhow::Error::msg)?;
    let (class, warning) = config.served_class(&models);
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    models.class = class;
    tracing::info!(class = models.class.as_str(), "gateway class");
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
    // Names and settings only, never a token.
    for client in &config.clients {
        let p = models.client_policy(&client.name);
        tracing::info!(
            client = %client.name, trusted = p.trusted, max_in_flight = p.max_in_flight,
            max_queue = p.max_queue, priority = ?p.priority, user = ?p.user,
            per_user_share = p.per_user_share, shared_person_cap = ?p.shared_person_cap, "client"
        );
    }
    let app = router(GatewayConfig {
        models,
        clients: config.clients.clone(),
        usage: usage.clone(),
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
    // Refusals still held for a summary line are counted in the log before exiting.
    usage.flush_all_refusals();
    Ok(())
}
