#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicBool, Ordering};

use pmos_toolserver::audit::{
    AuditEvent, AuditQuery, AuditSink, MemoryAudit, Outcome, ReadEvent, StoredEvent,
};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::context::load_docs;
use pmos_toolserver::pg::PgStore;
use pmos_toolserver::store::{StoreError, StoreResult};
use pmos_toolserver::{Stores, ToolService};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::Connection;
use std::str::FromStr;
use tempfile::TempDir;

pub fn now() -> DateTime<Utc> {
    "2026-11-14T10:00:00Z".parse().unwrap()
}

/// A Postgres store in its own fresh schema, for one test.
///
/// Needs `PMOS_TEST_DATABASE_URL`. Without it, returns `None` locally (the test is skipped)
/// and panics in CI (`CI` is set), so a missing database can't pass silently.
pub struct TestPg {
    pub store: PgStore,
    base: PgConnectOptions,
    schema: String,
    url: String,
}

impl TestPg {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("PMOS_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "PMOS_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: PMOS_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("PMOS_TEST_DATABASE_URL parses");
        let schema = format!("t_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect_with(&base)
            .await
            .expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect_with(base.clone().options([("search_path", schema.as_str())]))
            .await
            .expect("connect to test schema");
        let store = PgStore::from_pool(pool);
        store.migrate().await.expect("migrations apply");
        Some(Self {
            store,
            base,
            schema,
            url,
        })
    }

    /// A connection URL for this test's schema, for a child process (the host commands).
    pub fn schema_url(&self) -> String {
        let sep = if self.url.contains('?') { '&' } else { '?' };
        format!(
            "{}{sep}options=-c%20search_path%3D{}",
            self.url, self.schema
        )
    }

    pub async fn drop_schema(self) {
        self.store.pool().close().await;
        if let Ok(mut admin) = sqlx::PgConnection::connect_with(&self.base).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&mut admin)
            .await;
        }
    }
}

/// An audit sink that can be switched off, to test that writes fail closed.
pub struct SwitchAudit {
    inner: Arc<dyn AuditSink>,
    pub down: AtomicBool,
}

#[async_trait::async_trait]
impl AuditSink for SwitchAudit {
    async fn record(&self, event: AuditEvent) -> StoreResult<()> {
        if self.down.load(Ordering::SeqCst) {
            return Err(StoreError("audit log unreachable".into()));
        }
        self.inner.record(event).await
    }

    async fn reads(
        &self,
        user: &str,
        run_id: &str,
        until: DateTime<Utc>,
        limit: usize,
    ) -> StoreResult<Option<Vec<ReadEvent>>> {
        self.inner.reads(user, run_id, until, limit).await
    }

    async fn search(&self, query: &AuditQuery) -> StoreResult<Option<Vec<StoredEvent>>> {
        self.inner.search(query).await
    }

    async fn count_outcomes(
        &self,
        since: DateTime<Utc>,
    ) -> StoreResult<Option<std::collections::BTreeMap<Outcome, u64>>> {
        self.inner.count_outcomes(since).await
    }

