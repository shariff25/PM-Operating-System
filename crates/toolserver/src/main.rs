use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::audit::{AuditSink, JsonlAudit, TeeAudit};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::config::{Config, Connectors};
use pmos_toolserver::connectors::atlassian::{Confluence, Jira};
use pmos_toolserver::connectors::docs::Docs;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::slack::Slack;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::context::load_docs;
use pmos_toolserver::oauth::{OAuth, OAuthConfig, TokenSource};
use pmos_toolserver::pg::PgStore;
use pmos_toolserver::team::Team;
use pmos_toolserver::vault::Vault;
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// How often expired sessions and grants are deleted.
const PRUNE_EVERY: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let catalog =
        Catalog::load(&config.content_root, Utc::now().date_naive()).context("loading content")?;
    let cancel = CancellationToken::new();

    let mut stores = match &config.database_url {
        Some(url) => {
            let pg = PgStore::connect(url)
                .await
                .context("connecting to Postgres")?;
            pg.migrate().await.context("applying migrations")?;
            let report = pg
                .reindex(&load_docs(&config.content_root))
                .await
                .context("indexing the context library")?;
            tracing::info!(
                indexed = report.indexed,
                removed = report.removed,
                "context indexed"
            );
            spawn_pruner(pg.clone(), cancel.clone());
            pg.stores()
        }
        None => {
            tracing::warn!("PMOS_DEV=1 and no PMOS_DATABASE_URL: every store is in memory");
            Stores::in_memory(&config.content_root).0
        }
    };
    if let Some(path) = &config.audit_log {
        let file: Arc<dyn AuditSink> =
            Arc::new(JsonlAudit::open(path).context("opening audit log")?);
        stores.audit = Arc::new(TeeAudit(vec![stores.audit.clone(), file]));
    }

    let live = matches!(config.connectors, Connectors::Live(_));
    // With live connectors only people in team.yaml may use PM OS, and a linked account must
    // be theirs.
    let team = Team::load(&config.content_root, live).map_err(anyhow::Error::msg)?;

    let (connectors, oauth): (Vec<Arc<dyn Connector>>, Option<Arc<OAuth>>) =
        match &config.connectors {
            Connectors::Fake => {
                tracing::warn!("PMOS_CONNECTORS=fake: serving fixture Jira, Confluence and Slack");
                (
                    vec![
                        Arc::new(FakeConnector::jira()),
                        Arc::new(FakeConnector::confluence()),
                        Arc::new(FakeConnector::slack()),
                        Arc::new(FakeConnector::docs()),
                    ],
                    None,
                )
            }
            Connectors::Live(live) => {
                let vault = Vault::new(live.vault_keys.clone(), stores.credentials.clone());
                let oauth = Arc::new(
                    OAuth::new(
                        OAuthConfig {
                            public_base_url: live.public_url.clone(),
                            atlassian: Some(live.atlassian.clone()),
                            atlassian_site: live.atlassian_site.clone(),
                            slack: Some(live.slack.clone()),
                            state_key: state_key(&config.service_token),
                            endpoints: live.endpoints.clone(),
                        },
                        vault,
                    )
                    .with_team(team.clone()),
                );
                let tokens: Arc<dyn TokenSource> = oauth.clone();
                tracing::info!(
                    public_url = %live.public_url,
                    vault_key = %live.vault_keys.active_id(),
                    "live connectors: every call acts with the PM's own token"
                );
                (
                    vec![
                        Arc::new(Jira::new(
                            tokens.clone(),
                            live.endpoints.atlassian_api.clone(),
                        )),
                        Arc::new(Confluence::new(
                            tokens.clone(),
                            live.endpoints.atlassian_api.clone(),
                        )),
                        Arc::new(Slack::new(tokens, live.endpoints.slack.clone())),
                        Arc::new(Docs::new(live.docs_url.clone(), live.docs_token.expose())),
                    ],
                    Some(oauth),
                )
            }
        };

    for skill in &config.bot_skills {
        if catalog.skill(skill).is_none() {
            anyhow::bail!("PMOS_BOT_SKILLS names unknown skill `{skill}`");
        }
    }
    let skills = catalog.skills().count();
    let svc = ToolService::new(catalog, stores, connectors)
        .map_err(anyhow::Error::msg)?
        .with_team(team.clone());
    let svc = Arc::new(svc);
    // One replica: anything still `executing` from a previous process was interrupted.
    match svc.reconcile(chrono::Duration::zero(), Utc::now()).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            items = n,
            "approved writes interrupted by a restart marked failed (outcome unknown)"
        ),
        Err(e) => tracing::error!(error = %e, "reconciling interrupted writes failed"),
    }
    spawn_reconciler(svc.clone(), cancel.clone());
    let app = router(
        svc,
        AppConfig {
            service_token: config.service_token.clone(),
            bot: config.bot_token.clone().map(|token| BotAccess {
                token,
                skills: config.bot_skills.clone(),
            }),
            content_root: config.content_root.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
            oauth,
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        skills,
        postgres = config.database_url.is_some(),
        "pmos-toolserver listening"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            pmos_runtime::shutdown_signal().await;
            tracing::info!("shutting down: finishing in-flight requests");
            cancel.cancel();
        })
        .await?;
    Ok(())
}

fn spawn_pruner(pg: PgStore, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    if let Err(e) = pg.prune(Utc::now()).await {
                        tracing::warn!(error = %e, "pruning expired sessions and grants failed");
                    }
                }
            }
        }
    });
}

/// Writes still `executing` after 10 minutes are stuck (connector calls time out long
/// before that): mark them failed, outcome unknown.
fn spawn_reconciler(svc: Arc<ToolService>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    if let Err(e) = svc.reconcile(chrono::Duration::minutes(10), Utc::now()).await {
                        tracing::warn!(error = %e, "reconciling stuck writes failed");
                    }
                }
            }
        }
    });
}

/// The key that signs OAuth `state`, derived from the service token so there is one fewer
/// secret to manage. Rotating the service token invalidates sign-in links in flight.
fn state_key(service_token: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(service_token.as_bytes())
        .expect("HMAC takes any key length");
    mac.update(b"pmos-oauth-state-v1");
    mac.finalize().into_bytes().to_vec()
}
