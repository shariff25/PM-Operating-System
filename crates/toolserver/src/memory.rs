//! Per-user memory (MEM-1). Each user reads and writes only their own notes, in memory
//! ([`InMemoryStore`]) or in Postgres (`pg.rs`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::store::StoreResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub key: String,
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// The user's notes, by key.
    async fn read(&self, user: &str) -> StoreResult<Vec<Note>>;
    /// Create or replace one note.
    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<Note>;
}

#[derive(Default)]
pub struct InMemoryStore {
    notes: Mutex<HashMap<String, BTreeMap<String, Note>>>,
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn read(&self, user: &str) -> StoreResult<Vec<Note>> {
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
        now: DateTime<Utc>,
    ) -> StoreResult<Note> {
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
        store.update("ana", "areas", "CCM", now).await.unwrap();
        store.update("ana", "areas", "CCM, DSM", now).await.unwrap();
        store.update("bo", "style", "short", now).await.unwrap();
        let ana = store.read("ana").await.unwrap();
        assert_eq!(ana.len(), 1);
        assert_eq!(ana[0].text, "CCM, DSM");
        assert!(store.read("cy").await.unwrap().is_empty());
    }

    #[test]
    fn keys() {
        assert!(valid_key("areas-owned"));
        assert!(!valid_key("../x"));
        assert!(!valid_key(""));
    }
}
