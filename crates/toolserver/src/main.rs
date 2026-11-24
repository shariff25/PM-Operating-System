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
    // `pmos-toolserver mcp digests <record.yaml>`: an operator's command for an Admin preparing
    // an approval (docs/toolserver.md, "MCP servers").
    if args.first().map(String::as_str) == Some("mcp") {
        std::process::exit(pmos_toolserver::mcp_client::host::main(args[1..].to_vec()).await);
    }
    // `pmos-toolserver policy-proof record`: the host's prover results (`pmos-admin policy
    // prove`, docs/openshell-prover-and-sandbox-audit-spec.md §A.5), not the server.
    if args.first().map(String::as_str) == Some("policy-proof") {
        std::process::exit(pmos_toolserver::openshell::proofs::host_main(args[1..].to_vec()).await);
    }

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let fake_only = config.model_source.fake_data_only();
    tracing::info!(
        model_source = config.model_source.as_str(),
        fake_data_only = fake_only,
        "model source"
    );
    // Slice H12: under `hosted`, whether this stack would pass every fake-data check (for a
    // gateway built with the test-only list, which every service treats as fake-data mode).
    let fake_eligible = config.fake_settings
        && matches!(config.connectors, Connectors::Fake)
        && config.memory != MemoryBackend::Github
        && pmos_contract::model_source::check_test_content(&config.content_root).is_ok();
    // Fake-data mode (slice H4): the content checkout must be test content.
    if fake_only {
        pmos_contract::model_source::check_test_content(&config.content_root).map_err(|c| {
            anyhow::anyhow!(c
                .iter()
                .map(|c| pmos_contract::model_source::fake_data_refusal(c))
                .collect::<Vec<_>>()
                .join("\n"))
        })?;
    }
    let catalog =
        Catalog::load(&config.content_root, Utc::now().date_naive()).context("loading content")?;
    // MCP server records (docs/mcp-servers-spec.md §2.2): every record in the content checkout is
    // checked at start, whether or not a system is switched onto it.
    let mcp_records = load_mcp_records(&config.content_root)?;
    let mut refusals = pmos_toolserver::mcp_client::startup_refusals(&mcp_records);
    if fake_only {
        refusals.extend(pmos_toolserver::mcp_client::fake_data_refusals(
            &mcp_records,
        ));
    }
    if !refusals.is_empty() {
        anyhow::bail!(refusals.join("\n"));
    }
    let cancel = CancellationToken::new();
    // Slice H12: the model-source history (the switch acknowledgement, X3) lives in the
    // database; `held_real_data` is read once, here, where it is written.
    let mut source_history = pmos_toolserver::model_source::History::default();
    let mut held_real_data = false;

    // MCP servers' per-PM tokens (slice M4): their own table beside the vault's.
    let mut mcp_store: Arc<dyn pmos_toolserver::mcp_client::oauth::McpCredentials> =
        Arc::new(pmos_toolserver::mcp_client::oauth::MemoryMcpCredentials::default());
    // The policy proofs and sandbox decisions (crate::openshell) live in the same database.
    let mut openshell_pg: Option<PgStore> = None;
    let mut stores = match &config.database_url {
        Some(url) => {
            let pg = PgStore::connect(url)
                .await
                .context("connecting to Postgres")?;
            pg.migrate().await.context("applying migrations")?;
            mcp_store = Arc::new(pmos_toolserver::mcp_client::oauth::PgMcpCredentials::new(
                pg.pool().clone(),
            ));
            // The permanent marker (slice H4): written at a start with live connectors or
            // GitHub memory, whatever the source; a fake-data start refuses a database with it.
            let real = matches!(config.connectors, Connectors::Live(_))
                || config.memory == MemoryBackend::Github;
            pg.check_held_real_data(real, fake_only)
                .await
                .map_err(anyhow::Error::msg)?;
            held_real_data = pg
                .held_real_data()
                .await
                .context("reading held_real_data")?;
            source_history = pmos_toolserver::model_source::History::Pg(pg.clone());
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
            openshell_pg = Some(pg.clone());
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
        // The eval stack's `evals` account gets every offered persona, so eval tasks pass the
        // team check (docs/bot-marketplace-spec.md §5). In memory only, as the seed is.
        let seeded = pmos_toolserver::team::seed_evals(
            stores.teams.as_ref(),
            stores.users.as_ref(),
            catalog.roster(),
            Utc::now(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        if seeded > 0 {
            tracing::info!(personas = seeded, "the evals account's team seeded");
        }
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
    let (mut connectors, oauth): (Vec<Arc<dyn Connector>>, Option<Arc<OAuth>>) = match &config
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

    // Per-PM OAuth to MCP servers (slice M4): with live connectors and any `oauth_per_user`
    // record, its tokens sealed with the vault's keys in their own table.
    let mut mcp_oauth = None;
    if let Connectors::Live(live) = &config.connectors {
        if mcp_records
            .iter()
            .any(|r| r.auth == pmos_contract::mcp::AuthKind::OauthPerUser)
        {
            let o = Arc::new(
                pmos_toolserver::mcp_client::oauth::McpOAuth::new(
                    &mcp_records,
                    live.vault_keys.clone(),
                    mcp_store.clone(),
                    people.clone(),
                    mcp_state_key(config.service_token.expose()),
                    live.public_url.clone(),
                    |k| std::env::var(k).ok(),
                    pmos_toolserver::mcp_client::transport::TestRoutes::default(),
                    Some(stores.audit.clone()),
                )
                .map_err(anyhow::Error::msg)?,
            );
            tracing::info!("MCP account linking on: each PM links their own account per server");
            mcp_oauth = Some(o);
        }
    }
    let mcp_tokens: Option<Arc<dyn pmos_toolserver::mcp_client::UserTokens>> = mcp_oauth
        .clone()
        .map(|o| o as Arc<dyn pmos_toolserver::mcp_client::UserTokens>);
    // Systems switched onto MCP (PMOS_MCP_SYSTEMS; default none, the quick way back): with
    // fake connectors Jira goes through the fake MCP server in this process; every other
    // system named is served by its approved record, which an active Admin must have approved.
    let mut switched: Vec<Arc<dyn Connector>> = Vec::new();
    let mut mcp_servers = Vec::new();
    let fake_systems: Vec<String> = config
        .mcp_systems
        .iter()
        .filter(|s| {
            config.fake_mcp && pmos_toolserver::mcp_client::FAKE_SYSTEMS.contains(&s.as_str())
        })
        .cloned()
        .collect();
    if !fake_systems.is_empty() {
        let (served, _fake) = pmos_toolserver::mcp_client::start_fake_for(
            &fake_systems,
            pmos_toolserver::mcp_client::fake::FakeMcp::new(),
            Some(stores.audit.clone()),
            cancel.clone(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        tracing::warn!(systems = ?fake_systems, "PMOS_MCP_SYSTEMS with fake connectors: these systems are served by the fake MCP server (fixtures)");
        switched.extend(served.over(&connectors).map_err(anyhow::Error::msg)?);
        mcp_servers.extend(served.servers);
    }
    let from_records: Vec<String> = config
        .mcp_systems
        .iter()
        .filter(|s| !fake_systems.contains(s))
        .cloned()
        .collect();
    if !from_records.is_empty() {
        let dir = stores.users.directory(Utc::now()).await.map_err(|e| {
            anyhow::anyhow!("the user database can't be read to check MCP approvals: {e}")
        })?;
        pmos_toolserver::mcp_client::check_approvals(&mcp_records, &dir)
            .map_err(|e| anyhow::anyhow!(e.join("\n")))?;
        let served = pmos_toolserver::mcp_client::connectors_for(
            &mcp_records,
            &from_records,
            |k| std::env::var(k).ok(),
            mcp_tokens.clone(),
            Some(stores.audit.clone()),
            &pmos_toolserver::mcp_client::transport::TestRoutes::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for c in &served.connectors {
            tracing::info!(
                system = c.system(),
                "served through its approved MCP server record"
            );
        }
        for c in &served.reads_only {
            tracing::warn!(
                system = c.system(),
                "its record doesn't approve the audience checks: reads are served over MCP, posts stay on the built-in connector"
            );
        }
        switched.extend(served.over(&connectors).map_err(anyhow::Error::msg)?);
        mcp_servers.extend(served.servers);
    }
    // Side by side (PMOS_MCP_COMPARE, fake connectors only, so fake data only): each read runs
    // on the in-process fake and on the fake MCP server, the built-in answer is returned and
    // the comparison audited (`mcp.compare`). Writes go to the built-in fake only.
    if !config.mcp_compare.is_empty() {
        let (shadow, _fake) = pmos_toolserver::mcp_client::start_fake_for(
            &config.mcp_compare,
            pmos_toolserver::mcp_client::fake::FakeMcp::new(),
            Some(stores.audit.clone()),
            cancel.clone(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        for shadow in shadow.connectors {
            let system = shadow.system().to_string();
            if !config.mcp_compare.contains(&system) {
                continue;
            }
            if let Some(builtin) = connectors.iter().find(|c| c.system() == system).cloned() {
                tracing::warn!(system = %system, "PMOS_MCP_COMPARE: reads run on the fake and the fake MCP server, and are compared");
                switched.push(Arc::new(
                    pmos_toolserver::mcp_client::compare::SideBySide::new(
                        builtin,
                        shadow,
                        stores.audit.clone(),
                    ),
                ));
            }
        }
    }
    for c in switched {
        connectors.retain(|old| old.system() != c.system());
        connectors.push(c);
    }

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
        .with_ledger_fast_approval_seconds(config.ledger_fast_approval_seconds)
        .with_slack_draft_ttl_hours(config.slack_draft_ttl_hours)
        .with_audit_retention_days(config.audit_retention_days)
        .with_mcp_servers(mcp_servers)
        // Slice H4: every session open and tool call checks the gateway's model source.
        .with_model_source(Arc::new(
            pmos_toolserver::model_source::SourceGuard::new(
                config.model_source,
                &config.gateway_url,
            )
            .with_history(source_history)
            .with_held_real_data(held_real_data)
            // A gateway with the test-only list (`hosted:stack-test`) is taken under
            // `hosted` only when this stack would pass every fake-data check.
            .with_fake_eligible(fake_eligible && !held_real_data),
        ));
    if let Some(pg) = openshell_pg {
        svc = svc.with_openshell(Arc::new(pg));
    }
    // The sandbox-audit collector (docs/openshell-prover-and-sandbox-audit-spec.md §B.2): off
    // unless PMOS_SANDBOX_AUDIT_TOKEN is set; then its two routes answer and the silence check
    // runs every minute.
    let sandbox_audit = config.sandbox_audit_token.is_some();
    if let Some(t) = &config.sandbox_audit_token {
        svc = svc.with_sandbox_audit_token(t.expose());
        tracing::info!("sandbox-audit collector accepted (PMOS_SANDBOX_AUDIT_TOKEN)");
    }
    // Password sign-in (docs/password-sign-in.md): on when the vault has a key to seal
    // authenticator secrets with; its routes also need assertion `require`.
    match &config.password_keys {
        Some(keys) => {
            let passwords = pmos_toolserver::password::PasswordService::new(keys.clone())
                .map_err(anyhow::Error::msg)?;
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
    if let Some(o) = mcp_oauth {
        svc = svc.with_mcp_oauth(o);
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
    if sandbox_audit {
        pmos_toolserver::openshell::spawn_watchdog(svc.clone(), cancel.clone());
    }
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
        // Fake-data mode: a pull that would bring a restricted file, or lose the TEST-CONTENT
        // marker, is not applied (slice H4).
        let sync = if fake_only {
            sync.with_test_content_check()
        } else {
            sync
        };
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
    let entra = match &config.entra {
        Some(tenant) => {
            tracing::info!(
                tenant = %tenant.tenant_id,
                client = %tenant.client_id,
                "first Entra links: ID tokens validated here for this tenant and client"
            );
            Some(Arc::new(
                pmos_toolserver::app::EntraLinkCheck::new(tenant.clone())
                    .map_err(anyhow::Error::msg)?,
            ))
        }
        None => {
            if config.web_token.is_some() {
                tracing::warn!(
                    "no PMOS_ENTRA_TENANT_ID and PMOS_ENTRA_CLIENT_ID: a first Microsoft sign-in \
                     can't link (the first-link route answers 501); pmos-web's /readyz reports \
                     it when Entra sign-in is on"
                );
            }
            None
        }
    };
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
            entra,
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
async fn dev_password_setup(svc: &ToolService, handle: &str, file: &std::path::Path) -> Result<()> {
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

/// Every record in `<content>/mcp/`, checked against the registry; none when there is no such
/// directory.
fn load_mcp_records(
    content_root: &std::path::Path,
) -> Result<Vec<pmos_contract::mcp::ServerRecord>> {
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(content_root.join("tools.yaml")).context("reading tools.yaml")?,
    )
    .context("parsing tools.yaml")?;
    pmos_contract::mcp::load(content_root, &registry).map_err(|e| anyhow::anyhow!(e.join("\n")))
}

/// The key that signs MCP servers' OAuth `state`: from the service token, under its own label.
fn mcp_state_key(service_token: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(service_token.as_bytes())
        .expect("HMAC takes any key length");
    mac.update(b"pmos-mcp-oauth-state-v1");
    mac.finalize().into_bytes().to_vec()
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
