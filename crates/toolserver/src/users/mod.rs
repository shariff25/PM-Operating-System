//! The user database (`docs/user-database-spec.md`): who exists, their status, roles and keys.
//!
//! Slice U1 ("Store"). Nothing decides from these tables yet: every service still reads
//! `content/team.yaml` until slices U3-U7 cut them over. What is here:
//!
//! - the model ([`User`], [`UserRole`], [`Identity`], [`RoleChange`], [`PilotException`]);
//! - [`UserStore`], with a Postgres implementation (`pg.rs`, migration 0015) and an in-memory
//!   one ([`MemoryUsers`]) that behave the same;
//! - the rules ([`rules`]): every write is planned against the current [`Directory`], then the
//!   planned result is checked against the same invariants the database's
//!   `users_invariants()` trigger enforces at commit (the last admin, Admin and Auditor only
//!   under the pilot exception, the `evals` account). In Postgres the trigger is the
//!   guarantee and the plan's check only gives a clear refusal first; in memory the plan's
//!   check is the guarantee;
//! - the import from `team.yaml` and the dev seed ([`import`]);
//! - the host commands, `pmos-toolserver users …` ([`host`]).
//!
//! Every write records `role_changes` rows (who, what, when, through which client, and the
//! invoking OS user for host commands) and audit events, in the same transaction. A refused
//! write records a `denied` audit event with the refusal's code. Neither carries an address,
//! an `oid` or a token.

pub mod detail;
pub mod host;
pub mod import;
pub mod memory;
pub mod pg;
pub mod rules;

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::store::{StoreError, StoreResult};

pub use import::{ImportMode, ImportRequest};
pub use memory::MemoryUsers;
pub use rules::{Plan, Rules};

/// The handle of the eval stack's account: `pm` only, never an identity (§1).
pub const EVALS: &str = "evals";

/// Web and Slack together may deactivate at most this many people in any rolling hour by
/// default (owner's answer to the spec's open question 1; `PMOS_USERS_DEACTIVATIONS_PER_HOUR`
/// changes it from slice U3). Host commands aren't capped.
pub const DEFAULT_DEACTIVATIONS_PER_HOUR: u32 = 3;

/// The window the deactivation cap counts over.
pub fn deactivation_window() -> Duration {
    Duration::hours(1)
}

/// A deactivated person's linked accounts are deleted this long after deactivation unless
/// they are reactivated first (owner's answer to open question 2). Used by slice U3.
pub const CREDENTIAL_GRACE_HOURS: i64 = 24;

/// `role_changes` rows about a person are kept until this long after their deactivation
/// (owner Q5). Written into the database's trigger too.
pub const ROLE_CHANGES_RETENTION_DAYS: i64 = 90;

/// The longest a pilot exception may run, from when it is switched on (the table's `CHECK`).
pub const MAX_EXCEPTION_DAYS: i64 = 90;

/// The four roles (owner's scope item b). `skill-author` from `team.yaml` is not one: the
/// import drops it and the person keeps `pm` (owner Q4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    Admin,
    Pm,
    Reviewer,
    Auditor,
}

impl UserRole {
    pub const ALL: [UserRole; 4] = [Self::Admin, Self::Pm, Self::Reviewer, Self::Auditor];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Pm => "pm",
            Self::Reviewer => "reviewer",
            Self::Auditor => "auditor",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// Roles as `pm, reviewer` (fixed names only).
pub fn role_list(roles: &BTreeSet<UserRole>) -> String {
    roles
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Deactivated,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deactivated => "deactivated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Active, Self::Deactivated]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// The kinds of key that point at a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    /// A Slack member id; issuer: the workspace id, or empty when unknown (import, host).
    Slack,
    /// An Entra object id (`oid`); issuer: the tenant id (`tid`).
    Entra,
    /// An Atlassian account id.
    Atlassian,
    /// A GitHub numeric user id.
    Github,
}

impl IdentityKind {
    pub const ALL: [IdentityKind; 4] = [Self::Slack, Self::Entra, Self::Atlassian, Self::Github];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Entra => "entra",
            Self::Atlassian => "atlassian",
            Self::Github => "github",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Through which client a change came. Each column allows a subset: `users.created_client`
/// web, bot, host, import; `user_identities.linked_client` those and link; `role_changes.client`
/// web, bot, host, import, system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// `pmos-web`, with a signed assertion (slice U5).
    Web,
    /// The Slack bot client, unsigned (slices U3, U4b).
    Bot,
    /// A host command, `pmos-admin users …`, with the OS user recorded.
    Host,
    /// The import from `team.yaml`, or the dev seed.
    Import,
    /// An OAuth or sign-in link (identities only).
    Link,
    /// The tool server itself (retention, the credential sweeper).
    System,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Bot => "bot",
            Self::Host => "host",
            Self::Import => "import",
            Self::Link => "link",
            Self::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Web,
            Self::Bot,
            Self::Host,
            Self::Import,
            Self::Link,
            Self::System,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    /// Whether the deactivation cap counts a deactivation through this client.
    pub fn capped(self) -> bool {
        matches!(self, Self::Web | Self::Bot)
    }
}

/// What a `role_changes` row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Create,
    Grant,
    Revoke,
    Deactivate,
    Reactivate,
    Link,
    Unlink,
    Import,
    Assign,
    Unassign,
    SetField,
    ConfirmSignInName,
    SuspendCredentials,
    DeleteCredentials,
    ExceptionOn,
    ExceptionOff,
}

