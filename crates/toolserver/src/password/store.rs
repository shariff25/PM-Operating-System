//! Where password sign-in keeps its state: [`PasswordStore`], implemented by the in-memory user
//! store (`users::memory`) and the Postgres one (`users::password_pg`, migrations 0020, 0022).
//!
//! **One person at a time, whole or not at all.** Every change goes through
//! [`PasswordStore::password_tx`]: the store loads one person's password state (with the
//! person's status and roles, an acting admin's, and the counters of the one address the
//! attempt comes from) under that person's lock, hands it to a plan (`super::plan`, plain Rust
//! shared by both stores), and writes what the plan says, its audit events and `role_changes`
//! rows included, in one transaction. Slow work (Argon2id) happens outside, before or between
//! such transactions, never while a lock is held.
//!
//! **`Debug` never shows a hash, a sealed secret or a token's hash**: the types that hold one
//! write their own (tested in `super::tests`).

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::limits::{Counter, SourceCounters};
use crate::audit::AuditEvent;
use crate::store::StoreResult;
use crate::users::{RoleChange, Status, UserRole, Via};

/// What the plan needs to know about a person (never an address but the sign-in name, which
/// is the enrolment's account label and a word a password may not be).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub handle: String,
    pub status: Status,
    pub roles: BTreeSet<UserRole>,
    pub sign_in_name: Option<String>,
    pub sign_in_name_confirmed: bool,
}

impl Person {
    pub fn active(&self) -> bool {
        self.status == Status::Active
    }

    /// Holds Admin or Auditor: their codes come from the host only.
    pub fn protected(&self) -> bool {
        self.roles.contains(&UserRole::Admin) || self.roles.contains(&UserRole::Auditor)
    }

    pub fn is_admin(&self) -> bool {
        self.active() && self.roles.contains(&UserRole::Admin)
    }
}

/// Someone issued a reset code for a person who already had a credential: shown to the person
/// until they dismiss it (the review of 2026-11-20, PM2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetNotice {
    /// The admin's handle, or `host`.
    pub by: String,
    /// `Web` (an admin) or `Host`.
    pub via: Via,
    /// From the host: the OS user who ran the command.
    pub host_user: Option<String>,
    pub at: DateTime<Utc>,
}

/// A person's password credential.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Credential {
    /// Argon2id PHC string; `None` until a setup finishes.
    pub password_hash: Option<String>,
    /// The authenticator secret, sealed by the vault for this person.
    pub totp_sealed: Option<String>,
    /// The last authenticator step accepted.
    pub totp_last_step: Option<i64>,
    /// Bumped by every setup, reset and change; a pmos-web session carries it.
    pub epoch: i64,
    /// Failed passwords and second-step codes from every address together (a count only:
    /// the waits are per address, [`SourceCounters`]).
    pub failed: Counter,
    /// Failed setup-code guesses from every address together (a count only).
    pub setup_failed: Counter,
    /// When the current password was set.
    pub set_at: Option<DateTime<Utc>>,
    /// Who issued the setup code this credential was set with: `Host`, or `Web` (an admin in
    /// the web app). `None` for one set before migration 0022 recorded it: treated as `Web`.
    /// A web-issued credential doesn't work for an Admin or Auditor (PM3).
    pub issued_via: Option<Via>,
    /// A reset code was issued for this person while they had a credential (PM2).
    pub reset_notice: Option<ResetNotice>,
}

fn redacted(v: &Option<String>) -> &'static str {
    if v.is_some() {
        "<redacted>"
    } else {
        "None"
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("password_hash", &redacted(&self.password_hash))
            .field("totp_sealed", &redacted(&self.totp_sealed))
            .field("totp_last_step", &self.totp_last_step)
            .field("epoch", &self.epoch)
            .field("failed", &self.failed)
            .field("setup_failed", &self.setup_failed)
            .field("set_at", &self.set_at)
            .field("issued_via", &self.issued_via)
            .field("reset_notice", &self.reset_notice)
            .finish()
    }
}

impl Credential {
    pub fn is_set(&self) -> bool {
        self.password_hash.is_some() && self.totp_sealed.is_some()
    }

    /// Was it set with a code from the host? (A credential from before migration 0022 counts
    /// as web-issued.)
    pub fn host_issued(&self) -> bool {
        self.issued_via == Some(Via::Host)
    }

