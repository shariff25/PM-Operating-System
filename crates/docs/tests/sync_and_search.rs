//! Sync and search against a mock portal, on both stores.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::{page_md, seed, start_portal, sync_config, Portal, TestPg};
use pmos_docs::store::{DocsStore, MemoryStore};
use pmos_docs::sync::{SyncConfig, SyncError, Syncer};

struct Case {
    base: String,
    portal: Arc<Portal>,
    store: Arc<dyn DocsStore>,
    syncer: Syncer,
    pg: Option<TestPg>,
    stop: tokio_util::sync::CancellationToken,
}

impl Case {
    fn new_syncer(base: &str, store: Arc<dyn DocsStore>) -> Syncer {
        Syncer::new(
            SyncConfig {
                concurrency: 2,
                max_page_bytes: 64 * 1024,
                min_keep_ratio: 0.5,
                ..sync_config(base)
            },
            store,
        )
    }

    async fn memory() -> Self {
        let (base, portal, stop) = start_portal().await;
        seed(&base, &portal);
        let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
        Self {
            syncer: Self::new_syncer(&base, store.clone()),
            base,
            portal,
            store,
            pg: None,
            stop,
        }
    }

    async fn postgres() -> Option<Self> {
        let pg = TestPg::new().await?;
        let (base, portal, stop) = start_portal().await;
        seed(&base, &portal);
        let store: Arc<dyn DocsStore> = Arc::new(pg.store.clone());
        Some(Self {
            syncer: Self::new_syncer(&base, store.clone()),
            base,
            portal,
            store,
            pg: Some(pg),
            stop,
        })
    }

    async fn finish(self) {
        self.stop.cancel();
        if let Some(pg) = self.pg {
            pg.drop_schema().await;
        }
    }

    fn url(&self, slug: &str) -> String {
        format!("{}/docs/{slug}.md", self.base)
    }
}

