//! Which GitHub repo holds each PM's memory (`docs/onboarding-spec.md` §2): `user → repo`
//! (`org/name`), plus when the repo was last checked to be private, in the org, not
//! archived and pushable by the PM. In memory ([`MemoryRepos`]) or in Postgres (`pg.rs`).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::StoreResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryRepo {
    #[serde(skip)]
    pub user: String,
    /// `org/name`.
    pub repo: String,
    pub verified_at: DateTime<Utc>,
}

#[async_trait]
pub trait MemoryRepoStore: Send + Sync {
    async fn get(&self, user: &str) -> StoreResult<Option<MemoryRepo>>;
    /// Set or replace the user's record.
    async fn put(&self, record: &MemoryRepo) -> StoreResult<()>;
    /// Remove the user's record. The repo itself is theirs and is left alone.
    async fn delete(&self, user: &str) -> StoreResult<bool>;
}

#[derive(Default)]
pub struct MemoryRepos {
    rows: Mutex<HashMap<String, MemoryRepo>>,
}

#[async_trait]
impl MemoryRepoStore for MemoryRepos {
    async fn get(&self, user: &str) -> StoreResult<Option<MemoryRepo>> {
        Ok(self
            .rows
            .lock()
            .expect("memory repos lock")
            .get(user)
            .cloned())
    }

    async fn put(&self, record: &MemoryRepo) -> StoreResult<()> {
        self.rows
            .lock()
            .expect("memory repos lock")
            .insert(record.user.clone(), record.clone());
        Ok(())
    }

    async fn delete(&self, user: &str) -> StoreResult<bool> {
        Ok(self
            .rows
            .lock()
            .expect("memory repos lock")
            .remove(user)
            .is_some())
    }
}
