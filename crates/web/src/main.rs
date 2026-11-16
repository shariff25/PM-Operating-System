use anyhow::{Context, Result};
use pmos_contract::model::TeamFile;
use pmos_web::{app, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Refuses to start without a sign-in provider, weak tokens or bad URLs.
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    // Refuse to start on a team file that doesn't parse; it's re-read on every request.
    let team = TeamFile::load(&config.content_root).map_err(anyhow::Error::msg)?;
    if team.members.is_empty() {
        tracing::warn!(
            root = %config.content_root.display(),
            "team.yaml lists nobody: every sign-in will be refused"
        );
    }
    for provider in app::providers(&config) {
        if provider.is_insecure() {
            tracing::warn!(
                provider = provider.name(),
                "DEVELOPMENT SIGN-IN IS ON (PMOS_WEB_DEV_LOGIN=1): anyone who can reach this \
                 port can sign in as any member, admins included. Never expose it."
            );
        }
    }
    // Which upstreams get a signed user assertion on calls for a PM (PMOS_ASSERTION_KEYS).
    tracing::info!(
        bots = config.bots_token.signs(),
        toolserver = config.toolserver_token.signs(),
        "user assertions signed for"
    );
    if config.public_url.is_none() {
        tracing::info!("no PMOS_WEB_PUBLIC_URL: writes must come from the request's own Host");
    }
    if config.static_dir.is_none() {
        tracing::info!("no PMOS_WEB_STATIC_DIR: serving /api only");
    }
    let bind = config.bind;
    let router = pmos_web::build(
        config,
        pmos_web::store_config::deps_from_env()
            .await
            .map_err(anyhow::Error::msg)?,
    )
    .map_err(anyhow::Error::msg)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(bind = %bind, "pmos-web listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            pmos_runtime::shutdown_signal().await;
            tracing::info!("shutting down: finishing in-flight requests");
        })
        .await?;
    Ok(())
}