impl ChangeKind {
    pub const ALL: [ChangeKind; 16] = [
        Self::Create,
        Self::Grant,
        Self::Revoke,
        Self::Deactivate,
        Self::Reactivate,
        Self::Link,
        Self::Unlink,
        Self::Import,
        Self::Assign,
        Self::Unassign,
        Self::SetField,
        Self::ConfirmSignInName,
        Self::SuspendCredentials,
        Self::DeleteCredentials,
        Self::ExceptionOn,
        Self::ExceptionOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Grant => "grant",
            Self::Revoke => "revoke",
            Self::Deactivate => "deactivate",
            Self::Reactivate => "reactivate",
            Self::Link => "link",
            Self::Unlink => "unlink",
            Self::Import => "import",
            Self::Assign => "assign",
            Self::Unassign => "unassign",
            Self::SetField => "set_field",
            Self::ConfirmSignInName => "confirm_sign_in_name",
            Self::SuspendCredentials => "suspend_credentials",
            Self::DeleteCredentials => "delete_credentials",
            Self::ExceptionOn => "exception_on",
            Self::ExceptionOff => "exception_off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// When and by whom a role was granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleGrant {
    pub at: DateTime<Utc>,
    pub by: String,
}

/// One key that points at a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub kind: IdentityKind,
    pub issuer: String,
    pub subject: String,
    pub linked_at: DateTime<Utc>,
    pub linked_by: String,
    pub linked_via: Via,
    /// Entra only: the host set or confirmed it (required for Admin and Auditor, §7).
    pub host_confirmed: bool,
}

/// A person. Addresses are not secrets, but they are never put in an audit event or a
/// `role_changes` detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub handle: String,
    pub display_name: String,
    pub sign_in_name: Option<String>,
    pub sign_in_name_confirmed_at: Option<DateTime<Utc>>,
    pub sign_in_name_confirmed_by: Option<String>,
    pub atlassian_email: Option<String>,
    pub github_login: Option<String>,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub created_via: Via,
    pub deactivated_at: Option<DateTime<Utc>>,
    pub deactivated_by: Option<String>,
    pub roles: BTreeMap<UserRole, RoleGrant>,
    pub identities: BTreeMap<IdentityKind, Identity>,
}

impl User {
    pub fn active(&self) -> bool {
        self.status == Status::Active
    }

    pub fn has(&self, role: UserRole) -> bool {
        self.roles.contains_key(&role)
    }

    pub fn role_set(&self) -> BTreeSet<UserRole> {
        self.roles.keys().copied().collect()
    }

    /// Holds both `admin` and `auditor` (allowed only under the pilot exception).
    pub fn dual_role(&self) -> bool {
        self.has(UserRole::Admin) && self.has(UserRole::Auditor)
    }

    /// Holds `admin` or `auditor`: their addresses and Entra link change on the host only.
    pub fn protected(&self) -> bool {
        self.has(UserRole::Admin) || self.has(UserRole::Auditor)
    }

    pub fn identity(&self, kind: IdentityKind) -> Option<&Identity> {
        self.identities.get(&kind)
    }

    /// The Entra state `resolve` reports (§2): `none`, `linked`, or `needs_host` for an
    /// Admin or Auditor whose link the host hasn't confirmed.
    pub fn entra_state(&self) -> &'static str {
        match self.identity(IdentityKind::Entra) {
            None => "none",
            Some(e) if self.protected() && !e.host_confirmed => "needs_host",
            Some(_) => "linked",
        }
    }
}

/// The open pilot exception (Admin and Auditor on one person, §6), lapsed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotException {
    pub enabled_at: DateTime<Utc>,
    pub enabled_by: String,
    pub expires_at: DateTime<Utc>,
}

