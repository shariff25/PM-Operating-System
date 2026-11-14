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
}

impl Stores {
    /// In-memory stores with the context library loaded from `content_root`. Returns the
    /// audit sink too, so tests can read its events.
    pub fn in_memory(content_root: &std::path::Path) -> (Self, Arc<MemoryAudit>) {
        let audit = Arc::new(MemoryAudit::default());
        let stores = Self {
            context: Arc::new(MemoryIndex::load(content_root)),
            memory: Arc::new(InMemoryStore::default()),
            sessions: Arc::new(MemorySessions::default()),
            approvals: Arc::new(MemoryApprovals::default()),
            audit: audit.clone(),
            credentials: Arc::new(MemoryCredentials::default()),
            memory_repos: Arc::new(MemoryRepos::default()),
            member_requests: Arc::new(MemoryRequests::default()),
        };
        (stores, audit)
    }
}
