//! Approvals for writes outside PM OS (OUT-4).
//!
//! A write tool call runs only if it matches a **grant**: the PM approved exactly this call
//! (same session, same tool, same arguments) in the harness, and the orchestrator recorded
//! it here. Grants are single-use and short-lived. A write with no grant never runs; it
//! becomes a pending **approval item** in the PM's inbox, and runs only when its own user
//! approves it there.

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// How long a grant stays valid after the PM approves.
pub const GRANT_TTL_MINUTES: i64 = 15;

/// SHA-256 of the canonical JSON form of `args` (object keys sorted at every level).
pub fn args_hash(args: &Value) -> String {
    let canonical = serde_json::to_string(&canonicalize(args)).expect("json value serializes");
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> =
                map.iter().map(|(k, v)| (k, canonicalize(v))).collect();
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

#[derive(Debug, Clone)]
struct Grant {
    session_token: String,
    tool: String,
    args_hash: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Approved,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalItem {
    pub id: String,
    pub user: String,
    pub skill: String,
    pub run_id: String,
    /// Registry name, such as `confluence.create_draft`.
    pub tool: String,
    pub args: Value,
    pub args_hash: String,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Edit,
    Reject,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecideError {
    #[error("approval item not found")]
    NotFound,
    #[error("only the item's own user can decide it")]
    Forbidden,
    #[error("approval item is already decided")]
    NotPending,
    #[error("an edit decision needs the edited arguments")]
    MissingArgs,
}

#[derive(Default)]
pub struct ApprovalStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    grants: Vec<Grant>,
    items: BTreeMap<String, ApprovalItem>,
}

pub struct NewItem<'a> {
    pub user: &'a str,
    pub skill: &'a str,
    pub run_id: &'a str,
    pub tool: &'a str,
    pub args: &'a Value,
}

impl ApprovalStore {
    pub fn add_grant(&self, session_token: &str, tool: &str, args: &Value, now: DateTime<Utc>) {
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        state.grants.push(Grant {
            session_token: session_token.to_string(),
            tool: tool.to_string(),
            args_hash: args_hash(args),
            expires_at: now + Duration::minutes(GRANT_TTL_MINUTES),
        });
    }

    /// Consume a grant matching this exact call. Returns whether one existed.
    pub fn take_grant(
        &self,
        session_token: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> bool {
        let hash = args_hash(args);
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        let found = state.grants.iter().position(|g| {
            g.session_token == session_token && g.tool == tool && g.args_hash == hash
        });
        match found {
            Some(i) => {
                state.grants.swap_remove(i);
                true
            }
            None => false,
        }
    }

    pub fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> ApprovalItem {
        let item = ApprovalItem {
            id: format!("apr_{}", uuid::Uuid::new_v4().simple()),
            user: new.user.to_string(),
            skill: new.skill.to_string(),
            run_id: new.run_id.to_string(),
            tool: new.tool.to_string(),
            args: new.args.clone(),
            args_hash: args_hash(new.args),
            status: Status::Pending,
            created_at: now,
            decided_at: None,
            decided_by: None,
            result: None,
        };
        self.state
            .lock()
            .expect("approvals lock")
            .items
            .insert(item.id.clone(), item.clone());
        item
    }

    pub fn get(&self, id: &str) -> Option<ApprovalItem> {
        self.state
            .lock()
            .expect("approvals lock")
            .items
            .get(id)
            .cloned()
    }

    /// Items for one user, newest first, optionally filtered by status.
    pub fn list(&self, user: &str, status: Option<Status>) -> Vec<ApprovalItem> {
        let state = self.state.lock().expect("approvals lock");
        let mut items: Vec<_> = state
            .items
            .values()
            .filter(|i| i.user == user && status.is_none_or(|s| i.status == s))
            .cloned()
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.created_at));
        items
    }

    /// Record a decision. On approve or edit, returns the item with the arguments to run;
    /// the caller runs the write and then calls [`ApprovalStore::complete`].
    pub fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut state = self.state.lock().expect("approvals lock");
        let item = state.items.get_mut(id).ok_or(DecideError::NotFound)?;
        if item.user != decided_by {
            return Err(DecideError::Forbidden);
        }
        if item.status != Status::Pending {
            return Err(DecideError::NotPending);
        }
        match decision {
            Decision::Reject => item.status = Status::Rejected,
            Decision::Approve => item.status = Status::Approved,
            Decision::Edit => {
                let args = edited_args.ok_or(DecideError::MissingArgs)?;
                item.args_hash = args_hash(&args);
                item.args = args;
                item.status = Status::Approved;
            }
        }
        item.decided_at = Some(now);
        item.decided_by = Some(decided_by.to_string());
        Ok(item.clone())
    }

    /// Store the outcome of an approved write.
    pub fn complete(&self, id: &str, result: Result<Value, String>) -> Option<ApprovalItem> {
        let mut state = self.state.lock().expect("approvals lock");
        let item = state.items.get_mut(id)?;
        match result {
            Ok(value) => item.result = Some(value),
            Err(message) => {
                item.status = Status::Failed;
                item.result = Some(serde_json::json!({ "error": message }));
            }
        }
        Some(item.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn hash_ignores_key_order() {
        let a = json!({"title": "PRD", "space": {"key": "PM", "id": 1}});
        let b = json!({"space": {"id": 1, "key": "PM"}, "title": "PRD"});
        assert_eq!(args_hash(&a), args_hash(&b));
        assert_ne!(args_hash(&a), args_hash(&json!({"title": "PRD2"})));
    }

    #[test]
    fn grant_matches_once_with_same_args() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let args = json!({"summary": "Add SSO"});
        store.add_grant("tok", "jira.create_issue", &args, now);

        assert!(!store.take_grant(
            "tok",
            "jira.create_issue",
            &json!({"summary": "Other"}),
            now
        ));
        assert!(!store.take_grant("other", "jira.create_issue", &args, now));
        assert!(!store.take_grant("tok", "slack.send_message", &args, now));
        assert!(store.take_grant("tok", "jira.create_issue", &args, now));
        assert!(
            !store.take_grant("tok", "jira.create_issue", &args, now),
            "single use"
        );
    }

    #[test]
    fn grant_expires() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let args = json!({"summary": "Add SSO"});
        store.add_grant("tok", "jira.create_issue", &args, now);
        let later = now + Duration::minutes(GRANT_TTL_MINUTES);
        assert!(!store.take_grant("tok", "jira.create_issue", &args, later));
    }

    fn item(store: &ApprovalStore, now: DateTime<Utc>) -> ApprovalItem {
        store.create(
            NewItem {
                user: "ana",
                skill: "prd-draft",
                run_id: "run-1",
                tool: "confluence.create_draft",
                args: &json!({"title": "PRD"}),
            },
            now,
        )
    }

    #[test]
    fn only_owner_decides_once() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let it = item(&store, now);
        assert_eq!(
            store.decide(&it.id, "bo", Decision::Approve, None, now),
            Err(DecideError::Forbidden)
        );
        let decided = store
            .decide(&it.id, "ana", Decision::Approve, None, now)
            .unwrap();
        assert_eq!(decided.status, Status::Approved);
        assert_eq!(
            store.decide(&it.id, "ana", Decision::Reject, None, now),
            Err(DecideError::NotPending)
        );
        assert_eq!(
            store.decide("apr_missing", "ana", Decision::Approve, None, now),
            Err(DecideError::NotFound)
        );
    }

    #[test]
    fn edit_replaces_args() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let it = item(&store, now);
        assert_eq!(
            store.decide(&it.id, "ana", Decision::Edit, None, now),
            Err(DecideError::MissingArgs)
        );
        let edited = json!({"title": "PRD v2"});
        let decided = store
            .decide(&it.id, "ana", Decision::Edit, Some(edited.clone()), now)
            .unwrap();
        assert_eq!(decided.args, edited);
        assert_eq!(decided.args_hash, args_hash(&edited));
    }

    #[test]
    fn list_filters_by_user_and_status() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let a = item(&store, now);
        let _b = item(&store, now + Duration::minutes(1));
        store
            .decide(&a.id, "ana", Decision::Reject, None, now)
            .unwrap();
        assert_eq!(store.list("ana", None).len(), 2);
        assert_eq!(store.list("ana", Some(Status::Pending)).len(), 1);
        assert!(store.list("bo", None).is_empty());
    }

    #[test]
    fn failed_write_is_recorded() {
        let store = ApprovalStore::default();
        let now = t("2026-11-14T10:00:00Z");
        let it = item(&store, now);
        store
            .decide(&it.id, "ana", Decision::Approve, None, now)
            .unwrap();
        let done = store
            .complete(&it.id, Err("space not found".into()))
            .unwrap();
        assert_eq!(done.status, Status::Failed);
    }
}
