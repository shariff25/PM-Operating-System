use anyhow::{Context, Result};
use pmos_console::config::{Config, Identity};
use pmos_contract::model::TeamFile;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    // Refuse to start on a team file that doesn't parse; it's re-read on every request.
    let team = TeamFile::load(&config.content_root).map_err(anyhow::Error::msg)?;
    if team.members.is_empty() {
        tracing::warn!(
            root = %config.content_root.display(),
            "team.yaml lists nobody: every request will be refused"
        );
    }
    match &config.identity {
        Identity::Proxy => {
            tracing::info!("trusting X-Forwarded-User: only the SSO proxy may reach this port")
        }
        Identity::Dev(user) => {
            tracing::warn!(user = %user, "PMOS_CONSOLE_DEV_USER: every request is this person")
        }
    }
    if config.audit_log.is_none() {
        tracing::info!("no PMOS_CONSOLE_AUDIT_LOG: task pages won't show audit events");
    }
    let bind = config.bind;
    let app = pmos_console::router(config);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(bind = %bind, "pmos-console listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            pmos_runtime::shutdown_signal().await;
            tracing::info!("shutting down: finishing in-flight requests");
        })
        .await?;
    Ok(())
}
