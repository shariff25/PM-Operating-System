//! What the web app keeps per PM: hints seen, nudges, the mascot, the picker's last bot, whether they have opened a draft yet,
//! the practice draft and its edit, and change suggestions. (A real draft's edit is saved on
//! the item at the tool server, so every surface shows it: `app.rs`, `save_edit`.)
//!
//! [`MemorySettingsStore`] loses everything on restart. [`crate::pg::PgStore`] keeps it in
//! `web_user_settings`, `web_draft_edits`, `web_suggestions`, `web_practice_drafts`,
//! `web_draft_flags` and `web_revision_tasks`, every row keyed by handle, with
//! [`SettingsStore::update`] one transaction holding a `FOR UPDATE` lock on the PM's settings
//! row, so two tabs can't both start a change.
//!
//! Retention: an edit or a suggestion is deleted when its draft is decided, and by
//! [`SettingsStore::purge`] once untouched for [`EDIT_RETENTION_DAYS`] (an edit may hold Slack
//! text, and the tool server drops a Slack draft's own text after 24 hours; the edit is
//! unreachable once its draft is gone, so this is a backstop, not the 24-hour rule).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use chrono::{DateTime, Utc};

pub use crate::session::StoreError;

/// How long an untouched edit or suggestion is kept in Postgres.
pub const EDIT_RETENTION_DAYS: i64 = 7;

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
    /// Whether the app draws Nudge, its mascot. Off keeps every hint and empty state's text.
    pub mascot: bool,
    /// `first_visit` is `!opened_a_draft`.
    pub opened_a_draft: bool,
    /// The practice draft's edit, by its id. Real drafts' edits live at the tool server; an
    /// entry kept from before that is ignored, and deleted with the draft's suggestion.
    pub edits: BTreeMap<String, DraftEdit>,
    /// By draft id.
    pub suggestions: BTreeMap<String, StoredSuggestion>,
    pub practice: Option<PracticeDraft>,
    /// Drafts the tool server said post to a channel shared outside the company
    /// (`needs_external_confirm`).
    pub external: BTreeSet<String>,
    /// Tasks this backend made to revise drafts; hidden from the PM's task list.
    pub revision_tasks: BTreeSet<String>,
    /// The bot the ask box's picker last asked (a persona key; `None` is Nudge). A preference
    /// only, never a decision: `GET /api/me` falls back to Nudge when it is no longer on the
    /// PM's team, and every ask is checked again.
    pub ask_persona: Option<String>,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            hints_seen: BTreeSet::new(),
            nudges: true,
            mascot: true,
            opened_a_draft: false,
            edits: BTreeMap::new(),
            suggestions: BTreeMap::new(),
            practice: None,
            external: BTreeSet::new(),
            revision_tasks: BTreeSet::new(),
            ask_persona: None,
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

    /// Delete edits and suggestions untouched for [`EDIT_RETENTION_DAYS`], and flags and
    /// revision-task marks older than 30 days; the number removed. A no-op in memory.
    async fn purge(&self, _now: DateTime<Utc>) -> Result<u64, StoreError> {
        Ok(0)
    }
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
