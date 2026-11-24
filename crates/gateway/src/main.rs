use std::sync::Arc;

use anyhow::{Context, Result};
use pmos_gateway::config::{Config, Models, UpstreamKind};
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router_with, GatewayConfig, RouterOptions};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // The HTTP libraries' own lines are kept at `warn` whatever RUST_LOG says: their debug and
    // info lines name the upstream's address and port ("connecting to …"), which no gateway
    // line may (V3, slice H4).
    let mut filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    for quiet in ["reqwest", "hyper", "hyper_util", "h2", "rustls"] {
        filter = filter.add_directive(format!("{quiet}=warn").parse().expect("a valid directive"));
    }
    // `pmos-gateway --check-config [path]` (docs/model-providers-spec.md §4.12): every start-up
    // check of the config against this environment, then exit, serving nothing. Prints the
    // roles and their routes by label only, never a URL, host, key or model id.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--check-config") {
        return match pmos_gateway::config::check_config(
            |k| std::env::var(k).ok(),
            args.get(1).map(std::path::PathBuf::from),
        ) {
            Ok(summary) => {
                println!("{summary}");
                Ok(())
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
    }
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
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
    // Slice H4: the config must fit PMOS_MODEL_SOURCE, checked on its own class.
    config.check_source(&models).map_err(anyhow::Error::msg)?;
    let (class, warning) = config.served_class(&models);
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    models.class = class;
    tracing::info!(
        class = models.class.as_str(),
        model_source = config.served_source(),
        "gateway class and model source"
    );
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
        // V3 (slice H4): the role and its kind label, never the provider's host.
        tracing::warn!(
            role = %role.alias(),
            upstream = role.upstream_kind_label(),
            "TESTING ONLY: a remote test upstream is on (an OpenAI-compatible API on the internet), \
             so this role's prompts leave this machine; use test data and test accounts only"
        );
    }
    for role in models.roles.values() {
        // A labelled role is logged by its label and upstream kind only (`config::Role::label`);
        // every role's upstream by its kind, never its URL (V3, slice H4), except that a
        // production gateway's model server keeps its pinned `*.pmos.internal` name.
        let host = role.upstream_host();
        let server = if pmos_gateway::config::pinned_address(&host).is_some() {
            host
        } else {
            String::new()
        };
        tracing::info!(
            role = %role.alias(), model = %role.reported_model(),
            model_sha256 = role.model_sha256().as_deref().unwrap_or(""),
            upstream = role.upstream_kind_label(), model_server = %server,
            labelled = role.label.is_some(), "model role"
        );
    }
    // Names and settings only, never a token.
    for client in &config.clients {
        let p = models.client_policy(&client.name);
        tracing::info!(
            client = %client.name, trusted = p.trusted, max_in_flight = p.max_in_flight,
            max_queue = p.max_queue, priority = ?p.priority, user = ?p.user,
            per_user_share = p.per_user_share,
            person_pool = p.pool.map_or("", |pool| models.pool_name(pool.id)),
            person_pool_slots = ?p.pool.map(|pool| pool.cap), "client"
        );
    }
    // Slice MP2: the upstreams' key files are read afresh on SIGHUP (and on a change of size or
    // modification time, looked at once a second at most).
    let keys = std::sync::Arc::new(pmos_gateway::keys::KeyStore::default());
    #[cfg(unix)]
    {
        let keys = keys.clone();
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .context("listening for SIGHUP")?;
        tokio::spawn(async move {
            while hup.recv().await.is_some() {
                keys.reload();
                tracing::info!("SIGHUP: the upstreams' key files will be read afresh");
            }
        });
    }
    let app = router_with(
        GatewayConfig {
            models,
            clients: config.clients.clone(),
            usage: usage.clone(),
            stub: config.stub,
            workarounds_on: config.workarounds_on,
            anthropic_key: config.anthropic_key.clone(),
            remote_key: config.remote_key.clone(),
            retry: config.retry,
            timeouts: Default::default(),
            model_source: config.model_source,
        },
        RouterOptions {
            keys: keys.clone(),
            // Slice MP8: `disabled-<label>` files here take a model out of every route.
            route_control: std::env::var("PMOS_GATEWAY_ROUTE_CONTROL")
                .ok()
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from),
        },
    );
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