impl PilotException {
    /// Open and not past `expires_at`: only then may a person hold both roles.
    pub fn in_force(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now
    }
}

/// One row of the authoritative history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChange {
    /// 0 until stored.
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub via: Via,
    pub host_user: Option<String>,
    /// A handle, or `*` for an import or a pilot exception.
    pub target: String,
    pub change: ChangeKind,
    pub role: Option<UserRole>,
    /// Fixed formats only (`detail.rs`), never free text.
    pub detail: Option<String>,
}

/// Everything the rules need to plan a write, read under the users lock.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub users: BTreeMap<String, User>,
    /// The open pilot exception (`ended_at` unset), lapsed or not.
    pub exception: Option<PilotException>,
    /// When the web and Slack clients deactivated someone within the cap's window before
    /// the `now` the directory was read for, from `role_changes`.
    pub capped_deactivations: Vec<DateTime<Utc>>,
    /// The directory's version (the ETag, slice U2).
    pub version: i64,
}

impl Directory {
    pub fn get(&self, handle: &str) -> Option<&User> {
        self.users.get(handle)
    }

    /// Is there an active admin?
    pub fn has_active_admin(&self) -> bool {
        self.users
            .values()
            .any(|u| u.active() && u.has(UserRole::Admin))
    }

    /// May a person hold Admin and Auditor right now?
    pub fn exception_in_force(&self, now: DateTime<Utc>) -> bool {
        self.exception.as_ref().is_some_and(|e| e.in_force(now))
    }

    /// Active people holding both Admin and Auditor.
    pub fn dual_role_holders(&self) -> Vec<&User> {
        self.users
            .values()
            .filter(|u| u.active() && u.dual_role())
            .collect()
    }
}

/// The OS user who ran a host command (`PMOS_HOST_USER`): what the host says, not a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostUser(String);

impl HostUser {
    /// `^[a-z_][a-z0-9_.-]{0,31}$`, the `role_changes.host_user` check.
    pub fn parse(s: &str) -> Option<Self> {
        let mut chars = s.chars();
        let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_');
        let ok = first_ok
            && s.len() <= 32
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
            });
        ok.then(|| Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Who makes a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// A handle, or `host` / `import`.
    pub name: String,
    pub via: Via,
    pub host_user: Option<HostUser>,
}

impl Actor {
    /// A host command (`pmos-admin users …`).
    pub fn host(host_user: HostUser) -> Self {
        Self {
            name: "host".into(),
            via: Via::Host,
            host_user: Some(host_user),
        }
    }

    /// The import from `team.yaml` on the host, or (with no host user) the dev seed.
    pub fn import(host_user: Option<HostUser>) -> Self {
        Self {
            name: "import".into(),
            via: Via::Import,
            host_user,
        }
    }

    /// A signed-in person through `pmos-web` (slice U5).
    pub fn web(handle: &str) -> Self {
        Self {
            name: handle.into(),
            via: Via::Web,
            host_user: None,
        }
    }

    /// An admin named by the Slack bot client (unsigned; slices U3, U4b).
    pub fn bot(handle: &str) -> Self {
        Self {
            name: handle.into(),
            via: Via::Bot,
            host_user: None,
        }
    }
}

/// Why a write was refused. [`Refusal::code`] is what the audit event's detail and an API's
/// error code carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A malformed value.
    Invalid,
    NotFound,
    Exists,
    /// A key or address that belongs to another user (they are never reused).
    Taken,
    /// Nothing to change.
    Unchanged,
    LastAdmin,
    DualRole,
    EvalsAccount,
    /// Not READ COMMITTED (the trigger's isolation check).
    Isolation,
    /// `bootstrap-admin` with an active admin already there.
    AdminExists,
    /// `import --bootstrap` on a table with any row.
    NotEmpty,
    NotActive,
    /// The web or Slack actor isn't an active admin in the directory.
    Forbidden,
    HostOnly,
    SelfChange,
    AdminNotFromSlack,
    RoleNotFromSlack,
    ProtectedNotFromSlack,
    /// Addresses and Entra links never change through the Slack bot client.
    NotFromSlack,
    DeactivationLimit,
    /// An Atlassian email changes in web only while no Atlassian account is linked.
    Linked,
    EntraMismatch,
    EntraNeedsHost,
    ExceptionOpen,
    NoException,
    ExceptionTooLong,
    /// The import stopped before any write (unknown role, duplicate key, …).
    ImportRefused,
}

