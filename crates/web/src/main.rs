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
    let providers = app::providers(&config).map_err(anyhow::Error::msg)?;
    for provider in &providers {
        if provider.is_insecure() {
            tracing::warn!(
                provider = provider.name(),
                "DEVELOPMENT SIGN-IN IS ON (PMOS_WEB_DEV_LOGIN=1): anyone who can reach this \
                 port can sign in as any member, admins included. Never expose it."
            );
        }
    }
    if let Some(entra) = &config.entra {
        // Ids and URLs only: never the client secret.
        tracing::info!(
            tenant = %entra.tenant_id,
            client = %entra.client_id,
            redirect_uri = %entra.redirect_uri,
            "Entra ID sign-in is on"
        );
        if config.dev_login {
            tracing::warn!(
                "BOTH ENTRA ID AND DEVELOPMENT SIGN-IN ARE ON: development sign-in bypasses \
                 Entra ID entirely, so anyone who can reach this port can still sign in as any \
                 member. Unset PMOS_WEB_DEV_LOGIN on any server people use."
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
    let mut deps = pmos_web::store_config::deps_from_env()
        .await
        .map_err(anyhow::Error::msg)?;
    // The admin policy and content screens. Never logs the GitHub token.
    deps.admin = pmos_web::admin_config::AdminConfig::from_env(&config.content_root)
        .map_err(anyhow::Error::msg)?;
    match &deps.admin.github {
        Some(g) => {
            tracing::info!(repo = %g.repo, base = ?g.base, "admin proposals open pull requests")
        }
        None => tracing::info!("no PMOS_WEB_GITHUB_REPO: admin proposals answer not_configured"),
    }
    if deps.admin.policies_root.is_none() {
        tracing::info!("no policies root: the policy screen can't compare with checked-in files");
    }
    let router = pmos_web::build(config, deps).map_err(anyhow::Error::msg)?;
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
