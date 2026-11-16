//! Where accounts and sessions live: in memory (`PMOS_DEV=1`, tests) or in Postgres
//! (`pg.rs`). Both pass the same contract tests (`tests/store.rs`).
//!
//! The rules that must hold under concurrency are enforced here, inside one lock or one
//! transaction, rather than read-then-write in the service:
//! - the failed-attempt count is incremented atomically, so parallel guesses can't share one
//!   count and slip past the lockout;
//! - the last active admin can't lose the role or be disabled, even when two admins demote
//!   each other at the same moment.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pmos_contract::model::Role;
use uuid::Uuid;

use crate::model::{normalize_roles, User, UserStatus};
use crate::password::LockoutPolicy;
use crate::session::Session;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("that handle is taken")]
    HandleTaken,
    #[error("no such user")]
    NotFound,
    #[error("the last active admin can't lose the admin role or be disabled")]
    LastAdmin,
    #[error("storage error: {0}")]
    Backend(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Backend(e.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// The count and lock after a wrong password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub failed_attempts: u32,
    pub locked_until: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait UserStore: Send + Sync {
    /// Add an account. [`StoreError::HandleTaken`] if the handle exists.
    async fn create_user(&self, user: &User) -> StoreResult<()>;
    async fn user_by_id(&self, id: Uuid) -> StoreResult<Option<User>>;
    async fn user_by_handle(&self, handle: &str) -> StoreResult<Option<User>>;
    async fn count_active_admins(&self) -> StoreResult<u64>;
    /// Count one more wrong password and, past the policy's threshold, lock the account.
    /// Atomic.
    async fn record_failure(
        &self,
        id: Uuid,
        at: DateTime<Utc>,
        policy: &LockoutPolicy,
    ) -> StoreResult<Failure>;
    /// A successful sign-in: clear the count and any lock.
    async fn record_success(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<()>;
    /// A new password hash. Clears the count and any lock; sets `must_change_password`.
    async fn set_password(
        &self,
        id: Uuid,
        hash: &str,
        must_change: bool,
        at: DateTime<Utc>,
    ) -> StoreResult<User>;
    /// The same password, rehashed with current parameters. Changes nothing else.
    async fn upgrade_hash(&self, id: Uuid, hash: &str) -> StoreResult<()>;
    /// Replace the roles. [`StoreError::LastAdmin`] if that would leave no active admin.
    async fn set_roles(&self, id: Uuid, roles: &[Role], at: DateTime<Utc>) -> StoreResult<User>;
    /// [`StoreError::LastAdmin`] if disabling would leave no active admin.
    async fn set_status(
        &self,
        id: Uuid,
        status: UserStatus,
        at: DateTime<Utc>,
    ) -> StoreResult<User>;
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn insert_session(&self, session: &Session) -> StoreResult<()>;
    async fn session(&self, id_hash: &str) -> StoreResult<Option<Session>>;
    async fn touch_session(&self, id_hash: &str, at: DateTime<Utc>) -> StoreResult<()>;
    async fn delete_session(&self, id_hash: &str) -> StoreResult<()>;
    /// Sign a person out everywhere. Returns how many sessions ended.
    async fn delete_user_sessions(&self, user_id: Uuid) -> StoreResult<u64>;
    /// Drop sessions past their absolute expiry or idle since before `idle_before`.
    async fn purge_expired(
        &self,
        now: DateTime<Utc>,
        idle_before: DateTime<Utc>,
    ) -> StoreResult<u64>;
}

// ----- In memory ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Inner {
    users: HashMap<Uuid, User>,
    sessions: HashMap<String, Session>,
}

impl Inner {
    fn user_mut(&mut self, id: Uuid) -> StoreResult<&mut User> {
        self.users.get_mut(&id).ok_or(StoreError::NotFound)
    }

    fn other_active_admins(&self, id: Uuid) -> usize {
        self.users
            .values()
            .filter(|u| u.id != id && u.is_active_admin())
            .count()
    }
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl UserStore for MemoryStore {
    async fn create_user(&self, user: &User) -> StoreResult<()> {
        let mut inner = self.lock();
        if inner.users.values().any(|u| u.handle == user.handle) {
            return Err(StoreError::HandleTaken);
        }
        let mut user = user.clone();
        normalize_roles(&mut user.roles);
        inner.users.insert(user.id, user);
        Ok(())
    }

    async fn user_by_id(&self, id: Uuid) -> StoreResult<Option<User>> {
        Ok(self.lock().users.get(&id).cloned())
    }

    async fn user_by_handle(&self, handle: &str) -> StoreResult<Option<User>> {
        Ok(self
            .lock()
            .users
            .values()
            .find(|u| u.handle == handle)
            .cloned())
    }

    async fn count_active_admins(&self) -> StoreResult<u64> {
        Ok(self
            .lock()
            .users
            .values()
            .filter(|u| u.is_active_admin())
            .count() as u64)
    }

    async fn record_failure(
        &self,
        id: Uuid,
        at: DateTime<Utc>,
        policy: &LockoutPolicy,
    ) -> StoreResult<Failure> {
        let mut inner = self.lock();
        let u = inner.user_mut(id)?;
        u.failed_attempts = u.failed_attempts.saturating_add(1);
        if let Some(until) = policy.lock_after(u.failed_attempts, at) {
            u.locked_until = Some(until);
        }
        Ok(Failure {
            failed_attempts: u.failed_attempts,
            locked_until: u.locked_until,
        })
    }

    async fn record_success(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<()> {
        let mut inner = self.lock();
        let u = inner.user_mut(id)?;
        u.failed_attempts = 0;
        u.locked_until = None;
        u.last_login_at = Some(at);
        Ok(())
    }

    async fn set_password(
        &self,
        id: Uuid,
        hash: &str,
        must_change: bool,
        at: DateTime<Utc>,
    ) -> StoreResult<User> {
        let mut inner = self.lock();
        let u = inner.user_mut(id)?;
        u.password_hash = hash.to_string();
        u.must_change_password = must_change;
        u.failed_attempts = 0;
        u.locked_until = None;
        u.password_changed_at = at;
        u.updated_at = at;
        Ok(u.clone())
    }

    async fn upgrade_hash(&self, id: Uuid, hash: &str) -> StoreResult<()> {
        self.lock().user_mut(id)?.password_hash = hash.to_string();
        Ok(())
    }

    async fn set_roles(&self, id: Uuid, roles: &[Role], at: DateTime<Utc>) -> StoreResult<User> {
        let mut inner = self.lock();
        let others = inner.other_active_admins(id);
        let u = inner.user_mut(id)?;
        if u.is_active_admin() && !roles.contains(&Role::Admin) && others == 0 {
            return Err(StoreError::LastAdmin);
        }
        u.roles = roles.to_vec();
        normalize_roles(&mut u.roles);
        u.updated_at = at;
        Ok(u.clone())
    }

    async fn set_status(
        &self,
        id: Uuid,
        status: UserStatus,
        at: DateTime<Utc>,
    ) -> StoreResult<User> {
        let mut inner = self.lock();
        let others = inner.other_active_admins(id);
        let u = inner.user_mut(id)?;
        if u.is_active_admin() && status != UserStatus::Active && others == 0 {
            return Err(StoreError::LastAdmin);
        }
        u.status = status;
        u.updated_at = at;
        Ok(u.clone())
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn insert_session(&self, session: &Session) -> StoreResult<()> {
        self.lock()
            .sessions
            .insert(session.id_hash.clone(), session.clone());
        Ok(())
    }

    async fn session(&self, id_hash: &str) -> StoreResult<Option<Session>> {
        Ok(self.lock().sessions.get(id_hash).cloned())
    }

    async fn touch_session(&self, id_hash: &str, at: DateTime<Utc>) -> StoreResult<()> {
        if let Some(s) = self.lock().sessions.get_mut(id_hash) {
            s.last_seen_at = s.last_seen_at.max(at);
        }
        Ok(())
    }

    async fn delete_session(&self, id_hash: &str) -> StoreResult<()> {
        self.lock().sessions.remove(id_hash);
        Ok(())
    }

    async fn delete_user_sessions(&self, user_id: Uuid) -> StoreResult<u64> {
        let mut inner = self.lock();
        let before = inner.sessions.len();
        inner.sessions.retain(|_, s| s.user_id != user_id);
        Ok((before - inner.sessions.len()) as u64)
    }

    async fn purge_expired(
        &self,
        now: DateTime<Utc>,
        idle_before: DateTime<Utc>,
    ) -> StoreResult<u64> {
        let mut inner = self.lock();
        let before = inner.sessions.len();
        inner
            .sessions
            .retain(|_, s| s.expires_at > now && s.last_seen_at > idle_before);
        Ok((before - inner.sessions.len()) as u64)
    }
}