    async fn purge(
        &self,
        retention_days: u32,
        batch: u32,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<pmos_toolserver::audit::Purged>> {
        self.inner.purge(retention_days, batch, now).await
    }

    async fn retention(&self) -> StoreResult<Option<pmos_toolserver::audit::RetentionFacts>> {
        self.inner.retention().await
    }
}

pub struct Fixture {
    pub _dir: TempDir,
    pub root: PathBuf,
    pub svc: Arc<ToolService>,
    /// The stores behind `svc`, to set up states the service can't reach on its own.
    pub stores: Stores,
    pub audit: Arc<MemoryAudit>,
    pub audit_switch: Arc<SwitchAudit>,
    pub jira: Arc<FakeConnector>,
    pub confluence: Arc<FakeConnector>,
    pub slack: Arc<FakeConnector>,
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The repo's content, plus a Restricted context file and an `internal-brief` skill whose
/// data class is `internal`.
pub fn fixture() -> Fixture {
    let (dir, root) = content_tree();
    let (stores, audit) = Stores::in_memory(&root);
    build(dir, root, stores, audit)
}

/// The same fixture with every store in Postgres. `None` when no test database is set.
pub async fn pg_fixture() -> Option<(Fixture, TestPg)> {
    let pg = TestPg::new().await?;
    let (dir, root) = content_tree();
    pg.store.reindex(&load_docs(&root)).await.expect("reindex");
    let fixture = build(
        dir,
        root,
        pg.store.stores(),
        Arc::new(MemoryAudit::default()),
    );
    Some((fixture, pg))
}

/// The handle and Slack id of the admin a seeded directory gets when its members name none
/// (the user database always has an active admin).
pub const FIXTURE_ADMIN: (&str, &str) = ("fixture-admin", "U0FIXADMIN");

/// One person in a fixture's seed. The seed is the import's file shape (a `members:` list),
/// written out by [`seed_yaml`] and parsed by the tool server's import, the only code that
/// parses that shape (slice U7).
#[derive(Debug, Clone)]
pub struct SeedMember {
    pub handle: String,
    pub slack_id: String,
    pub email: Option<String>,
    pub github: Option<String>,
    pub entra_oid: Option<String>,
    pub roles: Vec<pmos_contract::model::Role>,
}

/// `members` in the seed's YAML shape.
pub fn seed_yaml(members: &[SeedMember]) -> String {
    let quote = |s: &str| serde_json::to_string(s).unwrap();
    let mut out = String::from("members:\n");
    if members.is_empty() {
        return "members: []\n".into();
    }
    for m in members {
        out.push_str(&format!("  - handle: {}\n", quote(&m.handle)));
        out.push_str(&format!("    slack-id: {}\n", quote(&m.slack_id)));
        if let Some(e) = &m.email {
            out.push_str(&format!("    email: {}\n", quote(e)));
        }
        if let Some(g) = &m.github {
            out.push_str(&format!("    github: {}\n", quote(g)));
        }
        if let Some(o) = &m.entra_oid {
            out.push_str(&format!("    entra-oid: {}\n", quote(o)));
        }
        let roles: Vec<&str> = m.roles.iter().map(|r| r.as_str()).collect();
        out.push_str(&format!("    roles: [{}]\n", roles.join(", ")));
    }
    out
}

/// `members` as a bootstrap seed, with [`FIXTURE_ADMIN`] added when none of them is an admin.
pub fn seed_request(mut members: Vec<SeedMember>) -> pmos_toolserver::users::ImportRequest {
    use pmos_contract::model::Role;
    if !members.iter().any(|m| m.roles.contains(&Role::Admin)) {
        members.push(SeedMember {
            handle: FIXTURE_ADMIN.0.into(),
            slack_id: FIXTURE_ADMIN.1.into(),
            email: None,
            github: None,
            entra_oid: None,
            roles: vec![Role::Pm, Role::Admin],
        });
    }
    pmos_toolserver::users::ImportRequest::parse(
        seed_yaml(&members).as_bytes(),
        None,
        pmos_toolserver::users::ImportMode::Bootstrap,
    )
    .expect("the fixture seed parses")
}

/// An in-memory user store seeded with `members` (and [`FIXTURE_ADMIN`] if none is an admin),
/// whose deactivations reach `stores`. The `evals` account may have its fake Slack id, as in
/// the eval stack's seed.
pub async fn memory_users(
    members: Vec<SeedMember>,
    stores: &Stores,
) -> Arc<dyn pmos_toolserver::users::UserStore> {
    let rules = pmos_toolserver::users::Rules {
        evals_slack_allowed: true,
        ..Default::default()
    };
    Arc::new(
        pmos_toolserver::users::MemoryUsers::seeded(
            seed_request(members),
            rules,
            Some(stores.audit.clone()),
        )
        .await
        .expect("the fixture seed applies")
        .with_linked(stores.linked()),
    )
}

/// Seed `members` (and [`FIXTURE_ADMIN`] if none is an admin) into `stores`' own user store,
/// as `import --bootstrap` does.
pub async fn seed_users(stores: &Stores, members: Vec<SeedMember>) {
    stores
        .users
        .apply(
            &pmos_toolserver::users::Actor::import(None),
            pmos_toolserver::users::Op::Import(Box::new(seed_request(members))),
            seeded_at(),
        )
        .await
        .expect("the fixture seed applies");
}

/// When fixtures seed their user database ([`now`]): its audit events are the newest.
pub fn seeded_at() -> DateTime<Utc> {
    now()
}

/// A seed member: `handle`, Slack id, roles.
pub fn member(handle: &str, slack_id: &str, roles: Vec<pmos_contract::model::Role>) -> SeedMember {
    SeedMember {
        handle: handle.to_string(),
        slack_id: slack_id.to_string(),
        email: None,
        github: None,
        entra_oid: None,
        roles,
    }
}

/// A distinct fake Slack id for the `i`th fixture member.
fn slack_of(i: usize) -> String {
    format!("U0TEST{i:03}")
}

/// The in-memory fixture with the user database enforced, holding `members` as PMs.
pub async fn team_fixture(members: &[&str]) -> Fixture {
    let (dir, root) = content_tree();
    let (stores, audit) = Stores::in_memory(&root);
    seed_users(
        &stores,
        members
            .iter()
            .enumerate()
            .map(|(i, h)| member(h, &slack_of(i), vec![pmos_contract::model::Role::Pm]))
            .collect(),
    )
    .await;
    build_with(dir, root, stores, audit, true)
}

/// For the Slack reply assistant: the user database seeded with the Slack fixtures' team
/// ([`pmos_toolserver::connectors::fake::SLACK_TEAM`]: evals, ben and cy), and a `reply-test`
/// skill whose data class is `restricted`, with the context, Jira, docs, Slack and memory tools. In
/// memory, or with `postgres` every store in Postgres (`None` when no test database is set).
pub async fn reply_fixture(postgres: bool) -> Option<(Fixture, Option<TestPg>)> {
    let (dir, root) = content_tree();
    write(
        &root,
        "skills/reply-test/SKILL.md",
        "---\nname: reply-test\ndescription: Test skill that drafts Slack replies.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"restricted\"\n  tools: \"context.search context.read jira.search jira.read docs.search docs.read slack.search slack.read_thread slack.post_reply slack.send_message jira.create_issue memory.read memory.update\"\n---\n\n# /reply-test\n\nTest.\n",
    );
    let team: Vec<_> = pmos_toolserver::connectors::fake::SLACK_TEAM
        .iter()
        .map(|(handle, slack_id)| member(handle, slack_id, vec![pmos_contract::model::Role::Pm]))
        .collect();
    if postgres {
        let pg = TestPg::new().await?;
        pg.store.reindex(&load_docs(&root)).await.expect("reindex");
        let mut stores = pg.store.stores();
        // Audited to Postgres, and to memory for the test to read.
        let audit = Arc::new(MemoryAudit::default());
        stores.audit = Arc::new(pmos_toolserver::audit::TeeAudit(vec![
            stores.audit.clone(),
            audit.clone(),
        ]));
        // The users in memory: the `evals` account's fake Slack id is an eval stack's seed,
        // which the database never holds.
        stores.users = memory_users(team, &stores).await;
        let f = build_with(dir, root, stores, audit, true);
        Some((f, Some(pg)))
    } else {
        let (mut stores, audit) = Stores::in_memory(&root);
        stores.users = memory_users(team, &stores).await;
        Some((build_with(dir, root, stores, audit, true), None))
    }
}

/// The user database seeded with `members` and their roles; every store in memory, or with
/// `postgres` in Postgres (`None` when no test database is set). The audit log is read back
/// from the backend under test.
pub async fn roles_fixture(
    members: &[(&str, pmos_contract::model::Role)],
    postgres: bool,
) -> Option<(Fixture, Option<TestPg>)> {
    let (dir, root) = content_tree();
    let team: Vec<_> = members
        .iter()
        .enumerate()
        .map(|(i, (handle, role))| member(handle, &slack_of(i), vec![role.clone()]))
        .collect();
    if postgres {
        let pg = TestPg::new().await?;
        pg.store.reindex(&load_docs(&root)).await.expect("reindex");
        let stores = pg.store.stores();
        seed_users(&stores, team).await;
        let f = build_with(dir, root, stores, Arc::new(MemoryAudit::default()), true);
        Some((f, Some(pg)))
    } else {
        let (stores, audit) = Stores::in_memory(&root);
        seed_users(&stores, team).await;
        Some((build_with(dir, root, stores, audit, true), None))
    }
}

fn build(dir: TempDir, root: PathBuf, stores: Stores, audit: Arc<MemoryAudit>) -> Fixture {
    build_with(dir, root, stores, audit, false)
}

/// `enforced`: the user database decides who may use PM OS; otherwise it is open (tests
/// only: any valid handle not listed may).
fn build_with(
    dir: TempDir,
    root: PathBuf,
    mut stores: Stores,
    audit: Arc<MemoryAudit>,
    enforced: bool,
) -> Fixture {
    let audit_switch = Arc::new(SwitchAudit {
        inner: stores.audit.clone(),
        down: AtomicBool::new(false),
    });
    stores.audit = audit_switch.clone();
    let catalog = Catalog::load(&root, now().date_naive()).expect("fixture content lints");
    let jira = Arc::new(FakeConnector::jira());
    let confluence = Arc::new(FakeConnector::confluence());
    let slack = Arc::new(FakeConnector::slack());
    let connectors: Vec<Arc<dyn Connector>> = vec![
        jira.clone(),
        confluence.clone(),
        slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    let mut svc = ToolService::new(catalog, stores.clone(), connectors).unwrap();
    if !enforced {
        svc = svc.with_open_directory();
    }
    Fixture {
        _dir: dir,
        root,
        svc: Arc::new(svc),
        stores,
        audit,
        audit_switch,
        jira,
        confluence,
        slack,
    }
}

/// The repo's content (with the team bot `docs`), plus:
/// - a Restricted context file and an `internal-brief` skill whose data class is `internal`;
/// - a `field-scout` role (internal, skill `field-watch`, `slack.post_reply: never`);
/// - team bots `scout` (reads Jira `PM`, Confluence `ENG`, Slack `C0FIELD`) and `blind`
///   (reads nothing), both `field-scout`;
/// - `pm-ana`, Ana's personal `pm-assistant` bot, with `slack.send_message: never`.
pub fn content_tree() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("content");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content"),
        &root,
    );
    write(
        &root,
        "context/partners/globex-nda.md",
        "---\ntitle: Globex partnership (NDA)\nowner: \"@pm\"\nlast-reviewed: 2026-11-01\nsensitivity: restricted\n---\n\n# Globex\n\nJoint roadmap for key management under NDA.\n",
    );
    write(
        &root,
        "context/products/key-management.md",
        "---\ntitle: Key management overview\nowner: \"@pm\"\nlast-reviewed: 2026-06-01\nsensitivity: internal\n---\n\n# Key management\n\nPublic overview of key management in Acme Vault.\n",
    );
    write(
        &root,
        "skills/internal-brief/SKILL.md",
        "---\nname: internal-brief\ndescription: Test skill limited to internal data.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"context.search context.read jira.search jira.create_issue memory.read memory.update\"\n---\n\n# /internal-brief\n\nTest.\n",
    );
    write(
        &root,
        "skills/page-update/SKILL.md",
        "---\nname: page-update\ndescription: Test skill that updates Confluence pages.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"confluence.read confluence.create_draft confluence.update_page\"\n---\n\n# /page-update\n\nTest.\n",
    );
    write(
        &root,
        "skills/field-watch/SKILL.md",
        "---\nname: field-watch\ndescription: Test skill that watches field questions.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"context.search context.read jira.search jira.read jira.create_issue confluence.search confluence.read slack.search slack.read_thread slack.post_reply\"\n---\n\n# /field-watch\n\nTest.\n",
    );
    write(
        &root,
        "roles/field-scout/ROLE.md",
        "---\nname: field-scout\ndescription: Test role that watches field questions.\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"field-watch\"\ndata-class: \"internal\"\nmodel: \"default\"\nactions:\n  slack.post_reply: never\n---\n\n# Field scout\n\nWatch the field channel.\n",
    );
    write(
        &root,
        "bots/scout/BOT.md",
        "---\nname: scout\nrole: field-scout\nscope: team\nowner: \"@ana\"\ndescription: Test team bot.\nreads:\n  jira-projects: [PM]\n  confluence-spaces: [ENG]\n  slack-channels: [C0FIELD]\n---\n\n# Scout\n\nAnswer field questions.\n",
    );
    write(
        &root,
        "bots/blind/BOT.md",
        "---\nname: blind\nrole: field-scout\nscope: team\nowner: \"@ana\"\ndescription: Test team bot with no read scope.\n---\n\n# Blind\n\nReads nothing.\n",
    );
    write(
        &root,
        "bots/pm-ana/BOT.md",
        "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\ndescription: Ana's assistant.\nactions:\n  slack.send_message: never\n---\n\n# Ana's assistant\n\nHelp Ana.\n",
    );

    (dir, root)
}

/// Write a file under the fixture's content root, as a merge to the content repo would.
pub fn write_content(root: &Path, rel: &str, text: &str) {
    write(root, rel, text)
}
