//! Approvals for writes outside PM OS (OUT-4).
//!
//! A write tool call runs only if it matches a **grant**: the PM approved exactly this call
//! (same session, same tool, same arguments) in the harness, and the orchestrator recorded
//! it here. Grants are single-use and short-lived. A write with no grant never runs; it
//! becomes a pending **approval item** in the PM's inbox, and runs only when its own user
//! approves it there.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::{StoreError, StoreResult};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for its user to decide.
    Pending,
    /// Approved; the write is running. An item left here (a crash mid-write) is resolved
    /// by the start-up reconciler as `failed`, outcome unknown.
    Executing,
    /// Approved, and the write succeeded; `result` holds what was written.
    Approved,
    Rejected,
    /// Approved, but the write failed or its outcome is unknown; `result` says which.
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "executing" => Some(Self::Executing),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
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
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct NewItem<'a> {
    pub user: &'a str,
    pub skill: &'a str,
    pub run_id: &'a str,
    pub tool: &'a str,
    pub args: &'a Value,
}

impl NewItem<'_> {
    pub fn into_item(self, now: DateTime<Utc>) -> ApprovalItem {
        ApprovalItem {
            id: format!("apr_{}", uuid::Uuid::new_v4().simple()),
            user: self.user.to_string(),
            skill: self.skill.to_string(),
            run_id: self.run_id.to_string(),
            tool: self.tool.to_string(),
            args: self.args.clone(),
            args_hash: args_hash(self.args),
            status: Status::Pending,
            created_at: now,
            decided_at: None,
            decided_by: None,
            result: None,
        }
    }
}

/// Grants and approval items. `session_key` is the session token's hash
/// ([`crate::session::token_hash`]), never the token itself.
#[async_trait]
pub trait ApprovalRepo: Send + Sync {
    /// Record that the PM approved this exact call; valid for [`GRANT_TTL_MINUTES`].
    async fn add_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<()>;

    /// Consume one live grant matching this exact call. Returns whether one existed. Two
    /// concurrent calls with one grant: exactly one gets `true`.
    async fn take_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<bool>;

    async fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> StoreResult<ApprovalItem>;

    async fn get(&self, id: &str) -> StoreResult<Option<ApprovalItem>>;

    /// Items for one user, newest first, optionally filtered by status.
    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>>;

    /// Record a decision. Only the item's own user may decide, and only once: of two
    /// concurrent decisions, exactly one succeeds. Approve and edit move the item to
    /// `executing` and return it with the arguments to run; the caller runs the write and then
    /// calls `complete`.
    async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError>;

    /// Store the outcome of an executing write: `approved` with the result, or `failed`
    /// with the error. Only an `executing` item changes; `None` otherwise.
    async fn complete(
        &self,
        id: &str,
        result: Result<Value, String>,
    ) -> StoreResult<Option<ApprovalItem>>;

    /// Items still `executing` that were decided at or before `before`: writes interrupted
    /// by a crash or restart.
    async fn stuck(&self, before: DateTime<Utc>) -> StoreResult<Vec<ApprovalItem>>;
}

#[derive(Debug, Clone)]
struct Grant {
    session_key: String,
    tool: String,
    args_hash: String,
    expires_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct MemoryApprovals {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    grants: Vec<Grant>,
    items: BTreeMap<String, ApprovalItem>,
}

#[async_trait]
impl ApprovalRepo for MemoryApprovals {
    async fn add_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<()> {
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        state.grants.push(Grant {
            session_key: session_key.to_string(),
            tool: tool.to_string(),
            args_hash: args_hash(args),
            expires_at: now + Duration::minutes(GRANT_TTL_MINUTES),
        });
        Ok(())
    }

    async fn take_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let hash = args_hash(args);
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        let found = state
            .grants
            .iter()
            .position(|g| g.session_key == session_key && g.tool == tool && g.args_hash == hash);
        Ok(match found {
            Some(i) => {
                state.grants.swap_remove(i);
                true
            }
            None => false,
        })
    }

    async fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> StoreResult<ApprovalItem> {
        let item = new.into_item(now);
        self.state
            .lock()
            .expect("approvals lock")
            .items
            .insert(item.id.clone(), item.clone());
        Ok(item)
    }

    async fn get(&self, id: &str) -> StoreResult<Option<ApprovalItem>> {
        Ok(self
            .state
            .lock()
            .expect("approvals lock")
            .items
            .get(id)
            .cloned())
    }

    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>> {
        let state = self.state.lock().expect("approvals lock");
        let mut items: Vec<_> = state
            .items
            .values()
            .filter(|i| i.user == user && status.is_none_or(|s| i.status == s))
            .cloned()
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.created_at));
        Ok(items)
    }

    async fn decide(
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
            Decision::Approve => item.status = Status::Executing,
            Decision::Edit => {
                let args = edited_args.ok_or(DecideError::MissingArgs)?;
                item.args_hash = args_hash(&args);
                item.args = args;
                item.status = Status::Executing;
            }
        }
        item.decided_at = Some(now);
        item.decided_by = Some(decided_by.to_string());
        Ok(item.clone())
    }

    async fn complete(
        &self,
        id: &str,
        result: Result<Value, String>,
    ) -> StoreResult<Option<ApprovalItem>> {
        let mut state = self.state.lock().expect("approvals lock");
        let Some(item) = state
            .items
            .get_mut(id)
            .filter(|i| i.status == Status::Executing)
        else {
            return Ok(None);
        };
        match result {
            Ok(value) => {
                item.status = Status::Approved;
                item.result = Some(value);
            }
            Err(message) => {
                item.status = Status::Failed;
                item.result = Some(serde_json::json!({ "error": message }));
            }
        }
        Ok(Some(item.clone()))
    }

    async fn stuck(&self, before: DateTime<Utc>) -> StoreResult<Vec<ApprovalItem>> {
        let state = self.state.lock().expect("approvals lock");
        Ok(state
            .items
            .values()
            .filter(|i| i.status == Status::Executing && i.decided_at.is_some_and(|d| d <= before))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hash_ignores_key_order() {
        let a = json!({"title": "PRD", "space": {"key": "PM", "id": 1}});
        let b = json!({"space": {"id": 1, "key": "PM"}, "title": "PRD"});
        assert_eq!(args_hash(&a), args_hash(&b));
        assert_ne!(args_hash(&a), args_hash(&json!({"title": "PRD2"})));
    }

    #[test]
    fn status_round_trips() {
        for s in [
            Status::Pending,
            Status::Executing,
            Status::Approved,
            Status::Rejected,
            Status::Failed,
        ] {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        assert_eq!(Status::parse("maybe"), None);
    }
}