    /// Set, but held because `person` holds Admin or Auditor and an admin in the web app issued
    /// it: refused at sign-in until a host-issued setup replaces it (PM3).
    pub fn held_for(&self, person: &Person) -> bool {
        self.is_set() && person.protected() && !self.host_issued()
    }
}

/// One recovery code, as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryCode {
    pub hash: String,
    pub used_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryCode")
            .field("hash", &"<redacted>")
            .field("used_at", &self.used_at)
            .finish()
    }
}

/// Whether a setup code sets a first credential or replaces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Setup,
    Reset,
}

impl CodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Reset => "reset",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "setup" => Some(Self::Setup),
            "reset" => Some(Self::Reset),
            _ => None,
        }
    }
}

/// A person's live setup code (not used, not replaced; maybe expired).
#[derive(Clone, PartialEq, Eq)]
pub struct SetupCode {
    pub id: i64,
    pub hash: String,
    pub kind: CodeKind,
    pub issued_at: DateTime<Utc>,
    pub issued_by: String,
    /// `Web` (an admin) or `Host`.
    pub issued_via: Via,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupCode")
            .field("id", &self.id)
            .field("hash", &"<redacted>")
            .field("kind", &self.kind)
            .field("issued_at", &self.issued_at)
            .field("issued_by", &self.issued_by)
            .field("issued_via", &self.issued_via)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A code to issue (replacing the live one).
#[derive(Clone, PartialEq, Eq)]
pub struct NewSetupCode {
    pub hash: String,
    pub kind: CodeKind,
    pub issued_by: String,
    pub issued_via: Via,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for NewSetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewSetupCode")
            .field("hash", &"<redacted>")
            .field("kind", &self.kind)
            .field("issued_by", &self.issued_by)
            .field("issued_via", &self.issued_via)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// What a pending token is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// After a correct password: the second step, and nothing else.
    SignIn,
    /// A setup (or reset) in progress.
    Setup,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::Setup => "setup",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sign_in" => Some(Self::SignIn),
            "setup" => Some(Self::Setup),
            _ => None,
        }
    }
}

/// A token between steps, as stored (its hash only).
#[derive(Clone, PartialEq, Eq)]
pub struct Pending {
    pub token_hash: String,
    pub purpose: Purpose,
    /// A setup token's code.
    pub setup_code_id: Option<i64>,
    /// The credential's epoch when it was made: a change meanwhile makes it worthless.
    pub epoch: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,
    /// Setup only: the chosen password's hash, before the setup finishes.
    pub staged_hash: Option<String>,
    /// Setup only: the authenticator secret being enrolled, sealed.
    pub staged_totp: Option<String>,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("token_hash", &"<redacted>")
            .field("purpose", &self.purpose)
            .field("setup_code_id", &self.setup_code_id)
            .field("epoch", &self.epoch)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("attempts", &self.attempts)
            .field("staged_hash", &redacted(&self.staged_hash))
            .field("staged_totp", &redacted(&self.staged_totp))
            .finish()
    }
}

/// One person's password state, as a plan reads it.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The person (`None`: no such user).
    pub person: Option<Person>,
    /// The acting admin, when the plan was given one.
    pub actor: Option<Person>,
    pub credential: Option<Credential>,
    pub recovery: Vec<RecoveryCode>,
    /// The live setup code (at most one).
    pub setup_code: Option<SetupCode>,
    /// The person's pending tokens.
    pub pending: Vec<Pending>,
    /// The counters of the address the attempt comes from (all zero when none are stored, or
    /// when the plan was given no address).
    pub source: SourceCounters,
}

impl Snapshot {
    pub fn pending(&self, token_hash: &str) -> Option<&Pending> {
        self.pending
            .iter()
            .find(|p| super::codes::same_hash(&p.token_hash, token_hash))
    }
}

/// What a plan writes. Everything here is written in one transaction, or nothing is.
#[derive(Clone, Default)]
pub struct Writes {
    /// Insert or replace the person's credential row.
    pub credential: Option<Credential>,
    /// Replace every recovery code with these hashes.
    pub recovery: Option<Vec<String>>,
    /// Mark one recovery code used.
    pub use_recovery: Option<String>,
    /// Issue a setup code: the live one, if any, is revoked first.
    pub issue_code: Option<NewSetupCode>,
    /// Mark a setup code used.
    pub use_code: Option<i64>,
    /// Insert or replace these pending tokens (by hash).
    pub put_pending: Vec<Pending>,
    /// Delete these pending tokens (by hash).
    pub delete_pending: Vec<String>,
    /// Delete every pending token of the person (before `put_pending`).
    pub clear_pending: bool,
    /// Every address's setup counters for the person back to zero (a new code).
    pub reset_source_setup: bool,
    /// Every address's counters for the person back to zero, their known addresses kept (a
    /// finished setup or reset). Before `source`.
    pub reset_sources: bool,
    /// Insert or replace the counters of the address the plan was given.
    pub source: Option<SourceCounters>,
    pub events: Vec<AuditEvent>,
    pub changes: Vec<RoleChange>,
}