impl Refusal {
    pub const ALL: [Refusal; 27] = [
        Self::Invalid,
        Self::NotFound,
        Self::Exists,
        Self::Taken,
        Self::Unchanged,
        Self::LastAdmin,
        Self::DualRole,
        Self::EvalsAccount,
        Self::Isolation,
        Self::AdminExists,
        Self::NotEmpty,
        Self::NotActive,
        Self::Forbidden,
        Self::HostOnly,
        Self::SelfChange,
        Self::AdminNotFromSlack,
        Self::RoleNotFromSlack,
        Self::ProtectedNotFromSlack,
        Self::NotFromSlack,
        Self::DeactivationLimit,
        Self::Linked,
        Self::EntraMismatch,
        Self::EntraNeedsHost,
        Self::ExceptionOpen,
        Self::NoException,
        Self::ExceptionTooLong,
        Self::ImportRefused,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::Exists => "exists",
            Self::Taken => "taken",
            Self::Unchanged => "unchanged",
            Self::LastAdmin => "last_admin",
            Self::DualRole => "dual_role",
            Self::EvalsAccount => "evals_account",
            Self::Isolation => "users_isolation",
            Self::AdminExists => "admin_exists",
            Self::NotEmpty => "not_empty",
            Self::NotActive => "not_active",
            Self::Forbidden => "forbidden",
            Self::HostOnly => "host_only",
            Self::SelfChange => "self_change",
            Self::AdminNotFromSlack => "admin_not_from_slack",
            Self::RoleNotFromSlack => "role_not_from_slack",
            Self::ProtectedNotFromSlack => "protected_not_from_slack",
            Self::NotFromSlack => "not_from_slack",
            Self::DeactivationLimit => "deactivation_limit",
            Self::Linked => "linked",
            Self::EntraMismatch => "entra_mismatch",
            Self::EntraNeedsHost => "entra_needs_host",
            Self::ExceptionOpen => "exception_open",
            Self::NoException => "no_exception",
            Self::ExceptionTooLong => "exception_too_long",
            Self::ImportRefused => "import_refused",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.code() == code)
    }
}

/// A refused write, or a storage failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserError {
    #[error("{message}")]
    Refused { refusal: Refusal, message: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl UserError {
    pub fn refused(refusal: Refusal, message: impl Into<String>) -> Self {
        Self::Refused {
            refusal,
            message: message.into(),
        }
    }

    /// The refusal, if this is one.
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { refusal, .. } => Some(*refusal),
            Self::Store(_) => None,
        }
    }
}

pub type UserResult<T> = Result<T, UserError>;

/// A person to create.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewUser {
    pub handle: String,
    pub display_name: String,
    pub sign_in_name: Option<String>,
    pub atlassian_email: Option<String>,
    pub github_login: Option<String>,
    pub slack_id: Option<String>,
    pub roles: BTreeSet<UserRole>,
}

/// An Entra key: tenant id and object id, both GUIDs, stored lower-case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraKey {
    pub tid: String,
    pub oid: String,
}

/// How `grant-admin` treats the person's Entra link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminEntra {
    /// Set or confirm this host-confirmed link in the same transaction.
    Oid(EntraKey),
    /// Keep the existing link, which must already be host-confirmed.
    Keep,
}

/// An address field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    SignInName,
    AtlassianEmail,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignInName => "sign_in_name",
            Self::AtlassianEmail => "atlassian_email",
        }
    }
}

/// One write to the user database. Each is planned by [`rules::plan`] and applied whole or
/// not at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// The first admin (§5): refused once any active admin exists.
    BootstrapAdmin {
        user: NewUser,
        entra: Option<EntraKey>,
    },
    /// Invite or join approval (slices U3, U5): never `admin`; from Slack `pm` only.
    CreateUser {
        user: NewUser,
    },
    /// The whole role set (web, U5). Never grants `admin` (that is [`Op::GrantAdmin`]).
    SetRoles {
        handle: String,
        roles: BTreeSet<UserRole>,
    },
    /// Host only: the one way to make an admin (owner Q1).
    GrantAdmin {
        handle: String,
        entra: AdminEntra,
    },
    RevokeAdmin {
        handle: String,
    },
    /// Host only: set or replace a host-confirmed Entra link.
    SetEntra {
        handle: String,
        key: EntraKey,
    },
    UnsetEntra {
        handle: String,
    },
    SetField {
        handle: String,
        field: Field,
        value: String,
    },
    Deactivate {
        handle: String,
    },
    Reactivate {
        handle: String,
    },
    /// Host only: Admin and Auditor allowed on one person until `until` (≤ 90 days).
    ExceptionOn {
        until: DateTime<Utc>,
    },
    /// Host only: refused while anyone holds both, unless `revoke_auditor`.
    ExceptionOff {
        revoke_auditor: bool,
    },
    Import(Box<ImportRequest>),
}