macro_rules! both {
    ($($case:ident),* $(,)?) => {
        mod memory { $( #[tokio::test] async fn $case() { let c = super::Case::memory().await; super::$case(&c).await; c.finish().await; } )* }
        mod postgres { $( #[tokio::test] async fn $case() { if let Some(c) = super::Case::postgres().await { super::$case(&c).await; c.finish().await; } } )* }
    };
}

both!(
    first_sync_indexes_every_listed_page,
    unchanged_pages_cost_a_304,
    changed_pages_are_refetched,
    pages_gone_from_the_index_are_removed,
    a_failed_fetch_keeps_the_last_good_copy,
    a_shrunken_index_does_not_wipe_the_store,
    robots_and_login_only_pages_are_not_indexed,
    oversized_pages_are_skipped,
    search_ranks_titles_first_and_filters_by_product,
    redirects_stay_on_the_portal,
    bodies_are_capped_however_they_are_sent,
    an_html_page_keeps_the_last_good_copy,
    many_login_walls_at_once_keep_every_page,
    a_rate_limited_robots_txt_skips_the_run,
    sync_runs_are_recorded,
);

#[tokio::test]
async fn rate_limits_are_honored_and_retried() {
    let c = Case::memory().await;
    c.portal
        .throttle
        .lock()
        .unwrap()
        .insert("/docs/vault-backup.md".into(), 2);
    let started = std::time::Instant::now();
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!((r.fetched, r.failed), (4, 0), "{:?}", r.errors);
    assert_eq!(
        c.portal.hits("/docs/vault-backup.md"),
        3,
        "two 429s, then the page"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "waited Retry-After each time"
    );
    c.finish().await;
}

#[tokio::test]
async fn a_page_that_stays_rate_limited_fails_without_stopping_the_sync() {
    let c = Case::memory().await;
    c.portal
        .throttle
        .lock()
        .unwrap()
        .insert("/docs/getting-started.md".into(), 99);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!((r.fetched, r.failed), (3, 1));
    assert!(r.errors[0].contains("429"));
    assert_eq!(
        c.portal.hits("/docs/getting-started.md"),
        4,
        "first try plus 3 retries"
    );
    c.finish().await;
}

#[tokio::test]
async fn only_pages_under_the_configured_prefixes_are_indexed() {
    let (base, portal, stop) = start_portal().await;
    portal.set_index_paths(
        &base,
        &[
            ("/help/vault-sso.md", "Acme Vault SSO"),
            ("/api/v2/create-key.md", "Create a key"),
            ("/blog/launch.md", "Launch post"),
            ("/help-old/page.md", "Lookalike prefix"),
        ],
    );
    for (path, title) in [
        ("/help/vault-sso.md", "Acme Vault SSO"),
        ("/api/v2/create-key.md", "Create a key"),
        ("/blog/launch.md", "Launch post"),
        ("/help-old/page.md", "Lookalike prefix"),
    ] {
        portal.set_path(path, "\"e\"", &page_md(title, "x", "Body text."));
    }
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let config = SyncConfig {
        site: pmos_docs::page::Site::new(&base)
            .with_prefixes(&["/help/", "/api/"])
            .with_api_prefix("/api/"),
        ..sync_config(&base)
    };
    let r = Syncer::new(config, store.clone())
        .run(Utc::now())
        .await
        .unwrap();
    assert_eq!((r.listed, r.fetched), (2, 2));
    assert_eq!(
        portal.hits("/blog/launch.md"),
        0,
        "not under a prefix: never fetched"
    );
    assert_eq!(portal.hits("/help-old/page.md"), 0);
    let sso = store
        .get(&format!("{base}/help/vault-sso.md"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (sso.section.as_str(), sso.product.as_str()),
        ("docs", "vault")
    );
    let api = store
        .get(&format!("{base}/api/v2/create-key.md"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(api.section, "api", "under the API prefix");
    stop.cancel();
}

#[tokio::test]
async fn the_default_prefix_indexes_every_page_nested_or_not() {
    let (base, portal, stop) = start_portal().await;
    let pages = [
        ("/intro.md", "Introduction"),
        ("/guides/cloud/setup.md", "Set up"),
    ];
    portal.set_index_paths(&base, &pages);
    for (path, title) in pages {
        portal.set_path(path, "\"e\"", &page_md(title, "x", "Body text."));
    }
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let config = SyncConfig {
        site: pmos_docs::page::Site::new(&base),
        ..sync_config(&base)
    };
    let r = Syncer::new(config, store.clone())
        .run(Utc::now())
        .await
        .unwrap();
    assert_eq!((r.listed, r.fetched, r.failed), (2, 2, 0));
    let nested = store
        .get(&format!("{base}/guides/cloud/setup.md"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(nested.product, "cloud", "the URL path names the product");
    assert_eq!(nested.web_url(), format!("{base}/guides/cloud/setup"));
    stop.cancel();
}

#[tokio::test]
async fn a_database_migrated_before_a_comment_was_reworded_still_starts() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let (version, old) = pmos_docs::store::REWORDED[0];
    sqlx::query("UPDATE _pmos_docs_migrations SET checksum = $1 WHERE version = $2")
        .bind(pmos_docs::store::hex(old))
        .bind(version)
        .execute(pg.store.pool())
        .await
        .unwrap();
    pg.store
        .migrate()
        .await
        .expect("the old checksum is accepted");
    let now: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _pmos_docs_migrations WHERE version = $1")
            .bind(version)
            .fetch_one(pg.store.pool())
            .await
            .unwrap();
    assert_ne!(
        now,
        pmos_docs::store::hex(old),
        "the current checksum is recorded"
    );
    // Any other change to a migration is still refused.
    sqlx::query("UPDATE _pmos_docs_migrations SET checksum = $1 WHERE version = $2")
        .bind(vec![0u8; 48])
        .bind(version)
        .execute(pg.store.pool())
        .await
        .unwrap();
    assert!(pg.store.migrate().await.is_err());
    pg.drop_schema().await;
}

#[tokio::test]
async fn requests_are_paced_across_workers() {
    let (base, portal, stop) = start_portal().await;
    seed(&base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let syncer = Syncer::new(
        SyncConfig {
            concurrency: 4,
            delay: Duration::from_millis(200),
            ..sync_config(&base)
        },
        store,
    );
    let started = std::time::Instant::now();
    syncer.run(Utc::now()).await.unwrap();
    // Four pages at one start per 200 ms take at least 600 ms, even with 4 workers.
    assert!(started.elapsed() >= Duration::from_millis(600));
    stop.cancel();
}

async fn first_sync_indexes_every_listed_page(c: &Case) {
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!((r.listed, r.fetched, r.failed, r.removed), (4, 4, 0, 0));
    assert_eq!(c.store.count().await.unwrap(), 4);
    let page = c
        .store
        .get(&c.url("vault-key-rotation"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.title, "Acme Vault Key Rotation");
    assert_eq!(page.product, "vault");
    assert_eq!(
        page.description.as_deref(),
        Some("Rotate keys in Acme Vault")
    );
    assert_eq!(page.headings, "Rotate a key\nRotation policy");
    assert!(page.body.starts_with("# Acme Vault Key Rotation"));
    assert!(!page.body.contains("Documentation Index"));
    assert_eq!(page.etag.as_deref(), Some("\"r1\""));
    assert_eq!(
        c.store
            .get(&c.url("cloud-overview"))
            .await
            .unwrap()
            .unwrap()
            .product,
        "cloud"
    );
}

async fn unchanged_pages_cost_a_304(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!((r.fetched, r.unchanged), (0, 4));
    let seen = c
        .portal
        .conditional
        .lock()
        .unwrap()
        .get("/docs/vault-backup.md")
        .cloned()
        .unwrap();
    assert_eq!(
        seen,
        vec!["\"b1\""],
        "the stored ETag went out as If-None-Match"
    );
}

async fn changed_pages_are_refetched(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    c.portal.set_page(
        "vault-backup",
        "\"b2\"",
        &page_md(
            "Acme Vault Backup and Restore",
            "vault-backup",
            "Now with quantum-safe backups.",
        ),
    );
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!((r.fetched, r.unchanged), (1, 3));
    let hits = c.store.search("quantum", None, 5).await.unwrap();
    assert_eq!(hits[0].page.url, c.url("vault-backup"));
}

async fn pages_gone_from_the_index_are_removed(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    c.portal.set_index(
        &c.base,
        &[
            ("vault-key-rotation", "Acme Vault Key Rotation", None),
            ("vault-backup", "Acme Vault Backup and Restore", None),
            ("cloud-overview", "Acme Cloud Overview", None),
        ],
    );
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.removed, 1);
    assert!(c
        .store
        .get(&c.url("getting-started"))
        .await
        .unwrap()
        .is_none());
}

async fn a_failed_fetch_keeps_the_last_good_copy(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    c.portal.set_status("vault-backup", 503);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.failed, 1);
    assert!(r.errors[0].contains("vault-backup"));
    assert!(c.store.get(&c.url("vault-backup")).await.unwrap().is_some());
}

async fn a_shrunken_index_does_not_wipe_the_store(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    c.portal
        .set_index(&c.base, &[("getting-started", "Getting Started", None)]);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert!(r.removal_skipped);
    assert_eq!(r.removed, 0);
    assert_eq!(c.store.count().await.unwrap(), 4);
}

async fn robots_and_login_only_pages_are_not_indexed(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    *c.portal.robots.lock().unwrap() =
        (200, "User-agent: *\nDisallow: /docs/vault-backup\n".into());
    c.portal.set_status("cloud-overview", 403);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.skipped_robots, 1);
    assert_eq!(r.restricted, 1);
    assert_eq!(
        c.portal.hits("/docs/vault-backup.md"),
        1,
        "not requested again once disallowed"
    );
    assert!(c.store.get(&c.url("vault-backup")).await.unwrap().is_none());
    assert!(c
        .store
        .get(&c.url("cloud-overview"))
        .await
        .unwrap()
        .is_none());

    // If the portal can't say what's allowed, nothing is fetched at all.
    *c.portal.robots.lock().unwrap() = (503, String::new());
    let before = c.portal.total_hits();
    assert!(matches!(
        c.syncer.run(Utc::now()).await,
        Err(SyncError::Robots(_))
    ));
    assert_eq!(
        c.portal.total_hits(),
        before + 1,
        "only robots.txt was requested"
    );
}

async fn oversized_pages_are_skipped(c: &Case) {
    c.portal
        .set_page("getting-started", "\"g2\"", &"x".repeat(100 * 1024));
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.too_large, 1);
    assert_eq!(r.fetched, 3);
}

async fn search_ranks_titles_first_and_filters_by_product(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    let hits = c.store.search("key rotation", None, 10).await.unwrap();
    assert_eq!(hits[0].page.url, c.url("vault-key-rotation"));
    assert!(hits[0].snippet.to_lowercase().contains("rotat"));
    let cloud = c.store.search("enclaves", Some("cloud"), 10).await.unwrap();
    assert_eq!(cloud.len(), 1);
    assert!(c
        .store
        .search("enclaves", Some("vault"), 10)
        .await
        .unwrap()
        .is_empty());
    assert!(c
        .store
        .search("& | ! :*", None, 10)
        .await
        .unwrap()
        .is_empty());
    assert!(!c
        .store
        .search("key & | ! backup", None, 10)
        .await
        .unwrap()
        .is_empty());
    // A page with every query word beats one that repeats a single word more often.
    let both = c.store.search("backup cluster", None, 10).await.unwrap();
    assert_eq!(both[0].page.url, c.url("vault-backup"));
    let mixed = c.store.search("rotation enclaves", None, 10).await.unwrap();
    assert_eq!(mixed.len(), 2, "any-word matches still come back");
    let products = c.store.products().await.unwrap();
    let names: Vec<_> = products
        .iter()
        .map(|p| (p.product.as_str(), p.pages))
        .collect();
    assert_eq!(names, vec![("cloud", 1), ("general", 1), ("vault", 2)]);
}

async fn redirects_stay_on_the_portal(c: &Case) {
    // Another server (another port: another origin) the portal tries to send us to.
    let (other, other_portal, stop_other) = start_portal().await;
    other_portal.set_page("evil", "\"e\"", &page_md("Evil", "evil", "INTERNAL-SECRET"));
    c.portal.redirects.lock().unwrap().insert(
        "/docs/vault-backup.md".into(),
        format!("{other}/docs/evil.md"),
    );
    // A redirect within the portal is followed.
    c.portal.redirects.lock().unwrap().insert(
        "/docs/getting-started.md".into(),
        format!("{}/docs/vault-key-rotation.md", c.base),
    );
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(
        other_portal.hits("/docs/evil.md"),
        0,
        "never left the portal"
    );
    assert_eq!(r.failed, 1);
    assert!(r.errors.iter().any(|e| e.contains("vault-backup")));
    assert!(c.store.get(&c.url("vault-backup")).await.unwrap().is_none());
    let followed = c
        .store
        .get(&c.url("getting-started"))
        .await
        .unwrap()
        .unwrap();
    assert!(followed.body.contains("Rotate"), "{}", followed.body);
    stop_other.cancel();
}

async fn bodies_are_capped_however_they_are_sent(c: &Case) {
    // No Content-Length: the cap applies while reading (the cap here is 64 KB).
    c.portal
        .streamed
        .lock()
        .unwrap()
        .insert("/docs/vault-backup.md".into(), 1024 * 1024);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.too_large, 1);
    assert_eq!(r.fetched, 3);

    // The index has its own cap.
    c.portal
        .streamed
        .lock()
        .unwrap()
        .insert("/llms.txt".into(), pmos_docs::sync::MAX_INDEX_BYTES + 8192);
    assert!(matches!(
        c.syncer.run(Utc::now()).await,
        Err(SyncError::Index(e)) if e.contains("size cap")
    ));
}

async fn an_html_page_keeps_the_last_good_copy(c: &Case) {
    c.syncer.run(Utc::now()).await.unwrap();
    // The portal starts answering with an HTML page (a login wall, an error page) and 200.
    c.portal
        .set_page("vault-backup", "\"b2\"", "<html>Sign in</html>");
    c.portal.content_types.lock().unwrap().insert(
        "/docs/vault-backup.md".into(),
        "text/html; charset=utf-8".into(),
    );
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.failed, 1);
    assert!(r.errors[0].contains("text/html"), "{:?}", r.errors);
    let kept = c.store.get(&c.url("vault-backup")).await.unwrap().unwrap();
    assert!(!kept.body.contains("Sign in"));
}

async fn many_login_walls_at_once_keep_every_page(c: &Case) {
    // Twelve pages, all indexed.
    let slugs: Vec<String> = (0..12).map(|i| format!("kb-{i}")).collect();
    let entries: Vec<(&str, &str, Option<&str>)> = slugs
        .iter()
        .map(|s| (s.as_str(), "KB page", None))
        .collect();
    c.portal.set_index(&c.base, &entries);
    for s in &slugs {
        c.portal
            .set_page(s, "\"k\"", &page_md("KB page", s, "Body."));
    }
    c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(c.store.count().await.unwrap(), 12);

    // Then a bot wall answers 403 for most of them: that's not twelve pages going private.
    for s in &slugs[..8] {
        c.portal.set_status(s, 403);
    }
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.restricted, 8);
    assert!(r.errors[0].contains("blocking"), "{:?}", r.errors);
    assert_eq!(c.store.count().await.unwrap(), 12, "kept every page");

    // One page going private is still dropped.
    for s in &slugs[..8] {
        c.portal.set_status(s, 200);
    }
    c.portal.set_status(&slugs[0], 401);
    let r = c.syncer.run(Utc::now()).await.unwrap();
    assert_eq!(r.restricted, 1);
    assert_eq!(c.store.count().await.unwrap(), 11);
}

async fn a_rate_limited_robots_txt_skips_the_run(c: &Case) {
    *c.portal.robots.lock().unwrap() = (429, String::new());
    assert!(matches!(
        c.syncer.run(Utc::now()).await,
        Err(SyncError::Robots(_))
    ));
    assert_eq!(c.portal.total_hits(), 1, "only robots.txt was requested");
}

async fn sync_runs_are_recorded(c: &Case) {
    assert_eq!(c.store.last_success().await.unwrap(), None);
    let t1 = Utc::now();
    c.store
        .record_sync(t1, false, &serde_json::json!({"error": "x"}))
        .await
        .unwrap();
    assert_eq!(
        c.store.last_success().await.unwrap(),
        None,
        "failures don't count"
    );
    c.store
        .record_sync(t1, true, &serde_json::json!({"listed": 4}))
        .await
        .unwrap();
    let got = c.store.last_success().await.unwrap().unwrap();
    assert!((got - t1).num_milliseconds().abs() < 1);
}
