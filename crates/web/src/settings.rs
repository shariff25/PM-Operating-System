//! What the web app keeps per PM: hints seen, nudges, whether they have opened a draft yet,
//! pending draft edits, change suggestions and the practice draft.
//!
//! **In memory for now.** [`MemorySettingsStore`] loses everything on restart: a PM's
//! unsaved edits, suggestions and practice draft go, and hints show again. That is
//! acceptable while the web app is in development and nothing sent upstream depends on it;
//! an edit only reaches the target system when the PM approves.
//!
//! A Postgres store needs:
//! - a `web_user_settings (handle primary key, hints_seen text[], nudges bool,
//!   opened_a_draft bool, updated_at)` table;
//! - `web_draft_edits (handle, draft_id, title, body, updated_at, primary key (handle,
//!   draft_id))`, deleted when the draft is decided;
//! - `web_suggestions (handle, draft_id, task_id, instruction, status, body, error, …)`;
//! - `web_practice_drafts (handle primary key, id, status, created_at, decided_at)`;
//! - `web_draft_flags (handle, draft_id, external bool)` and `web_revision_tasks (handle,
//!   task_id)`;
//! - [`SettingsStore::update`] as one transaction with `SELECT … FOR UPDATE` on the
//!   settings row, so two tabs can't both start a change (the in-memory store holds a lock);
//! - the same retention as the tool server for Slack draft text: an edit of a Slack draft is
//!   Slack text and must be deleted with it (24 hours).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

pub use crate::session::StoreError;

/// A PM's saved edit of a draft. Nothing is sent until they approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftEdit {
    pub title: Option<String>,
    pub body: String,
}

/// A requested change: the bot's task, then its revised text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSuggestion {
    /// The runtime task revising the draft; `None` for a practice draft.
    pub task_id: Option<String>,
    pub instruction: String,
    /// `working`, `ready` or `failed`.
    pub status: String,
    pub body: Option<String>,
    pub error: Option<String>,
}

/// The practice draft: lives only here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PracticeDraft {
    pub id: String,
    /// `pending`, `approved` or `rejected`.
    pub status: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserState {
    pub hints_seen: BTreeSet<String>,
    pub nudges: bool,
    /// `first_visit` is `!opened_a_draft`.
    pub opened_a_draft: bool,
    /// By draft id.
    pub edits: BTreeMap<String, DraftEdit>,
    /// By draft id.
    pub suggestions: BTreeMap<String, StoredSuggestion>,
    pub practice: Option<PracticeDraft>,
    /// Drafts the tool server said post to a channel shared outside the company
    /// (`needs_external_confirm`).
    pub external: BTreeSet<String>,
    /// Tasks this backend made to revise drafts; hidden from the PM's task list.
    pub revision_tasks: BTreeSet<String>,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            hints_seen: BTreeSet::new(),
            nudges: true,
            opened_a_draft: false,
            edits: BTreeMap::new(),
            suggestions: BTreeMap::new(),
            practice: None,
            external: BTreeSet::new(),
            revision_tasks: BTreeSet::new(),
        }
    }
}

impl UserState {
    /// Forget what was kept for a draft once it is decided or gone.
    pub fn forget_draft(&mut self, id: &str) {
        self.edits.remove(id);
        self.suggestions.remove(id);
    }
}

/// A change to one PM's state, applied atomically.
pub type Update<'a> = Box<dyn FnOnce(&mut UserState) + Send + 'a>;

#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// The PM's state; the defaults for someone not seen before.
    async fn get(&self, handle: &str) -> Result<UserState, StoreError>;

    /// Apply `change` atomically and return the new state.
    async fn update<'a>(
        &'a self,
        handle: &'a str,
        change: Update<'a>,
    ) -> Result<UserState, StoreError>;
}

#[derive(Default)]
pub struct MemorySettingsStore {
    users: Mutex<HashMap<String, UserState>>,
}

impl MemorySettingsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SettingsStore for MemorySettingsStore {
    async fn get(&self, handle: &str) -> Result<UserState, StoreError> {
        Ok(self
            .users
            .lock()
            .expect("settings lock")
            .get(handle)
            .cloned()
            .unwrap_or_default())
    }

    async fn update<'a>(
        &'a self,
        handle: &'a str,
        change: Update<'a>,
    ) -> Result<UserState, StoreError> {
        let mut users = self.users.lock().expect("settings lock");
        let state = users.entry(handle.to_string()).or_default();
        change(state);
        Ok(state.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keeps_each_pms_state_apart() {
        let store = MemorySettingsStore::new();
        assert!(store.get("ana").await.unwrap().nudges);
        let ana = store
            .update(
                "ana",
                Box::new(|s| {
                    s.nudges = false;
                    s.hints_seen.insert("ask".into());
                }),
            )
            .await
            .unwrap();
        assert!(!ana.nudges);
        assert_eq!(store.get("ana").await.unwrap(), ana);
        assert_eq!(store.get("ben").await.unwrap(), UserState::default());
    }
}
