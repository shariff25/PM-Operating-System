//! Storage seams. Each store is an async trait with an in-memory implementation (tests and
//! `PMOS_DEV=1`) and a Postgres one (`pg.rs`). The shared contract tests in
//! `tests/stores.rs` run the same cases against both.

use std::sync::Arc;

use crate::approvals::{ApprovalRepo, MemoryApprovals};
use crate::audit::{AuditSink, MemoryAudit};
use crate::context::{ContextIndex, MemoryIndex};
use crate::members::{MemberRequestStore, MemoryRequests};
use crate::memory::{InMemoryStore, MemoryStore};
use crate::memory_repo::{MemoryRepoStore, MemoryRepos};
use crate::session::{MemorySessions, SessionRepo};
use crate::team::{MemoryTeams, TeamStore};
use crate::users::{MemoryUsers, UserStore};
use crate::vault::{CredentialStore, MemoryCredentials};

/// A storage failure: the database is down, a query failed, or a row didn't decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("storage error: {0}")]
pub struct StoreError(pub String);

pub type StoreResult<T> = Result<T, StoreError>;

/// Every store the tool server uses.
#[derive(Clone)]
pub struct Stores {
    pub context: Arc<dyn ContextIndex>,
    pub memory: Arc<dyn MemoryStore>,
    pub sessions: Arc<dyn SessionRepo>,
    pub approvals: Arc<dyn ApprovalRepo>,
    pub audit: Arc<dyn AuditSink>,
    /// Sealed OAuth credentials; see `vault.rs`.
    pub credentials: Arc<dyn CredentialStore>,
    /// Which GitHub repo holds each PM's memory; see `memory_repo.rs`.
    pub memory_repos: Arc<dyn MemoryRepoStore>,
    /// Requests to join PM OS; see `members.rs`.
    pub member_requests: Arc<dyn MemberRequestStore>,
    /// The user database (`users/`): the tool server's source of users and roles since slice
    /// U3 (`users::access::People`).
    pub users: Arc<dyn UserStore>,
    /// Each PM's team of marketplace bots; see `team.rs`.
    pub teams: Arc<dyn TeamStore>,
}

impl Stores {
    /// In-memory stores with the context library loaded from `content_root`. Returns the
    /// audit sink too, so tests can read its events.
    pub fn in_memory(content_root: &std::path::Path) -> (Self, Arc<MemoryAudit>) {
        let audit = Arc::new(MemoryAudit::default());
        let sessions: Arc<dyn SessionRepo> = Arc::new(MemorySessions::default());
        let approvals: Arc<dyn ApprovalRepo> = Arc::new(MemoryApprovals::default());
        let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentials::default());
        let mut stores = Self {
            context: Arc::new(MemoryIndex::load(content_root)),
            memory: Arc::new(InMemoryStore::default()),
            sessions,
            approvals,
            audit: audit.clone(),
            credentials,
            memory_repos: Arc::new(MemoryRepos::default()),
            member_requests: Arc::new(MemoryRequests::default()),
            users: Arc::new(MemoryUsers::default()),
            teams: Arc::new(MemoryTeams::default().with_audit(audit.clone())),
        };
        stores.users = Arc::new(
            MemoryUsers::default()
                .with_audit(audit.clone())
                .with_linked(stores.linked()),
        );
        (stores, audit)
    }

    /// The stores a deactivation and a reactivation reach (an in-memory user store's
    /// [`crate::users::memory::LinkedStores`]).
    pub fn linked(&self) -> crate::users::memory::LinkedStores {
        crate::users::memory::LinkedStores {
            sessions: self.sessions.clone(),
            credentials: self.credentials.clone(),
            approvals: self.approvals.clone(),
        }
    }
}