impl std::fmt::Debug for Writes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writes")
            .field("credential", &self.credential)
            .field("recovery", &self.recovery.as_ref().map(Vec::len))
            .field("use_recovery", &redacted(&self.use_recovery))
            .field("issue_code", &self.issue_code)
            .field("use_code", &self.use_code)
            .field("put_pending", &self.put_pending)
            .field("delete_pending", &self.delete_pending.len())
            .field("clear_pending", &self.clear_pending)
            .field("reset_source_setup", &self.reset_source_setup)
            .field("reset_sources", &self.reset_sources)
            .field("source", &self.source)
            .field("events", &self.events.len())
            .field("changes", &self.changes.len())
            .finish()
    }
}

/// A plan: reads one snapshot, says what to write. Called once, inside the transaction.
pub type Plan<'a> = Box<dyn FnOnce(&Snapshot) -> Writes + Send + 'a>;

/// What the Team page and the person's own security page show about a person's password, and
/// nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PasswordStatus {
    /// A password that works: set, and not held ([`Credential::held_for`]).
    pub password_set: bool,
    pub second_step_enrolled: bool,
    /// A setup or reset code issued, unused and unexpired.
    pub setup_code_pending: bool,
    /// Unused recovery codes (the person's own view only).
    pub recovery_codes_left: u32,
    pub epoch: i64,
    /// Set, but held until a host-issued setup replaces it: the person holds Admin or Auditor
    /// and their credential came from a code an admin issued in the web app (PM3).
    pub needs_host_setup: bool,
    /// A reset the person hasn't dismissed (PM2).
    pub reset_notice: Option<ResetNotice>,
}

impl PasswordStatus {
    /// The status of `credential` for `person`.
    pub fn of(
        person: Option<&Person>,
        credential: Option<&Credential>,
        setup_code_pending: bool,
        recovery_codes_left: u32,
    ) -> Self {
        let held = match (person, credential) {
            (Some(p), Some(c)) => c.held_for(p),
            _ => false,
        };
        Self {
            password_set: !held && credential.is_some_and(|c| c.password_hash.is_some()),
            second_step_enrolled: !held && credential.is_some_and(|c| c.totp_sealed.is_some()),
            setup_code_pending,
            recovery_codes_left,
            epoch: credential.map_or(0, |c| c.epoch),
            needs_host_setup: held,
            reset_notice: credential.and_then(|c| c.reset_notice.clone()),
        }
    }
}

#[async_trait]
pub trait PasswordStore: Send + Sync {
    /// The handle whose sign-in name is `name` (lower-case), if any. No lock.
    async fn handle_by_sign_in_name(&self, name: &str) -> StoreResult<Option<String>>;

    /// The handle a pending token (by hash) belongs to, if it is stored. No lock.
    async fn handle_by_token(&self, token_hash: &str) -> StoreResult<Option<String>>;

    /// Load `handle`'s state (and `actor`'s, when given; and the counters of the address
    /// `source`, a [`super::limits::source_key`], when given) under the person's lock, run
    /// `plan`, write what it returns in the same transaction.
    async fn password_tx(
        &self,
        handle: &str,
        actor: Option<&str>,
        source: Option<&str>,
        now: DateTime<Utc>,
        plan: Plan<'_>,
    ) -> StoreResult<()>;

    /// Record audit events about no one in particular (a sign-in name nobody has).
    async fn password_events(&self, events: Vec<AuditEvent>) -> StoreResult<()>;

    /// Every person's [`PasswordStatus`] that has any password state, by handle.
    async fn password_statuses(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, PasswordStatus>>;

    /// `handle`'s [`PasswordStatus`] (all false when none).
    async fn password_status(
        &self,
        handle: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<PasswordStatus>;

    /// Delete pending tokens past their expiry, setup codes past theirs, and address counters
    /// with nothing left to remember. Returns how many.
    async fn prune_passwords(&self, now: DateTime<Utc>) -> StoreResult<u64>;
}
