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
    // Slice H4, first: PMOS_EXPOSURE, PMOS_MODEL_SOURCE and PMOS_CONNECTORS, each required.
    let host = pmos_web::host::HostSettings::from_env().map_err(anyhow::Error::msg)?;
    let fake_only = host.model_source.fake_data_only();
    tracing::info!(
        exposure = ?host.exposure,
        model_source = host.model_source.as_str(),
        fake_data_only = fake_only,
        "host settings"
    );
    // Refuses to start without a sign-in provider, weak tokens or bad URLs.
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    // Slice H10: the admin door on every host (D3 (b)); on `public`, no development sign-in and
    // password sign-in only with PMOS_WEB_PASSWORD_CLIENTS (D2 (a)).
    pmos_web::host::check_deployment(&host, &config).map_err(anyhow::Error::msg)?;
    // Under `private`, the public URL must not resolve to a public address (r6): at start
    // here, and every 10 minutes below.
    let private_url = match (host.exposure, &config.public_url) {
        (pmos_web::host::Exposure::Private, Some(u)) => Some(u.clone()),
        _ => None,
    };
    if let Some(url) = &private_url {
        pmos_web::host::check_private_url(url)
            .await
            .map_err(anyhow::Error::msg)?;
    }
    if let Some(admin) = &config.admin {
        tracing::info!(
            admin_url = %admin.url,
            mode = match admin.mode {
                pmos_web::config::AdminMode::Tunnel { .. } => "tunnel",
                pmos_web::config::AdminMode::Proxy { .. } => "proxy",
            },
            entra = admin.entra,
            "the admin door: /api/admin/* only there"
        );
    }
    // Fake-data mode: the content checkout the admin screens read must be test content.
    if fake_only {
        pmos_contract::model_source::check_test_content(&config.content_root).map_err(|c| {
            anyhow::anyhow!(pmos_contract::model_source::fake_data_refusal(
                &c.join("; ")
            ))
        })?;
    }
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
    if config.password_login {
        // The tool server decides each sign-in; pmos-web holds no hash or secret.
        tracing::info!(
            trusted_proxies = config.trusted_proxies.len(),
            "password sign-in is on (PMOS_WEB_PASSWORD_LOGIN=1), with a required authenticator code"
        );
        if config.trusted_proxies.is_empty() {
            // Only with PMOS_WEB_DIRECT=1 on a loopback bind (config.rs).
            tracing::info!("no trusted proxy (PMOS_WEB_DIRECT=1): browsers are counted by their own address, and a forwarded header is refused");
        }
        if config.dev_login {
            // Only with PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1 (config.rs).
            tracing::warn!(
                "BOTH PASSWORD AND DEVELOPMENT SIGN-IN ARE ON (PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1): \
                 anyone who can reach this port can still sign in as any member without a \
                 password. Never on a server people use."
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
    let admin_bind = config.admin.as_ref().map(|a| a.bind);
    let stores = pmos_web::store_config::StoreConfig::from_env().map_err(anyhow::Error::msg)?;
    let mut deps = stores.deps().await.map_err(anyhow::Error::msg)?;
    // The permanent marker (slice H4): written at a start with live connectors; a fake-data
    // start refuses a database that has it.
    stores
        .check_held_real_data(host.connectors_live, fake_only)
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
    let mut server = app::build_server(config, deps).map_err(anyhow::Error::msg)?;
    if fake_only {
        // What people type can't be checked by code: every page says so, on both doors.
        server.public = pmos_web::host::with_test_stack_banner(server.public);
        server.admin = server.admin.map(pmos_web::host::with_test_stack_banner);
        tracing::warn!("PMOS_MODEL_SOURCE=hosted-test: every page shows the test-stack banner");
    }
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
    // The private-exposure check every 10 minutes: a public answer stops sign-in on both doors
    // (and says so at error level) until the name resolves privately again.
    if let Some(url) = private_url {
        let stopped = server.sign_in_stopped.clone();
        let stop = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(10 * 60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tick.tick() => {}
                }
                match pmos_web::host::check_private_url(&url).await {
                    Ok(()) => stopped.store(false, std::sync::atomic::Ordering::Relaxed),
                    Err(e) => {
                        tracing::error!(error = %e, "sign-in stopped on both doors");
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
    }
    // The admin door (slice H10), with each connection's peer address: its gate answers 404 to
    // any peer it doesn't take.
    let admin = match (admin_bind, server.admin) {
        (Some(at), Some(router)) => {
            let listener = tokio::net::TcpListener::bind(at)
                .await
                .with_context(|| format!("binding {at}"))?;
            tracing::info!(bind = %at, "pmos-web admin door listening");
            let stop = shutdown.clone();
            Some(tokio::spawn(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(async move { stop.cancelled_owned().await })
                .await
            }))
        }
        _ => None,
    };
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
    // With the peer's address: password sign-in counts refused guesses per address.
    axum::serve(
        listener,
        server
            .public
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move { stop.cancelled_owned().await })
    .await?;
    shutdown.cancel();
    if let Some(internal) = internal {
        internal.await??;
    }
    if let Some(admin) = admin {
        admin.await??;
    }
    purge.await?;
    Ok(())
}
