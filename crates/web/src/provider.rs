//! How a person proves who they are. Phase 1 has one provider, local handle and password;
//! Entra ID (OIDC) is added later as a second implementation of [`AuthProvider`]. A provider
//! only answers "which PM OS user is this?". Sessions, roles, CSRF and audit of the
//! sign-in itself are the same whichever provider answered ([`crate::service::Auth`]).
//!
//! Every failure looks the same to the caller ([`AuthFailure::InvalidCredentials`]): unknown
//! handle, wrong password, locked account and disabled account give one answer in about the
//! same time, so a sign-in form can't be used to learn who has an account. Which of these it
//! was goes to the audit trail only.

use std::sync::Arc;

use async_trait::async_trait;

use crate::audit::{AuditAction, AuditEvent, AuditOutcome, AuditSink, AuditTarget, ReasonCode};
use crate::clock::Clock;
use crate::model::User;
use crate::password::{Hasher, LockoutPolicy};
use crate::store::UserStore;

/// What the person presented. Later: `OidcCallback { code, state }` for Entra.
#[derive(Clone)]
pub enum Credentials {
    Password { handle: String, password: String },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Password { .. } => f.write_str("Credentials::Password(<redacted>)"),
        }
    }
}

/// Facts about the request a provider may record.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub ip: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthFailure {
    /// Any reason the credentials don't sign anyone in. Deliberately uninformative.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// The provider could not decide (store or hasher down, audit refused).
    #[error("sign-in is unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// A short stable name recorded with each session and sign-in event: `local`, `entra`.
    fn id(&self) -> &'static str;
    /// The active PM OS user these credentials belong to.
    async fn authenticate(
        &self,
        credentials: &Credentials,
        ctx: &RequestContext,
    ) -> Result<User, AuthFailure>;
}

/// Local accounts: a handle and an Argon2id-hashed password in the user store, with
/// per-account lockout.
pub struct LocalProvider {
    users: Arc<dyn UserStore>,
    hasher: Hasher,
    lockout: LockoutPolicy,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
}

impl LocalProvider {
    pub const ID: &'static str = "local";

    pub fn new(
        users: Arc<dyn UserStore>,
        hasher: Hasher,
        lockout: LockoutPolicy,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            users,
            hasher,
            lockout,
            audit,
            clock,
        }
    }

    async fn deny(&self, user: Option<&User>, reason: ReasonCode, ctx: &RequestContext) {
        let mut e = AuditEvent::new(
            self.clock.now(),
            AuditAction::SignInFailed,
            AuditOutcome::Denied,
        );
        e.reason_code = Some(reason);
        e.provider = Some(Self::ID);
        e.target = user.map(target);
        e.request.ip = ctx.ip.clone();
        self.record(e).await;
    }

    /// Failure events are best effort: an audit outage must not turn a wrong password into
    /// a different answer (that would leak which handles exist). It is logged loudly.
    async fn record(&self, e: AuditEvent) {
        if let Err(err) = self.audit.record(e).await {
            tracing::error!(error = %err, "could not record a sign-in failure event");
        }
    }
}

pub(crate) fn target(u: &User) -> AuditTarget {
    AuditTarget {
        kind: "user",
        id: u.id,
        handle: u.handle.clone(),
    }
}

fn unavailable(e: impl std::fmt::Display) -> AuthFailure {
    AuthFailure::Unavailable(e.to_string())
}

#[async_trait]
impl AuthProvider for LocalProvider {
    fn id(&self) -> &'static str {
        Self::ID
    }

    async fn authenticate(
        &self,
        credentials: &Credentials,
        ctx: &RequestContext,
    ) -> Result<User, AuthFailure> {
        let Credentials::Password { handle, password } = credentials;
        // Look up by the exact handle; a malformed one simply matches nobody.
        let user = self.users.user_by_handle(handle).await.map_err(unavailable)?;
        // Exactly one Argon2 verification on every path, so every answer takes as long.
        let matches = self
            .hasher
            .verify(password, user.as_ref().map(|u| u.password_hash.as_str()))
            .await
            .map_err(unavailable)?;
        let now = self.clock.now();
        let Some(user) = user else {
            self.deny(None, ReasonCode::UnknownUser, ctx).await;
            return Err(AuthFailure::InvalidCredentials);
        };
        if !user.is_active() {
            self.deny(Some(&user), ReasonCode::Disabled, ctx).await;
            return Err(AuthFailure::InvalidCredentials);
        }
        if user.is_locked(now) {
            // Not counted: attempts while locked can't extend the lock.
            self.deny(Some(&user), ReasonCode::Locked, ctx).await;
            return Err(AuthFailure::InvalidCredentials);
        }
        if !matches {
            let failure = self
                .users
                .record_failure(user.id, now, &self.lockout)
                .await
                .map_err(unavailable)?;
            self.deny(Some(&user), ReasonCode::WrongPassword, ctx).await;
            if failure.locked_until.is_some_and(|until| until > now) {
                let mut e = AuditEvent::new(now, AuditAction::Lockout, AuditOutcome::Denied);
                e.reason_code = Some(ReasonCode::WrongPassword);
                e.provider = Some(Self::ID);
                e.target = Some(target(&user));
                e.request.ip = ctx.ip.clone();
                self.record(e).await;
                tracing::warn!(
                    user = %user.handle,
                    failed_attempts = failure.failed_attempts,
                    "account locked after repeated wrong passwords"
                );
            }
            return Err(AuthFailure::InvalidCredentials);
        }
        self.users
            .record_success(user.id, now)
            .await
            .map_err(unavailable)?;
        if self.hasher.needs_rehash(&user.password_hash) {
            match self.hasher.hash(password).await {
                Ok(h) => {
                    if let Err(e) = self.users.upgrade_hash(user.id, &h).await {
                        tracing::warn!(error = %e, "could not upgrade a password hash");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "could not rehash a password"),
            }
        }
        let mut user = user;
        user.failed_attempts = 0;
        user.locked_until = None;
        user.last_login_at = Some(now);
        Ok(user)
    }
}
