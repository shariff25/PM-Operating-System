use anyhow::{Context, Result};
use pmos_console::config::{Config, Identity};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    // Who may use the console is the user database's answer, through the tool server's
    // `resolve` route; when the tool server can't answer, every request is refused.
    tracing::info!("viewers are looked up in the PM OS user database through the tool server");
    match &config.identity {
        Identity::Proxy { .. } => tracing::info!(
            "trusting X-Forwarded-User on requests that carry the proxy's X-Pmos-Proxy-Secret"
        ),
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
