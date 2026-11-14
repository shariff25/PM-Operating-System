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
use pmos_docs::sync::{SyncConfig, Syncer};

fn syncer(base: &str, store: Arc<dyn DocsStore>) -> Syncer {
    Syncer::new(
        SyncConfig {
            base: base.into(),
            delay: Duration::from_millis(1),
            ..SyncConfig::default()
        },
        store,
    )
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
