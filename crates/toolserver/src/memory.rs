//! Per-user memory (MEM-1). Each user reads and writes only their own notes: in memory
//! ([`InMemoryStore`]), in Postgres (`pg.rs`), or, with `PMOS_MEMORY=github`, in the PM's own
//! private repo in the GitHub org (`github_memory.rs`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub key: String,
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

/// Why memory couldn't be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The PM must act first: set up a memory repo, link GitHub, or fix the repo. The text
    /// is shown to the model as the tool error.
    #[error("{0}")]
    Refused(String),
    /// The memory's home (GitHub) didn't answer or refused for a reason of its own.
    #[error("{0}")]
    Unavailable(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// The user's notes, by key.
    async fn read(&self, user: &str) -> MemoryResult<Vec<Note>>;
    /// Create or replace one note. `source` names who wrote it: the bot or skill and the run
    /// id, such as `pm-ana, task 0d3f…`. Only the GitHub store records it.
    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        source: &str,
        now: DateTime<Utc>,
    ) -> MemoryResult<Note>;
}

#[derive(Default)]
pub struct InMemoryStore {
    notes: Mutex<HashMap<String, BTreeMap<String, Note>>>,
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn read(&self, user: &str) -> MemoryResult<Vec<Note>> {
        Ok(self
            .notes
            .lock()
            .expect("memory lock")
            .get(user)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        _source: &str,
        now: DateTime<Utc>,
    ) -> MemoryResult<Note> {
        let note = Note {
            key: key.to_string(),
            text: text.to_string(),
            updated_at: now,
        };
        self.notes
            .lock()
            .expect("memory lock")
            .entry(user.to_string())
            .or_default()
            .insert(key.to_string(), note.clone());
        Ok(note)
    }
}

/// Memory keys are short slugs, so a key can be shown as `/memories/<key>.md` in the harness.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn users_see_only_their_notes() {
        let store = InMemoryStore::default();
        let now: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        store
            .update("ana", "areas", "Acme Cloud", "t", now)
            .await
            .unwrap();
        store
            .update("ana", "areas", "Acme Cloud, Acme Vault", "t", now)
            .await
            .unwrap();
        store
            .update("bo", "style", "short", "t", now)
            .await
            .unwrap();
        let ana = store.read("ana").await.unwrap();
        assert_eq!(ana.len(), 1);
        assert_eq!(ana[0].text, "Acme Cloud, Acme Vault");
        assert!(store.read("cy").await.unwrap().is_empty());
    }

    #[test]
    fn keys() {
        assert!(valid_key("areas-owned"));
        assert!(!valid_key("../x"));
        assert!(!valid_key(""));
    }
}
