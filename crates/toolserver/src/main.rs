use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::audit::{AuditSink, JsonlAudit, TeeAudit};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::config::{Config, Connectors, MemoryBackend};
use pmos_toolserver::connectors::atlassian::{Confluence, Jira};
use pmos_toolserver::connectors::docs::Docs;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::slack::Slack;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::content_sync::ContentSync;
use pmos_toolserver::context::load_docs;
use pmos_toolserver::github::GitHub;
use pmos_toolserver::github_memory::GitHubMemory;
use pmos_toolserver::oauth::{OAuth, OAuthConfig, TokenSource};
use pmos_toolserver::onboarding::{GitHubSetup, Onboarding};
use pmos_toolserver::pg::PgStore;
use pmos_toolserver::vault::Vault;
use pmos_toolserver::{router_full, AppConfig, PersonClients, Stores, ToolService};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// How often expired sessions and grants are deleted.
const PRUNE_EVERY: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> Result<()> {
    // `pmos-toolserver users …`: a host command (docs/runbooks/users.md), not the server.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("users") {
        std::process::exit(pmos_toolserver::users::host::main(args[1..].to_vec()).await);
    }

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
            pg.with_deactivation_cap(config.users_deactivations_per_hour)
                .stores()
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
    // The user database (docs/user-database-spec.md): since slice U3 the tool server decides
    // from it who may use PM OS. In development the in-memory store is seeded (config.rs
    // refuses a seed with a database or live connectors); with no seed and no database
    // nobody may use PM OS. Since slice U7 no service reads a team file: the runtime, Slack,
    // pmos-web and the console ask this server's directory.
    let rules = pmos_toolserver::users::Rules {
        evals_slack_allowed: config.database_url.is_none()
            && matches!(config.connectors, Connectors::Fake),
        deactivations_per_hour: config.users_deactivations_per_hour,
    };
    if let Some(path) = &config.users_seed {
        let req = pmos_toolserver::users::import::seed_request(path).map_err(anyhow::Error::msg)?;
        let seeded =
            pmos_toolserver::users::MemoryUsers::seeded(req, rules, Some(stores.audit.clone()))
                .await
                .map_err(anyhow::Error::msg)?
                .with_linked(stores.linked());
        stores.users = Arc::new(seeded);
        tracing::info!("user store seeded from PMOS_USERS_SEED (development only)");
    } else if config.database_url.is_none() {
        stores.users = Arc::new(
            pmos_toolserver::users::MemoryUsers::new(rules)
                .with_audit(stores.audit.clone())
                .with_linked(stores.linked()),
        );
    }
    match stores.users.directory(Utc::now()).await {
        Ok(dir) if dir.users.is_empty() => tracing::warn!(
            "the user database is empty: nobody can use PM OS until `pmos-admin users import` \
             (or bootstrap-admin) runs, or a dev stack sets PMOS_USERS_SEED"
        ),
        Ok(dir) => tracing::info!(users = dir.users.len(), "user database loaded"),
        Err(e) => tracing::error!(error = %e, "the user database can't be read"),
    }
    let people = Arc::new(pmos_toolserver::users::access::People::new(
        stores.users.clone(),
    ));

    let mut github_setup = None;
    let mut proposals = None;
    let (connectors, oauth): (Vec<Arc<dyn Connector>>, Option<Arc<OAuth>>) = match &config
        .connectors
    {
        Connectors::Fake => {
            tracing::warn!("PMOS_CONNECTORS=fake: serving fixture Jira, Confluence and Slack");
            let docs: Arc<dyn Connector> = match &config.fake_mode_docs {
                Some((url, token)) => {
                    tracing::info!(url = %url, "docs tools use the real pmos-docs (public support docs)");
                    Arc::new(Docs::new(url.clone(), token.expose()))
                }
                None => Arc::new(FakeConnector::docs()),
            };
            (
                vec![
                    Arc::new(FakeConnector::jira()),
                    Arc::new(FakeConnector::confluence()),
                    Arc::new(FakeConnector::slack()),
                    docs,
                ],
                None,
            )
        }
        Connectors::Live(live) => {
            let vault = Vault::new(live.vault_keys.clone(), stores.credentials.clone());
            let oauth = Arc::new(OAuth::new(
                OAuthConfig {
                    public_base_url: live.public_url.clone(),
                    atlassian: Some(live.atlassian.clone()),
                    atlassian_site: live.atlassian_site.clone(),
                    slack: Some(live.slack.clone()),
                    github: live.github.as_ref().map(|g| g.app.clone()),
                    github_org: live.github.as_ref().map(|g| g.org.clone()),
                    state_key: state_key(config.service_token.expose()),
                    endpoints: live.endpoints.clone(),
                },
                vault,
                people.clone(),
            ));
            let tokens: Arc<dyn TokenSource> = oauth.clone();
            tracing::info!(
                public_url = %live.public_url,
                vault_key = %live.vault_keys.active_id(),
                "live connectors: every call acts with the PM's own token"
            );
            if let Some(gh) = &live.github {
                let github = Arc::new(GitHub::new(live.endpoints.github_api.clone()));
                if config.memory == MemoryBackend::Github {
                    stores.memory = Arc::new(GitHubMemory::new(
                        github.clone(),
                        tokens.clone(),
                        stores.memory_repos.clone(),
                        gh.org.clone(),
                    ));
                    tracing::info!(org = %gh.org, "PMOS_MEMORY=github: each PM's memory is in their own repo");
                }
                tracing::info!(
                    org = %gh.org,
                    content_repo = gh.content_repo.as_deref().unwrap_or("none"),
                    "GitHub linking on"
                );
                proposals = gh.proposals.clone();
                github_setup = Some(GitHubSetup {
                    github,
                    org: gh.org.clone(),
                    content_repo: gh.content_repo.clone(),
                    content_branch: gh.content_branch.clone(),
                    content_dir: gh.content_dir.clone(),
                    policies_dir: gh.policies_dir.clone(),
                    content_root: config.content_root.clone(),
                });
            }
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
    let bots = catalog.roster().bots().count();
    let mut svc = ToolService::new(catalog, stores, connectors)
        .map_err(anyhow::Error::msg)?
        .with_people(people)
        .with_ledger_expire_days(config.ledger_expire_days)
        .with_slack_draft_ttl_hours(config.slack_draft_ttl_hours)
        .with_audit_retention_days(config.audit_retention_days);
    // Password sign-in (docs/password-sign-in.md): on when the vault has a key to seal
    // authenticator secrets with; its routes also need assertion `require`.
    match &config.password_keys {
        Some(keys) => {
            let passwords =
                pmos_toolserver::password::PasswordService::new(keys.clone()).map_err(anyhow::Error::msg)?;
            let p = passwords.params();
            tracing::info!(
                argon2id_m_kib = p.m_kib,
                argon2id_t = p.t,
                argon2id_p = p.p,
                "password sign-in available (pmos-web turns it on with PMOS_WEB_PASSWORD_LOGIN=1)"
            );
            svc = svc.with_passwords(Arc::new(passwords));
        }
        None => tracing::info!("password sign-in off: no PMOS_VAULT_KEY"),
    }
    if let Some((handle, file)) = &config.dev_password_setup {
        dev_password_setup(&svc, handle, file).await?;
    }
    let svc = Arc::new(svc);
    // Audit retention (docs/audit-retention.md): daily, the first run a few minutes after
    // start-up; the database itself holds every rule.
    pmos_toolserver::retention::spawn(
        svc.stores().audit.clone(),
        config.audit_retention_days,
        cancel.clone(),
    );
    tracing::info!(
        retention_days = config.audit_retention_days,
        "audit retention on: events older than this are deleted daily"
    );
    if config.database_url.is_some() {
        pmos_toolserver::retention::spawn_role_changes(svc.stores().users.clone(), cancel.clone());
    }
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
    spawn_draft_sweeper(svc.clone(), cancel.clone());
    spawn_deactivation_sweeper(svc.clone(), cancel.clone());
    let mut onboarding = Onboarding::new(svc.clone(), oauth.clone(), github_setup);
    match &proposals {
        Some(p) => {
            onboarding = onboarding.with_proposals(p.per_hour, p.policies_root.clone());
            tracing::info!(
                per_hour = p.per_hour,
                "admin proposals on: opened as the acting admin, with their linked GitHub account"
            );
        }
        None => tracing::info!("admin proposals off (PMOS_PROPOSALS, PMOS_CONTENT_REPO)"),
    }
    if config.content_sync_secs > 0 {
        let sync = ContentSync::new(&config.content_root, svc.clone());
        sync.clone().spawn(
            Duration::from_secs(config.content_sync_secs),
            cancel.clone(),
        );
        onboarding = onboarding.with_sync(sync);
        tracing::info!(every_secs = config.content_sync_secs, "content sync on");
    }
    tracing::info!(
        web = config.web_token.is_some(),
        console = config.console_token.is_some(),
        mode = config.assertions.mode().as_str(),
        keys = ?config.assertions.key_ids(),
        "approvals clients that act for a person, and their user assertions"
    );
    if let Some(warning) = config.assertions.insecure_warning() {
        tracing::error!("{warning}");
    }
    let app = router_full(
        svc,
        AppConfig {
            service_token: config.service_token.expose().to_string(),
            bot: config.bot_token.as_ref().map(|token| BotAccess {
                token: token.expose().to_string(),
                skills: config.bot_skills.clone(),
            }),
            console_token: config
                .console_token
                .as_ref()
                .map(|t| t.expose().to_string()),
            content_root: config.content_root.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
            oauth,
        },
        Some(Arc::new(onboarding)),
        PersonClients {
            web_token: config.web_token.as_ref().map(|t| t.expose().to_string()),
            assertions: config.assertions.clone(),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        skills,
        bots,
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

/// Development only (config.rs refuses it anywhere else): a setup code for `handle`, issued as
/// the host would, written to a new file only its owner can read, so a test stack can print it.
async fn dev_password_setup(
    svc: &ToolService,
    handle: &str,
    file: &std::path::Path,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let host = pmos_toolserver::users::HostUser::parse("dev")
        .ok_or_else(|| anyhow::anyhow!("host user"))?;
    let issued = pmos_toolserver::password::issue_setup_code(
        svc.stores().users.passwords(),
        &pmos_toolserver::users::Actor::host(host),
        handle,
        Utc::now(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("PMOS_DEV_PASSWORD_SETUP_FOR: {}", e.message()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .with_context(|| format!("PMOS_DEV_PASSWORD_SETUP_FILE {}", file.display()))?;
    writeln!(f, "{}", issued.code.expose())?;
    tracing::warn!(
        user = %handle,
        "DEVELOPMENT: a password setup code was written to PMOS_DEV_PASSWORD_SETUP_FILE"
    );
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

/// Every 10 minutes, from start-up: undecided Slack drafts past `PMOS_SLACK_DRAFT_TTL_HOURS`
/// expire, and decided ones lose their text that long after their decision.
fn spawn_draft_sweeper(svc: Arc<ToolService>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => match svc.sweep_slack_drafts(Utc::now()).await {
                    Ok(swept) if swept.expired + swept.redacted > 0 => tracing::info!(
                        expired = swept.expired,
                        redacted = swept.redacted,
                        "Slack drafts swept"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "sweeping Slack drafts failed"),
                },
            }
        }
    });
}

/// Every 10 minutes, from start-up: people deactivated more than 24 hours ago (and not
/// reactivated) lose their suspended linked accounts, and their held drafts are closed
/// (`docs/user-database-spec.md` §6). With the database unavailable it does nothing.
fn spawn_deactivation_sweeper(svc: Arc<ToolService>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => match svc.sweep_deactivated(Utc::now()).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(people = n, "deactivation grace over: accounts deleted, drafts closed"),
                    Err(e) => tracing::warn!(error = %e, "the deactivation sweeper failed"),
                },
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
