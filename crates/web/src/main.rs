use anyhow::{Context, Result};
use pmos_web::{app, Config};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Refuses to start without a sign-in provider, weak tokens or bad URLs.
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    // Who may sign in, and their roles, come from the user database at the tool server
    // (`resolve`), not from a file: nothing to check here.
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
            // Only with PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1 and a loopback bind (config.rs).
            tracing::warn!(
                "BOTH ENTRA ID AND DEVELOPMENT SIGN-IN ARE ON (PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1): \
                 development sign-in bypasses Entra ID entirely, so anyone who can reach this \
                 port can still sign in as any member. Never on a server people use."
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
    let (bind, internal_bind) = (config.bind, config.internal_bind);
    let mut deps = pmos_web::store_config::deps_from_env()
        .await
        .map_err(anyhow::Error::msg)?;
    // The admin policy and content screens. pmos-web holds no GitHub token: the tool server
    // opens proposals with the admin's own linked GitHub account.
    deps.admin = pmos_web::admin_config::AdminConfig::from_env(&config.content_root)
        .map_err(anyhow::Error::msg)?;
    tracing::info!(
        content_dir = deps.admin.layout.content_dir(),
        policies_dir = deps.admin.layout.policies_dir(),
        "admin proposals go to the tool server"
    );
    if deps.admin.policies_root.is_none() {
        tracing::info!("no policies root: the policy screen can't compare with checked-in files");
    }
    let server = app::build_server(config, deps).map_err(anyhow::Error::msg)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(bind = %bind, "pmos-web listening");
    let shutdown = CancellationToken::new();
    // The periodic purge: expired sessions, follow-ups, `changed` items, edits and
    // suggestions go whether or not anyone signs in.
    let purge = tokio::spawn(
        server
            .housekeeping
            .clone()
            .run(app::HOUSEKEEPING_EVERY, shutdown.clone()),
    );
    // The server-to-server route on its own listener, when PMOS_WEB_INTERNAL_BIND is set.
    let internal = match (internal_bind, server.internal) {
        (Some(at), Some(router)) => {
            let listener = tokio::net::TcpListener::bind(at)
                .await
                .with_context(|| format!("binding {at}"))?;
            tracing::info!(bind = %at, "pmos-web internal route listening");
            let stop = shutdown.clone();
            Some(tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(async move { stop.cancelled_owned().await })
                    .await
            }))
        }
        _ => None,
    };
    let stop = shutdown.clone();
    tokio::spawn(async move {
        pmos_runtime::shutdown_signal().await;
        tracing::info!("shutting down: finishing in-flight requests");
        stop.cancel();
    });
    let stop = shutdown.clone();
    axum::serve(listener, server.public)
        .with_graceful_shutdown(async move { stop.cancelled_owned().await })
        .await?;
    shutdown.cancel();
    if let Some(internal) = internal {
        internal.await??;
    }
    purge.await?;
    Ok(())
}
