//! When syncs run: the schedule across restarts, retries after a failure, and a crash in a
//! run never leaving the "running" flag stuck.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use common::{seed, start_portal};
use pmos_docs::app::Runtime;
use pmos_docs::page::DocPage;
use pmos_docs::store::{DocsStore, Hit, MemoryStore, ProductCount, StoreResult};
use pmos_docs::sync::Syncer;

fn syncer(base: &str, store: Arc<dyn DocsStore>) -> Syncer {
    Syncer::new(common::sync_config(base), store)
}

const DAY: Duration = Duration::from_secs(24 * 3600);

#[tokio::test]
async fn a_fresh_index_is_not_synced_again_on_restart() {
    let (base, portal, stop) = start_portal().await;
    seed(&base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let now = Utc::now();

    let first = Runtime::new(store.clone(), syncer(&base, store.clone()));
    assert!(first.due(Some(DAY), now).await, "never synced");
    assert!(first.sync_once().await);

    // A restart an hour later: a new runtime over the same store.
    let restarted = Runtime::new(store.clone(), syncer(&base, store.clone()));
    let later = now + chrono::Duration::hours(1);
    assert!(!restarted.due(Some(DAY), later).await, "still fresh");
    assert!(!restarted.due(None, later).await);
    assert!(
        restarted
            .due(Some(DAY), now + chrono::Duration::hours(25))
            .await
    );
    stop.cancel();
}

#[tokio::test]
async fn a_failed_sync_is_retried_after_an_hour() {
    let (base, portal, stop) = start_portal().await;
    seed(&base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let rt = Runtime::new(store.clone(), syncer(&base, store.clone()));
    assert!(rt.sync_once().await);

    *portal.robots.lock().unwrap() = (503, String::new());
    // A later scheduled run fails...
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(rt.sync_once().await);
    assert!(matches!(&*rt.last.lock().unwrap(), Some(Err(_))));
    let now = Utc::now();
    // ... and is tried again after an hour, not a whole day.
    assert!(!rt.due(Some(DAY), now + chrono::Duration::minutes(10)).await);
    assert!(rt.due(Some(DAY), now + chrono::Duration::minutes(61)).await);
    stop.cancel();
}

/// A store that panics when the sync reads its ETags.
struct Exploding(MemoryStore);

#[async_trait]
impl DocsStore for Exploding {
    async fn upsert(&self, page: &DocPage) -> StoreResult<()> {
        self.0.upsert(page).await
    }
    async fn get(&self, url: &str) -> StoreResult<Option<DocPage>> {
        self.0.get(url).await
    }
    async fn etags(&self) -> StoreResult<HashMap<String, String>> {
        panic!("boom")
    }
    async fn retain(&self, keep: &HashSet<String>) -> StoreResult<u64> {
        self.0.retain(keep).await
    }
    async fn count(&self) -> StoreResult<i64> {
        self.0.count().await
    }
    async fn search(
        &self,
        query: &str,
        product: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Hit>> {
        self.0.search(query, product, limit).await
    }
    async fn products(&self) -> StoreResult<Vec<ProductCount>> {
        self.0.products().await
    }
    async fn record_sync(
        &self,
        finished_at: DateTime<Utc>,
        ok: bool,
        report: &serde_json::Value,
    ) -> StoreResult<()> {
        self.0.record_sync(finished_at, ok, report).await
    }
    async fn last_success(&self) -> StoreResult<Option<DateTime<Utc>>> {
        self.0.last_success().await
    }
}

#[tokio::test]
async fn a_crash_in_a_sync_is_recorded_and_does_not_block_the_next() {
    let (base, portal, stop) = start_portal().await;
    seed(&base, &portal);
    let store: Arc<dyn DocsStore> = Arc::new(Exploding(MemoryStore::default()));
    let rt = Runtime::new(store.clone(), syncer(&base, store));
    assert!(rt.sync_once().await);
    assert!(!rt.is_running(), "the flag is cleared");
    let last = rt.last.lock().unwrap().clone();
    assert!(
        matches!(&last, Some(Err(e)) if e.contains("stopped unexpectedly")),
        "{last:?}"
    );
    // The next run starts (and crashes the same way) instead of being skipped forever.
    assert!(rt.sync_once().await);
    stop.cancel();
}

#[tokio::test]
async fn readyz_fails_on_an_empty_or_stale_index() {
    let store: Arc<dyn DocsStore> = Arc::new(MemoryStore::default());
    let runtime = Arc::new(Runtime::new(
        store.clone(),
        syncer("http://127.0.0.1:9", store.clone()),
    ));
    let app = pmos_docs::app::router(
        runtime,
        pmos_docs::app::AppConfig {
            token: "t".repeat(32),
            name: "the public docs".into(),
            site: Some(pmos_docs::page::Site::new("https://docs.example.com")),
            products: pmos_docs::page::Products::default(),
            allowed_hosts: vec!["127.0.0.1".into()],
            stale_after: Some(DAY * 2),
        },
        tokio_util::sync::CancellationToken::new(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let ready = || async {
        let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
        (
            res.status().as_u16(),
            res.json::<serde_json::Value>().await.unwrap(),
        )
    };

    let (status, body) = ready().await;
    assert_eq!(status, 503);
    assert_eq!(body["checks"]["pages"], "down");

    // Pages, but synced three days ago: stale.
    store
        .upsert(&DocPage {
            url: "https://docs.example.com/docs/x.md".into(),
            slug: "x".into(),
            title: "X".into(),
            section: "docs".into(),
            product: "general".into(),
            description: None,
            headings: String::new(),
            body: "x".into(),
            updated: None,
            etag: None,
            fetched_at: Utc::now(),
        })
        .await
        .unwrap();
    store
        .record_sync(
            Utc::now() - chrono::Duration::days(3),
            true,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    let (status, body) = ready().await;
    assert_eq!(status, 503);
    assert_eq!(body["checks"]["fresh"], "down");

    store
        .record_sync(Utc::now(), true, &serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(ready().await.0, 200);
}
