//! Requests to join PM OS (`docs/onboarding-spec.md` §3). Someone not in the user database asks in
//! Slack; a platform owner approves or rejects there. In memory ([`MemoryRequests`]) or in
//! Postgres (`pg.rs`). At most one request per Slack id is pending at a time.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::store::StoreResult;

/// More pending requests than this are refused, so a flood can't bury the admins' DMs.
pub const MAX_PENDING: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Pending,
    Approved,
    Rejected,
    /// Approved, but the commit to the content repo failed; `error` says why.
    Failed,
}

impl RequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberRequest {
    pub id: String,
    pub slack_id: String,
    pub email: String,
    pub name: String,
    pub proposed_handle: String,
    pub status: RequestStatus,
    pub requested_at: DateTime<Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    /// The handle granted, once approved.
    pub handle: Option<String>,
    /// The content-repo commit that added the member.
    pub commit: Option<String>,
    pub error: Option<String>,
}

pub struct NewRequest<'a> {
    pub slack_id: &'a str,
    pub email: &'a str,
    pub name: &'a str,
    pub proposed_handle: &'a str,
}

impl NewRequest<'_> {
    pub fn into_request(self, now: DateTime<Utc>) -> MemberRequest {
        MemberRequest {
            id: uuid::Uuid::new_v4().to_string(),
            slack_id: self.slack_id.to_string(),
            email: self.email.to_string(),
            name: self.name.to_string(),
            proposed_handle: self.proposed_handle.to_string(),
            status: RequestStatus::Pending,
            requested_at: now,
            decided_by: None,
            decided_at: None,
            handle: None,
            commit: None,
            error: None,
        }
    }
}

/// What deciding a request records. `status` is never `Pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub status: RequestStatus,
    pub decided_by: String,
    pub handle: Option<String>,
    pub commit: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Created {
    New(MemberRequest),
    /// That Slack id already has a pending request; here it is.
    Existing(MemberRequest),
    /// [`MAX_PENDING`] requests are already pending.
    TooMany,
}

#[async_trait]
pub trait MemberRequestStore: Send + Sync {
    /// Add a pending request, unless one is already pending for that Slack id.
    async fn create(&self, new: NewRequest<'_>, now: DateTime<Utc>) -> StoreResult<Created>;
    async fn get(&self, id: &str) -> StoreResult<Option<MemberRequest>>;
    async fn pending_for(&self, slack_id: &str) -> StoreResult<Option<MemberRequest>>;
    /// Oldest first.
    async fn list(&self, status: Option<RequestStatus>) -> StoreResult<Vec<MemberRequest>>;
    /// Record the decision, only if the request is still pending. `None` if it isn't (or
    /// doesn't exist), so of two concurrent decisions exactly one wins.
    async fn decide(
        &self,
        id: &str,
        decided: &Decided,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<MemberRequest>>;
}

#[derive(Default)]
pub struct MemoryRequests {
    rows: Mutex<BTreeMap<String, MemberRequest>>,
}

fn oldest_first(mut v: Vec<MemberRequest>) -> Vec<MemberRequest> {
    v.sort_by(|a, b| (a.requested_at, &a.id).cmp(&(b.requested_at, &b.id)));
    v
}

#[async_trait]
impl MemberRequestStore for MemoryRequests {
    async fn create(&self, new: NewRequest<'_>, now: DateTime<Utc>) -> StoreResult<Created> {
        let mut rows = self.rows.lock().expect("member requests lock");
        let pending: Vec<&MemberRequest> = rows
            .values()
            .filter(|r| r.status == RequestStatus::Pending)
            .collect();
        if let Some(existing) = pending.iter().find(|r| r.slack_id == new.slack_id) {
            return Ok(Created::Existing((*existing).clone()));
        }
        if pending.len() >= MAX_PENDING {
            return Ok(Created::TooMany);
        }
        let request = new.into_request(now);
        rows.insert(request.id.clone(), request.clone());
        Ok(Created::New(request))
    }

    async fn get(&self, id: &str) -> StoreResult<Option<MemberRequest>> {
        Ok(self
            .rows
            .lock()
            .expect("member requests lock")
            .get(id)
            .cloned())
    }

    async fn pending_for(&self, slack_id: &str) -> StoreResult<Option<MemberRequest>> {
        Ok(self
            .rows
            .lock()
            .expect("member requests lock")
            .values()
            .find(|r| r.slack_id == slack_id && r.status == RequestStatus::Pending)
            .cloned())
    }

    async fn list(&self, status: Option<RequestStatus>) -> StoreResult<Vec<MemberRequest>> {
        let rows = self.rows.lock().expect("member requests lock");
        Ok(oldest_first(
            rows.values()
                .filter(|r| status.is_none_or(|s| r.status == s))
                .cloned()
                .collect(),
        ))
    }

    async fn decide(
        &self,
        id: &str,
        decided: &Decided,
        now: DateTime<Utc>,
    ) -> StoreResult<Option<MemberRequest>> {
        let mut rows = self.rows.lock().expect("member requests lock");
        match rows.get_mut(id) {
            Some(r) if r.status == RequestStatus::Pending => {
                r.status = decided.status;
                r.decided_by = Some(decided.decided_by.clone());
                r.decided_at = Some(now);
                r.handle = decided.handle.clone();
                r.commit = decided.commit.clone();
                r.error = decided.error.clone();
                Ok(Some(r.clone()))
            }
            _ => Ok(None),
        }
    }
}