impl Op {
    /// The audit event's `tool` for this write.
    pub fn action(&self) -> &'static str {
        match self {
            Self::BootstrapAdmin { .. } => "users.bootstrap_admin",
            Self::CreateUser { .. } => "users.create",
            Self::SetRoles { .. } | Self::RevokeAdmin { .. } => "users.roles",
            Self::GrantAdmin { .. } => "users.grant_admin",
            Self::SetEntra { .. } => "users.set_entra",
            Self::UnsetEntra { .. } => "users.identity_unlink",
            Self::SetField { .. } => "users.fields",
            Self::Deactivate { .. } => "users.deactivate",
            Self::Reactivate { .. } => "users.reactivate",
            Self::ExceptionOn { .. } | Self::ExceptionOff { .. } => "users.pilot_exception",
            Self::Import(_) => "users.import",
        }
    }

    /// The audit event's `output_ref`: a handle, or `*`.
    pub fn target(&self) -> String {
        match self {
            Self::BootstrapAdmin { user, .. } | Self::CreateUser { user } => user.handle.clone(),
            Self::SetRoles { handle, .. }
            | Self::GrantAdmin { handle, .. }
            | Self::RevokeAdmin { handle }
            | Self::SetEntra { handle, .. }
            | Self::UnsetEntra { handle }
            | Self::SetField { handle, .. }
            | Self::Deactivate { handle }
            | Self::Reactivate { handle } => handle.clone(),
            Self::ExceptionOn { .. } | Self::ExceptionOff { .. } | Self::Import(_) => "*".into(),
        }
    }
}

/// What a write did, in lines for the host command to print (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    pub lines: Vec<String>,
}

#[async_trait]
pub trait UserStore: Send + Sync {
    /// The rules this store plans with.
    fn rules(&self) -> Rules;

    /// The directory as of `now` (the cap's window ends at `now`).
    async fn directory(&self, now: DateTime<Utc>) -> StoreResult<Directory>;

    /// Plan `op` against the current directory under the users lock, check the result, write
    /// it with its `role_changes` rows and audit events in one transaction. A refusal records
    /// a `denied` audit event and changes nothing else.
    async fn apply(&self, actor: &Actor, op: Op, now: DateTime<Utc>) -> UserResult<Applied>;

    /// `role_changes`, newest first; for one target when given.
    async fn role_changes(
        &self,
        target: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<RoleChange>>;

    /// The retention rule for `role_changes` (owner Q5): delete the rows about people
    /// deactivated more than 90 days ago, never an import or pilot exception row. Returns
    /// how many went.
    async fn purge_role_changes(&self, now: DateTime<Utc>) -> StoreResult<u64>;

    /// What `op` would do, without writing: the import's dry run.
    async fn preview(&self, actor: &Actor, op: &Op, now: DateTime<Utc>) -> UserResult<Applied> {
        let dir = self.directory(now).await?;
        let plan = rules::plan(&dir, actor, op, now, &self.rules())?;
        Ok(Applied { lines: plan.lines })
    }

    /// The deactivation cap's count (§6): deactivations through the web and Slack clients in
    /// the rolling hour before `now`, counted from `role_changes`. A refused attempt writes no
    /// row, so it never counts; a reactivation deletes nothing, so it never resets the count.
    /// [`Op::Deactivate`] from those clients is refused once this reaches the cap.
    async fn deactivations_in_window(&self, now: DateTime<Utc>) -> StoreResult<u32> {
        let dir = self.directory(now).await?;
        Ok(u32::try_from(dir.capped_deactivations.len()).unwrap_or(u32::MAX))
    }
}

/// The audit event for one of a write's [`rules::UserEvent`]s: the actor, the client (host
/// commands and the import as `host`), the target handle in `output_ref`, a fixed-format
/// detail. Metadata only.
pub(crate) fn audit_event(
    actor: &Actor,
    event: &rules::UserEvent,
    now: DateTime<Utc>,
) -> crate::audit::AuditEvent {
    use crate::audit::Client;
    crate::audit::AuditEvent {
        at: now,
        client: match actor.via {
            Via::Web => Client::Web,
            Via::Bot => Client::Bot,
            Via::System => Client::System,
            Via::Host | Via::Import | Via::Link => Client::Host,
        },
        run_id: "users".into(),
        user: actor.name.clone(),
        skill: None,
        bot: None,
        tool: event.action.into(),
        outcome: event.outcome,
        sources: vec![],
        source_titles: Default::default(),
        args_hash: None,
        output_ref: Some(event.target.clone()),
        detail: event.detail.clone(),
    }
}
