//! Per-user memory (MEM-1). Each user reads and writes only their own notes. The Postgres
//! store replaces [`InMemoryStore`] in 0c-2 behind the same trait.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub key: String,
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

pub trait MemoryStore: Send + Sync {
    fn read(&self, user: &str) -> Vec<Note>;
    fn update(&self, user: &str, key: &str, text: &str, now: DateTime<Utc>) -> Note;
}

#[derive(Default)]
pub struct InMemoryStore {
    notes: Mutex<HashMap<String, BTreeMap<String, Note>>>,
}

impl MemoryStore for InMemoryStore {
    fn read(&self, user: &str) -> Vec<Note> {
        self.notes
            .lock()
            .expect("memory lock")
            .get(user)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    fn update(&self, user: &str, key: &str, text: &str, now: DateTime<Utc>) -> Note {
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
        note
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

    #[test]
    fn users_see_only_their_notes() {
        let store = InMemoryStore::default();
        let now: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        store.update("ana", "areas", "CCM", now);
        store.update("ana", "areas", "CCM, DSM", now);
        store.update("bo", "style", "short", now);
        let ana = store.read("ana");
        assert_eq!(ana.len(), 1);
        assert_eq!(ana[0].text, "CCM, DSM");
        assert!(store.read("cy").is_empty());
    }

    #[test]
    fn keys() {
        assert!(valid_key("areas-owned"));
        assert!(!valid_key("../x"));
        assert!(!valid_key(""));
    }
}
