//! Planning a user write, and the invariants every write must keep.
//!
//! [`plan`] turns an [`Op`] into primitive [`Write`]s, the `role_changes` rows and audit
//! events that record them, and lines for the host command. It then applies the writes to a
//! copy of the [`Directory`] ([`apply_writes`]: the tables' keys, foreign keys and `CHECK`s)
//! and checks the copy ([`check_invariants`]: the `users_invariants()` trigger, migration
//! 0015). The in-memory store commits that copy, so these two functions are its guarantee. In
//! Postgres the trigger is the guarantee (it takes the lock and re-reads at commit); the
//! check here only makes a clear refusal before anything is written.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};

use super::detail;
use super::{
    Actor, AdminEntra, ChangeKind, Directory, EntraKey, Field, Identity, IdentityKind, NewUser, Op,
    PilotException, Refusal, RoleChange, RoleGrant, Status, User, UserError, UserResult, UserRole,
    Via, DEFAULT_DEACTIVATIONS_PER_HOUR, EVALS, MAX_EXCEPTION_DAYS,
};
use crate::audit::Outcome;

/// What a store plans with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// The deactivation cap for the web and Slack clients together (1 to 10).
    pub deactivations_per_hour: u32,
    /// The in-memory dev store with fake connectors only: the seed may give `evals` its fake
    /// Slack id. Never in Postgres.
    pub evals_slack_allowed: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            deactivations_per_hour: DEFAULT_DEACTIVATIONS_PER_HOUR,
            evals_slack_allowed: false,
        }
    }
}

/// One primitive change to the tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// A new row in `users` (its roles and identities follow as their own writes).
    InsertUser(Box<User>),
    /// Every column of an existing `users` row but the handle and creation.
    UpdateUser(Box<User>),
    InsertRole {
        handle: String,
        role: UserRole,
        grant: RoleGrant,
    },
    DeleteRole {
        handle: String,
        role: UserRole,
    },
    InsertIdentity {
        handle: String,
        identity: Identity,
    },
    DeleteIdentity {
        handle: String,
        kind: IdentityKind,
    },
    OpenException(PilotException),
    EndException {
        ended_at: DateTime<Utc>,
        ended_by: String,
    },
    /// The whole set of PMs assigned to `reviewer` (`reviewer_assignments`, migration 0018):
    /// rows not in `pms` go, new ones are added with `at` and `by`, kept ones are untouched.
    SetAssignments {
        reviewer: String,
        pms: BTreeSet<String>,
        at: DateTime<Utc>,
        by: String,
    },
}

/// Whom a write touched: the trigger checks Admin+Auditor and the `evals` rule for these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Touched {
    People(BTreeSet<String>),
    /// A pilot exception change touches everyone.
    Everyone,
}

/// A change outside the users tables that a write makes in the same transaction (Postgres) or
/// right after it (in memory): a deactivation's and a reactivation's consequences (§6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// End every MCP session of the person.
    RevokeSessions(String),
    /// Suspend every linked account of the person: never unsealed until restored.
    SuspendCredentials(String),
    /// A reactivation within the grace: the accounts work again.
    RestoreCredentials(String),
    /// The grace is over: delete the suspended accounts.
    DeleteSuspendedCredentials(String),
    /// The grace is over: close the person's pending drafts as the system (`rejected`, ledger
    /// reason `owner_deactivated`); no write runs.
    CloseHeldDrafts(String),
    /// An admin unlinked the person's Atlassian or GitHub identity: delete that linked
    /// account too, so another one can be linked (§6).
    DeleteCredential(String, crate::vault::Provider),
}

/// One audit event a write records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEvent {
    pub action: &'static str,
    pub target: String,
    pub outcome: Outcome,
    pub detail: Option<String>,
}

/// A planned, checked write.
#[derive(Debug, Clone)]
pub struct Plan {
    pub writes: Vec<Write>,
    pub changes: Vec<RoleChange>,
    pub events: Vec<UserEvent>,
    pub lines: Vec<String>,
    pub touched: Touched,
    /// What else the write does, outside the users tables.
    pub effects: Vec<Effect>,
    /// The directory after the writes (the in-memory store commits it).
    pub next: Directory,
}

fn refused(refusal: Refusal, message: impl Into<String>) -> UserError {
    UserError::refused(refusal, message)
}

// --- Values ----------------------------------------------------------------------------------

/// Names that are actors in `role_changes`, never a person's handle.
pub const RESERVED_HANDLES: [&str; 3] = ["host", "import", "system"];

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, not `bots.*`, not a reserved actor name.
pub fn valid_handle(handle: &str) -> bool {
    let mut chars = handle.chars();
    let first = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit());
    first
        && handle.len() <= 64
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && !handle.starts_with("bots.")
        && !RESERVED_HANDLES.contains(&handle)
}

/// A display name as stored: control characters and `<`, `>` removed, runs of white space
/// one space, trimmed, at most 100 characters (§10: untrusted text).
pub fn clean_display_name(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(c, '<' | '>'))
        .collect();
    kept.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(100)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn valid_display_name(s: &str) -> bool {
    s.chars().count() <= 100 && !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

/// An address (sign-in name or Atlassian email) as stored: trimmed, lower-case, a plausible
/// email, at most 254 characters.
pub fn address(raw: &str, what: &str) -> UserResult<String> {
    let value = raw.trim().to_lowercase();
    if value.len() > 254 || !pmos_contract::model::valid_email(&value) {
        return Err(refused(
            Refusal::Invalid,
            format!("that {what} isn't an email address"),
        ));
    }
    Ok(value)
}

fn valid_github_login(s: &str) -> bool {
    (1..=39).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A GitHub login: `^[A-Za-z0-9-]{1,39}$`.
pub fn github_login(raw: &str) -> UserResult<String> {
    let value = raw.trim();
    if !valid_github_login(value) {
        return Err(refused(
            Refusal::Invalid,
            format!("`{value}` isn't a GitHub login"),
        ));
    }
    Ok(value.to_string())
}

/// A GUID (an Entra `tid` or `oid`), lower-case.
pub fn guid(raw: &str, what: &str) -> UserResult<String> {
    let value = raw.trim().to_lowercase();
    if !pmos_contract::model::valid_entra_oid(&value) {
        return Err(refused(
            Refusal::Invalid,
            format!("the {what} must be a GUID (8-4-4-4-12 hex digits)"),
        ));
    }
    Ok(value)
}

/// A Slack member id (`U…` or `W…`).
pub fn slack_id(raw: &str) -> UserResult<String> {
    let value = raw.trim();
    if !pmos_contract::model::valid_slack_id(value) {
        return Err(refused(
            Refusal::Invalid,
            format!("`{value}` isn't a Slack member id (U… or W…)"),
        ));
    }
    Ok(value.to_string())
}

/// The `users` row's `CHECK`s.
fn validate_row(u: &User) -> UserResult<()> {
    let bad = |what: &str| Err(refused(Refusal::Invalid, format!("{}: {what}", u.handle)));
    if !valid_handle(&u.handle) {
        return Err(refused(
            Refusal::Invalid,
            format!(
                "`{}` isn't a usable handle: lowercase letters, digits, `.`, `_` and `-`, at most 64",
                u.handle
            ),
        ));
    }
    if !valid_display_name(&u.display_name) {
        return bad("the display name has control characters, `<` or `>`, or is too long");
    }
    for a in [&u.sign_in_name, &u.atlassian_email].into_iter().flatten() {
        if *a != a.to_lowercase() || a.len() > 254 {
            return bad("an address must be lower-case and at most 254 characters");
        }
    }
    if u.github_login
        .as_deref()
        .is_some_and(|g| !valid_github_login(g))
    {
        return bad("not a GitHub login");
    }
    if u.sign_in_name_confirmed_at.is_some() != u.sign_in_name_confirmed_by.is_some() {
        return bad("a sign-in name confirmation needs both when and by whom");
    }
    if (u.status == Status::Deactivated) != u.deactivated_at.is_some()
        || u.deactivated_at.is_some() != u.deactivated_by.is_some()
    {
        return bad("deactivated_at is set exactly while the user is deactivated");
    }
    if !matches!(u.created_via, Via::Web | Via::Bot | Via::Host | Via::Import) {
        return bad("created through an unknown client");
    }
    Ok(())
}

/// A user's addresses as the unique indexes compare them: `(field, value)`, GitHub logins
/// lower-cased (the names `retired_addresses.kind` uses).
fn compared_addresses(u: &User) -> Vec<(&'static str, String)> {
    [
        ("sign_in_name", u.sign_in_name.clone()),
        ("atlassian_email", u.atlassian_email.clone()),
        (
            "github_login",
            u.github_login.as_ref().map(|g| g.to_lowercase()),
        ),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect()
}

/// The unique indexes on addresses, across every user, active or not, and every address a
/// user had before (`retired_addresses`): never another user's (L8).
fn unique_addresses(dir: &Directory, u: &User) -> UserResult<()> {
    for (field, value) in compared_addresses(u) {
        if let Some(holder) = dir.retired.get(&(field.to_string(), value)) {
            if holder != &u.handle {
                return Err(refused(
                    Refusal::Taken,
                    format!(
                        "that {} was `{holder}`'s (addresses are never reused)",
                        field.replace('_', " ")
                    ),
                ));
            }
        }
    }
    for other in dir.users.values().filter(|o| o.handle != u.handle) {
        let clash = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        let field = if clash(&u.sign_in_name, &other.sign_in_name) {
            Some("sign-in name")
        } else if clash(&u.atlassian_email, &other.atlassian_email) {
            Some("Atlassian email")
        } else if u.github_login.is_some()
            && u.github_login.as_ref().map(|g| g.to_lowercase())
                == other.github_login.as_ref().map(|g| g.to_lowercase())
        {
            Some("GitHub login")
        } else {
            None
        };
        if let Some(field) = field {
            return Err(refused(
                Refusal::Taken,
                format!(
                    "that {field} belongs to `{}` (addresses are never reused)",
                    other.handle
                ),
            ));
        }
    }
    Ok(())
}

// --- The tables ------------------------------------------------------------------------------

/// Apply `writes` to `dir` as the tables would: keys, foreign keys and `CHECK`s. Not the
/// invariants: those are [`check_invariants`], at "commit".
pub fn apply_writes(dir: &mut Directory, writes: &[Write]) -> UserResult<()> {
    for w in writes {
        match w {
            Write::InsertUser(u) => {
                validate_row(u)?;
                if dir.users.contains_key(&u.handle) {
                    return Err(refused(
                        Refusal::Exists,
                        format!(
                            "`{}` is already a user (handles are never reused)",
                            u.handle
                        ),
                    ));
                }
                unique_addresses(dir, u)?;
                let mut row = (**u).clone();
                row.roles.clear();
                row.identities.clear();
                dir.users.insert(row.handle.clone(), row);
            }
            Write::UpdateUser(u) => {
                validate_row(u)?;
                unique_addresses(dir, u)?;
                let Some(row) = dir.users.get_mut(&u.handle) else {
                    return Err(not_found(&u.handle));
                };
                // An address the user no longer has is kept as theirs (L8), as the
                // `users_addresses_never_reused` trigger keeps it in Postgres.
                let now: Vec<(&str, String)> = compared_addresses(u);
                for (field, value) in compared_addresses(row) {
                    if !now.iter().any(|(f, v)| *f == field && *v == value) {
                        dir.retired
                            .entry((field.to_string(), value))
                            .or_insert_with(|| u.handle.clone());
                    }
                }
                let (roles, identities) = (row.roles.clone(), row.identities.clone());
                *row = (**u).clone();
                row.roles = roles;
                row.identities = identities;
            }
            Write::InsertRole {
                handle,
                role,
                grant,
            } => {
                let row = dir.users.get_mut(handle).ok_or_else(|| not_found(handle))?;
                if row.roles.contains_key(role) {
                    return Err(refused(
                        Refusal::Exists,
                        format!("`{handle}` already holds {}", role.as_str()),
                    ));
                }
                row.roles.insert(*role, grant.clone());
            }
            Write::DeleteRole { handle, role } => {
                if let Some(row) = dir.users.get_mut(handle) {
                    row.roles.remove(role);
                }
            }
            Write::InsertIdentity { handle, identity } => {
                if identity.subject.is_empty() {
                    return Err(refused(Refusal::Invalid, "an identity needs a subject"));
                }
                if !matches!(
                    identity.linked_via,
                    Via::Web | Via::Bot | Via::Host | Via::Import | Via::Link
                ) {
                    return Err(refused(
                        Refusal::Invalid,
                        "linked through an unknown client",
                    ));
                }
                if let Some(owner) = dir.users.values().find(|u| {
                    u.identity(identity.kind).is_some_and(|i| {
                        i.issuer == identity.issuer && i.subject == identity.subject
                    })
                }) {
                    return Err(refused(
                        Refusal::Taken,
                        format!(
                            "that {} key belongs to `{}` (one person per key)",
                            identity.kind.as_str(),
                            owner.handle
                        ),
                    ));
                }
                let row = dir.users.get_mut(handle).ok_or_else(|| not_found(handle))?;
                if row.identities.contains_key(&identity.kind) {
                    return Err(refused(
                        Refusal::Exists,
                        format!("`{handle}` already has a {} key", identity.kind.as_str()),
                    ));
                }
                row.identities.insert(identity.kind, identity.clone());
            }
            Write::DeleteIdentity { handle, kind } => {
                if let Some(row) = dir.users.get_mut(handle) {
                    row.identities.remove(kind);
                }
            }
            Write::OpenException(e) => {
                if dir.exception.is_some() {
                    return Err(refused(
                        Refusal::ExceptionOpen,
                        "a pilot exception is already open",
                    ));
                }
                if e.expires_at <= e.enabled_at {
                    return Err(refused(
                        Refusal::Invalid,
                        "the exception must end after it starts",
                    ));
                }
                if e.expires_at > e.enabled_at + Duration::days(MAX_EXCEPTION_DAYS) {
                    return Err(refused(
                        Refusal::ExceptionTooLong,
                        format!("a pilot exception lasts at most {MAX_EXCEPTION_DAYS} days"),
                    ));
                }
                dir.exception = Some(e.clone());
            }
            Write::EndException { .. } => dir.exception = None,
            Write::SetAssignments { reviewer, pms, .. } => {
                for h in std::iter::once(reviewer).chain(pms.iter()) {
                    if !dir.users.contains_key(h) {
                        return Err(not_found(h));
                    }
                }
                if pms.contains(reviewer) {
                    return Err(refused(
                        Refusal::Invalid,
                        "a reviewer is never assigned to review themselves",
                    ));
                }
                if pms.is_empty() {
                    dir.assignments.remove(reviewer);
                } else {
                    dir.assignments.insert(reviewer.clone(), pms.clone());
                }
            }
        }
    }
    Ok(())
}

fn not_found(handle: &str) -> UserError {
    refused(Refusal::NotFound, format!("no user `{handle}`"))
}

/// The `users_invariants()` trigger (migration 0015), on the directory as it would be after
/// a commit.
pub fn check_invariants(
    dir: &Directory,
    touched: &Touched,
    now: DateTime<Utc>,
    rules: &Rules,
) -> UserResult<()> {
    if !dir.users.is_empty() && !dir.has_active_admin() {
        return Err(refused(
            Refusal::LastAdmin,
            "PM OS needs at least one active admin: this would leave none",
        ));
    }
    let people: Vec<&User> = match touched {
        Touched::Everyone => dir.users.values().collect(),
        Touched::People(set) => set.iter().filter_map(|h| dir.get(h)).collect(),
    };
    if !dir.exception_in_force(now) {
        let offenders: Vec<&str> = people
            .iter()
            .filter(|u| u.active() && u.dual_role())
            .map(|u| u.handle.as_str())
            .collect();
        if !offenders.is_empty() {
            return Err(refused(
                Refusal::DualRole,
                format!(
                    "{} would hold admin and auditor with no pilot exception in force",
                    offenders.join(", ")
                ),
            ));
        }
    }
    if let Some(evals) = people.iter().find(|u| u.handle == EVALS) {
        if evals.roles.keys().any(|r| *r != UserRole::Pm) {
            return Err(refused(
                Refusal::EvalsAccount,
                "the evals account may hold pm only",
            ));
        }
        let allowed = |k: &IdentityKind| rules.evals_slack_allowed && *k == IdentityKind::Slack;
        if evals.identities.keys().any(|k| !allowed(k)) {
            return Err(refused(
                Refusal::EvalsAccount,
                "the evals account never has a Slack, Entra, Atlassian or GitHub key",
            ));
        }
    }
    Ok(())
}

// --- Planning --------------------------------------------------------------------------------

/// Builds one plan. Used by [`plan`] and the import (`import.rs`).
pub(crate) struct Builder<'a> {
    pub(crate) dir: &'a Directory,
    pub(crate) actor: &'a Actor,
    pub(crate) now: DateTime<Utc>,
    writes: Vec<Write>,
    changes: Vec<RoleChange>,
    events: Vec<UserEvent>,
    pub(crate) lines: Vec<String>,
    touched: Touched,
    effects: Vec<Effect>,
}

/// The most PMs one reviewer may be assigned (a bound on one write, not a policy).
pub const MAX_ASSIGNED_PMS: usize = 200;

impl<'a> Builder<'a> {
    fn new(dir: &'a Directory, actor: &'a Actor, now: DateTime<Utc>) -> Self {
        Self {
            dir,
            actor,
            now,
            writes: Vec::new(),
            changes: Vec::new(),
            events: Vec::new(),
            lines: Vec::new(),
            touched: Touched::People(BTreeSet::new()),
            effects: Vec::new(),
        }
    }

    fn touch(&mut self, handle: &str) {
        if let Touched::People(set) = &mut self.touched {
            set.insert(handle.to_string());
        }
    }

    fn touch_everyone(&mut self) {
        self.touched = Touched::Everyone;
    }

    pub(crate) fn change(
        &mut self,
        target: &str,
        change: ChangeKind,
        role: Option<UserRole>,
        detail: Option<String>,
    ) {
        self.changes.push(RoleChange {
            id: 0,
            at: self.now,
            actor: self.actor.name.clone(),
            via: self.actor.via,
            host_user: self
                .actor
                .host_user
                .as_ref()
                .map(|h| h.as_str().to_string()),
            target: target.to_string(),
            change,
            role,
            detail,
        });
    }

    pub(crate) fn event(&mut self, action: &'static str, target: &str, parts: Vec<String>) {
        self.events.push(UserEvent {
            action,
            target: target.to_string(),
            outcome: Outcome::Ok,
            detail: detail::join(self.actor.host_user.as_ref(), &parts),
        });
    }

    fn need(&self, handle: &str) -> UserResult<&'a User> {
        self.dir.get(handle).ok_or_else(|| not_found(handle))
    }

    fn host_only(&self, what: &str) -> UserResult<()> {
        if self.actor.via == Via::Host {
            Ok(())
        } else {
            Err(refused(
                Refusal::HostOnly,
                format!("{what} is a host command (pmos-admin users …)"),
            ))
        }
    }

    /// A web or Slack actor must be an active admin in the directory: the tool server checks
    /// the actor in its own tables, never the assertion's roles (§6).
    fn actor_is_admin(&self) -> UserResult<()> {
        if matches!(self.actor.via, Via::Host | Via::Import) {
            return Ok(());
        }
        match self.dir.get(&self.actor.name) {
            Some(u) if u.active() && u.has(UserRole::Admin) => Ok(()),
            _ => Err(refused(
                Refusal::Forbidden,
                format!("`{}` isn't an active admin", self.actor.name),
            )),
        }
    }

    /// No one changes their own roles, status or addresses (§6).
    fn not_self(&self, target: &str) -> UserResult<()> {
        if matches!(self.actor.via, Via::Web | Via::Bot) && self.actor.name == target {
            return Err(refused(
                Refusal::SelfChange,
                "no one changes their own roles, status or addresses",
            ));
        }
        Ok(())
    }

    pub(crate) fn grant(&mut self, handle: &str, role: UserRole) {
        self.writes.push(Write::InsertRole {
            handle: handle.to_string(),
            role,
            grant: RoleGrant {
                at: self.now,
                by: self.actor.name.clone(),
            },
        });
        self.touch(handle);
        self.change(handle, ChangeKind::Grant, Some(role), None);
    }

    fn revoke(&mut self, handle: &str, role: UserRole, detail: Option<&str>) {
        self.writes.push(Write::DeleteRole {
            handle: handle.to_string(),
            role,
        });
        self.touch(handle);
        self.change(
            handle,
            ChangeKind::Revoke,
            Some(role),
            detail.map(String::from),
        );
    }

    fn link(&mut self, handle: &str, kind: IdentityKind, issuer: &str, subject: &str, host: bool) {
        self.writes.push(Write::InsertIdentity {
            handle: handle.to_string(),
            identity: Identity {
                kind,
                issuer: issuer.to_string(),
                subject: subject.to_string(),
                linked_at: self.now,
                linked_by: self.actor.name.clone(),
                linked_via: self.actor.via,
                host_confirmed: host,
            },
        });
        self.touch(handle);
        let detail = if host {
            format!("{} host", kind.as_str())
        } else {
            kind.as_str().to_string()
        };
        self.change(handle, ChangeKind::Link, None, Some(detail));
    }

    fn unlink(&mut self, handle: &str, kind: IdentityKind) {
        self.writes.push(Write::DeleteIdentity {
            handle: handle.to_string(),
            kind,
        });
        self.touch(handle);
        self.change(
            handle,
            ChangeKind::Unlink,
            None,
            Some(kind.as_str().to_string()),
        );
    }

    fn update(&mut self, user: User) {
        self.touch(&user.handle);
        self.writes.push(Write::UpdateUser(Box::new(user)));
    }

    /// Create `user` with its roles and Slack key. `confirmed`: the sign-in name counts as
    /// confirmed (an admin or the host entered it, or a reviewed file did).
    pub(crate) fn create(
        &mut self,
        user: &NewUser,
        confirmed: bool,
        with_slack: bool,
    ) -> UserResult<()> {
        if !valid_handle(&user.handle) {
            return Err(refused(
                Refusal::Invalid,
                format!(
                    "`{}` isn't a usable handle: lowercase letters, digits, `.`, `_` and `-`, at most 64, not bots.*",
                    user.handle
                ),
            ));
        }
        if self.dir.get(&user.handle).is_some() {
            return Err(refused(
                Refusal::Exists,
                format!(
                    "`{}` is already a user (handles are never reused)",
                    user.handle
                ),
            ));
        }
        let sign_in = user
            .sign_in_name
            .as_deref()
            .map(|s| address(s, "sign-in name"))
            .transpose()?;
        let atlassian = user
            .atlassian_email
            .as_deref()
            .map(|s| address(s, "Atlassian email"))
            .transpose()?;
        let github = user.github_login.as_deref().map(github_login).transpose()?;
        let slack = match (&user.slack_id, with_slack) {
            (Some(s), true) => Some(slack_id(s)?),
            _ => None,
        };
        let confirm = confirmed && sign_in.is_some();
        let row = User {
            handle: user.handle.clone(),
            display_name: clean_display_name(&user.display_name),
            sign_in_name: sign_in,
            sign_in_name_confirmed_at: confirm.then_some(self.now),
            sign_in_name_confirmed_by: confirm.then(|| self.actor.name.clone()),
            atlassian_email: atlassian,
            github_login: github,
            status: Status::Active,
            created_at: self.now,
            created_by: self.actor.name.clone(),
            created_via: self.actor.via,
            deactivated_at: None,
            deactivated_by: None,
            roles: Default::default(),
            identities: Default::default(),
        };
        self.touch(&row.handle);
        self.writes.push(Write::InsertUser(Box::new(row)));
        self.change(&user.handle, ChangeKind::Create, None, None);
        for role in &user.roles {
            self.grant(&user.handle, *role);
        }
        if let Some(slack) = slack {
            self.link(&user.handle, IdentityKind::Slack, "", &slack, false);
        }
        Ok(())
    }

    /// Simulate the writes and check the result, as the database would at commit.
    pub(crate) fn finish(self, rules: &Rules) -> UserResult<Plan> {
        let mut next = self.dir.clone();
        apply_writes(&mut next, &self.writes)?;
        check_invariants(&next, &self.touched, self.now, rules)?;
        if !self.writes.is_empty() {
            next.version += 1;
        }
        Ok(Plan {
            writes: self.writes,
            changes: self.changes,
            events: self.events,
            lines: self.lines,
            touched: self.touched,
            effects: self.effects,
            next,
        })
    }
}

fn normalize(key: &EntraKey) -> UserResult<EntraKey> {
    Ok(EntraKey {
        tid: guid(&key.tid, "tenant id")?,
        oid: guid(&key.oid, "object id (--oid)")?,
    })
}

/// The `denied` event a refused write records.
pub fn denied_event(actor: &Actor, op: &Op, refusal: Refusal) -> UserEvent {
    UserEvent {
        action: op.action(),
        target: op.target(),
        outcome: Outcome::Denied,
        detail: detail::join(actor.host_user.as_ref(), &[refusal.code().to_string()]),
    }
}

/// Plan `op` by `actor` against `dir` at `now`, and check the result.
pub fn plan(
    dir: &Directory,
    actor: &Actor,
    op: &Op,
    now: DateTime<Utc>,
    rules: &Rules,
) -> UserResult<Plan> {
    plan_expecting(dir, actor, op, now, rules, None)
}

/// [`plan`], for a write made against the directory at `expected_version` (slice U5): once
/// the actor and the change pass every rule of their own, a directory whose version isn't
/// `expected_version` refuses it [`Refusal::Stale`], before the invariants are checked. So a
/// refusal that doesn't depend on what moved (not an admin, oneself, host only) is reported
/// as itself, and a write that would have gone through on a directory that moved is `stale`.
pub fn plan_expecting(
    dir: &Directory,
    actor: &Actor,
    op: &Op,
    now: DateTime<Utc>,
    rules: &Rules,
    expected_version: Option<i64>,
) -> UserResult<Plan> {
    let b = build(dir, actor, op, now, rules)?;
    if let Some(expected) = expected_version {
        if expected != dir.version {
            return Err(refused(
                Refusal::Stale,
                "the team changed since this page loaded: reload it and try again",
            ));
        }
    }
    b.finish(rules)
}

/// Every op's own rules, into a builder not yet checked against the invariants.
fn build<'a>(
    dir: &'a Directory,
    actor: &'a Actor,
    op: &Op,
    now: DateTime<Utc>,
    rules: &Rules,
) -> UserResult<Builder<'a>> {
    if actor.via == Via::Host && actor.host_user.is_none() {
        return Err(refused(
            Refusal::Invalid,
            "a host command needs PMOS_HOST_USER (the OS user who ran it)",
        ));
    }
    let mut b = Builder::new(dir, actor, now);
    match op {
        Op::BootstrapAdmin { user, entra } => {
            b.host_only("bootstrap-admin")?;
            if dir.has_active_admin() {
                return Err(refused(
                    Refusal::AdminExists,
                    "an active admin already exists: use grant-admin instead",
                ));
            }
            let mut user = user.clone();
            user.roles = [UserRole::Pm, UserRole::Admin].into();
            b.create(&user, true, true)?;
            if let Some(key) = entra {
                let key = normalize(key)?;
                b.link(&user.handle, IdentityKind::Entra, &key.tid, &key.oid, true);
            }
            let roles: Vec<UserRole> = user.roles.iter().copied().collect();
            b.event(
                "users.bootstrap_admin",
                &user.handle,
                vec![detail::roles_part(&roles)],
            );
            b.lines.push(format!(
                "created `{}` (admin, pm), the first admin{}",
                user.handle,
                if entra.is_some() {
                    ", with a host-confirmed Entra link"
                } else {
                    ", with no Entra link (set-entra when Entra sign-in is set up)"
                }
            ));
        }
        Op::CreateUser { user } => {
            match actor.via {
                Via::Bot => {
                    if user.roles.contains(&UserRole::Admin) {
                        return Err(refused(
                            Refusal::AdminNotFromSlack,
                            "admin is never granted from Slack",
                        ));
                    }
                    if user.roles != BTreeSet::from([UserRole::Pm]) {
                        return Err(refused(
                            Refusal::RoleNotFromSlack,
                            "a join approved in Slack gets pm only",
                        ));
                    }
                }
                Via::Web | Via::Host => {
                    if user.roles.contains(&UserRole::Admin) {
                        return Err(refused(
                            Refusal::HostOnly,
                            "admin is granted only with grant-admin on the host",
                        ));
                    }
                }
                _ => return Err(refused(Refusal::Invalid, "not a client that creates users")),
            }
            b.actor_is_admin()?;
            let confirmed = actor.via != Via::Bot;
            b.create(user, confirmed, true)?;
            let roles: Vec<UserRole> = user.roles.iter().copied().collect();
            let mut parts = vec![detail::roles_part(&roles)];
            if !confirmed {
                parts.push("sign_in unconfirmed".into());
            }
            b.event("users.create", &user.handle, parts);
            b.lines.push(format!("created `{}`", user.handle));
        }
        Op::SetRoles { handle, roles } => {
            match actor.via {
                Via::Bot => {
                    return Err(refused(
                        Refusal::RoleNotFromSlack,
                        "roles never change from Slack",
                    ))
                }
                Via::Web | Via::Host => {}
                _ => return Err(refused(Refusal::Invalid, "not a client that changes roles")),
            }
            b.actor_is_admin()?;
            b.not_self(handle)?;
            let user = b.need(handle)?;
            let current = user.role_set();
            let added: Vec<UserRole> = roles.difference(&current).copied().collect();
            let removed: Vec<UserRole> = current.difference(roles).copied().collect();
            if added.contains(&UserRole::Admin) {
                return Err(refused(
                    Refusal::HostOnly,
                    "admin is granted only with grant-admin on the host",
                ));
            }
            if added.is_empty() && removed.is_empty() {
                return Err(refused(Refusal::Unchanged, "those are already the roles"));
            }
            for r in &added {
                b.grant(handle, *r);
            }
            for r in &removed {
                b.revoke(handle, *r, None);
            }
            let delta = detail::role_delta(&added, &removed);
            b.event("users.roles", handle, vec![delta.clone()]);
            b.lines.push(format!("`{handle}`: {delta}"));
        }
        Op::GrantAdmin { handle, entra } => {
            b.host_only("grant-admin")?;
            let user = b.need(handle)?;
            if !user.active() {
                return Err(refused(
                    Refusal::NotActive,
                    format!("`{handle}` is deactivated: reactivate them first"),
                ));
            }
            if user.has(UserRole::Admin) {
                return Err(refused(
                    Refusal::Unchanged,
                    format!("`{handle}` is already an admin"),
                ));
            }
            let existing = user.identity(IdentityKind::Entra);
            match entra {
                AdminEntra::Oid(key) => {
                    let key = normalize(key)?;
                    match existing {
                        Some(e) if e.issuer == key.tid && e.subject == key.oid => {
                            if !e.host_confirmed {
                                b.unlink(handle, IdentityKind::Entra);
                                b.link(handle, IdentityKind::Entra, &key.tid, &key.oid, true);
                                b.lines.push(format!("confirmed `{handle}`'s Entra link on the host"));
                            }
                        }
                        Some(e) => {
                            return Err(refused(
                                Refusal::EntraMismatch,
                                format!(
                                    "`{handle}` has {} Entra link with a different oid: check the oid, or run unset-entra first",
                                    if e.host_confirmed { "a host-confirmed" } else { "an automatic" }
                                ),
                            ))
                        }
                        None => b.link(handle, IdentityKind::Entra, &key.tid, &key.oid, true),
                    }
                }
                AdminEntra::Keep => match existing {
                    Some(e) if e.host_confirmed => {}
                    Some(_) => {
                        return Err(refused(
                            Refusal::EntraNeedsHost,
                            format!("`{handle}`'s Entra link isn't host-confirmed: give --oid"),
                        ))
                    }
                    None => {
                        return Err(refused(
                            Refusal::EntraNeedsHost,
                            format!("`{handle}` has no Entra link: give --oid"),
                        ))
                    }
                },
            }
            b.grant(handle, UserRole::Admin);
            b.event("users.grant_admin", handle, vec!["+admin".into()]);
            b.lines.push(format!("`{handle}` is now an admin"));
        }
        Op::RevokeAdmin { handle } => {
            match actor.via {
                Via::Host | Via::Web => {}
                Via::Bot => {
                    return Err(refused(
                        Refusal::AdminNotFromSlack,
                        "admin never changes from Slack",
                    ))
                }
                _ => return Err(refused(Refusal::Invalid, "not a client that changes roles")),
            }
            b.actor_is_admin()?;
            b.not_self(handle)?;
            if !b.need(handle)?.has(UserRole::Admin) {
                return Err(refused(
                    Refusal::Unchanged,
                    format!("`{handle}` isn't an admin"),
                ));
            }
            b.revoke(handle, UserRole::Admin, None);
            b.event("users.roles", handle, vec!["-admin".into()]);
            b.lines.push(format!("`{handle}` is no longer an admin"));
        }
        Op::SetEntra { handle, key } => {
            b.host_only("set-entra")?;
            let key = normalize(key)?;
            let user = b.need(handle)?;
            match user.identity(IdentityKind::Entra) {
                Some(e) if e.issuer == key.tid && e.subject == key.oid && e.host_confirmed => {
                    return Err(refused(
                        Refusal::Unchanged,
                        format!("`{handle}` already has that host-confirmed Entra link"),
                    ))
                }
                Some(_) => b.unlink(handle, IdentityKind::Entra),
                None => {}
            }
            b.link(handle, IdentityKind::Entra, &key.tid, &key.oid, true);
            b.event("users.set_entra", handle, vec!["entra".into()]);
            b.lines
                .push(format!("`{handle}` has a host-confirmed Entra link"));
        }
        Op::UnsetEntra { handle } => unlink_identity(&mut b, handle, IdentityKind::Entra)?,
        Op::Unlink { handle, kind } => unlink_identity(&mut b, handle, *kind)?,
        Op::SetField {
            handle,
            field,
            value,
        } => set_fields(&mut b, handle, &[(*field, value.clone())])?,
        Op::SetFields { handle, values } => set_fields(&mut b, handle, values)?,
        Op::AssignReviewer { reviewer, pms } => assign_reviewer(&mut b, reviewer, pms)?,
        Op::Deactivate { handle } => {
            match actor.via {
                Via::Host | Via::Web | Via::Bot => {}
                _ => return Err(refused(Refusal::Invalid, "not a client that deactivates")),
            }
            b.actor_is_admin()?;
            b.not_self(handle)?;
            let user = b.need(handle)?;
            if actor.via == Via::Bot && user.protected() {
                return Err(refused(
                    Refusal::ProtectedNotFromSlack,
                    "an Admin or Auditor is never deactivated from Slack",
                ));
            }
            if !user.active() {
                return Err(refused(
                    Refusal::Unchanged,
                    format!("`{handle}` is already deactivated"),
                ));
            }
            if actor.via.capped() {
                let since = now - super::deactivation_window();
                let recent = dir
                    .capped_deactivations
                    .iter()
                    .filter(|at| **at > since && **at <= now)
                    .count();
                if recent >= rules.deactivations_per_hour as usize {
                    return Err(refused(
                        Refusal::DeactivationLimit,
                        "PM OS has stopped deactivations for an hour. Someone with host access can continue with `pmos-admin users deactivate`",
                    ));
                }
            }
            let mut row = user.clone();
            row.status = Status::Deactivated;
            row.deactivated_at = Some(now);
            row.deactivated_by = Some(actor.name.clone());
            b.update(row);
            b.change(handle, ChangeKind::Deactivate, None, None);
            // §6: sessions end and linked accounts are suspended in the same transaction;
            // they are deleted after the grace unless the person is reactivated first.
            b.effects.push(Effect::RevokeSessions(handle.clone()));
            b.effects.push(Effect::SuspendCredentials(handle.clone()));
            b.change(handle, ChangeKind::SuspendCredentials, None, None);
            // Review item 1: deactivation revokes admin; reactivation never restores it.
            if user.has(UserRole::Admin) {
                b.revoke(handle, UserRole::Admin, Some("on deactivation"));
                b.event("users.roles", handle, vec!["-admin on deactivation".into()]);
                b.lines.push(format!(
                    "`{handle}` was an admin: admin is revoked, and reactivation won't restore it"
                ));
            }
            b.event(
                "users.deactivate",
                handle,
                vec!["credentials suspended".into()],
            );
            b.lines.push(format!(
                "`{handle}` deactivated: their sessions ended and their linked accounts are suspended; \
                 reactivating within {}h restores them",
                super::CREDENTIAL_GRACE_HOURS
            ));
        }
        Op::Reactivate { handle } => {
            match actor.via {
                Via::Host | Via::Web => {}
                Via::Bot => {
                    return Err(refused(
                        Refusal::NotFromSlack,
                        "reactivation is in the web app or on the host",
                    ))
                }
                _ => return Err(refused(Refusal::Invalid, "not a client that reactivates")),
            }
            b.actor_is_admin()?;
            b.not_self(handle)?;
            let user = b.need(handle)?;
            if user.active() {
                return Err(refused(Refusal::Unchanged, format!("`{handle}` is active")));
            }
            let within_grace = user
                .deactivated_at
                .is_some_and(|at| now < at + super::credential_grace());
            let mut row = user.clone();
            row.status = Status::Active;
            row.deactivated_at = None;
            row.deactivated_by = None;
            b.update(row);
            b.change(handle, ChangeKind::Reactivate, None, None);
            if within_grace {
                b.effects.push(Effect::RestoreCredentials(handle.clone()));
                b.event(
                    "users.reactivate",
                    handle,
                    vec!["credentials restored".into()],
                );
            } else {
                // After the grace: the accounts and held drafts are gone, whether or not the
                // sweeper got to them first.
                b.effects
                    .push(Effect::DeleteSuspendedCredentials(handle.clone()));
                b.effects.push(Effect::CloseHeldDrafts(handle.clone()));
                b.change(handle, ChangeKind::DeleteCredentials, None, None);
                b.event(
                    "users.reactivate",
                    handle,
                    vec!["credentials deleted".into()],
                );
                b.lines.push(format!(
                    "`{handle}` was deactivated more than {}h ago: their accounts must be linked again",
                    super::CREDENTIAL_GRACE_HOURS
                ));
            }
            b.lines.push(format!(
                "`{handle}` reactivated with their kept roles (never admin: grant-admin on the host)"
            ));
        }
        Op::ExceptionOn { until } => {
            b.host_only("the pilot exception")?;
            if let Some(e) = &dir.exception {
                return Err(refused(
                    Refusal::ExceptionOpen,
                    format!(
                        "the pilot exception is already on, until {}{}: run `off` first",
                        detail::date(e.expires_at),
                        if e.in_force(now) { "" } else { " (lapsed)" }
                    ),
                ));
            }
            if *until <= now {
                return Err(refused(Refusal::Invalid, "--until must be in the future"));
            }
            if *until > now + Duration::days(MAX_EXCEPTION_DAYS) {
                return Err(refused(
                    Refusal::ExceptionTooLong,
                    format!("--until is at most {MAX_EXCEPTION_DAYS} days ahead"),
                ));
            }
            let by = actor
                .host_user
                .as_ref()
                .map(|h| h.as_str().to_string())
                .unwrap_or_else(|| actor.name.clone());
            b.writes.push(Write::OpenException(PilotException {
                enabled_at: now,
                enabled_by: by,
                expires_at: *until,
            }));
            b.touch_everyone();
            let date = detail::date(*until);
            b.change(
                "*",
                ChangeKind::ExceptionOn,
                None,
                Some(format!("admin_and_auditor until {date}")),
            );
            b.event(
                "users.pilot_exception",
                "*",
                vec![format!("on until {date}")],
            );
            b.lines.push(format!(
                "pilot exception on: one person may hold admin and auditor until {}",
                until.format("%Y-%m-%d %H:%M UTC")
            ));
        }
        Op::ExceptionOff { revoke_auditor } => {
            b.host_only("the pilot exception")?;
            if dir.exception.is_none() {
                return Err(refused(
                    Refusal::NoException,
                    "the pilot exception isn't on",
                ));
            }
            let holders: Vec<String> = dir
                .dual_role_holders()
                .iter()
                .map(|u| u.handle.clone())
                .collect();
            if !holders.is_empty() && !revoke_auditor {
                return Err(refused(
                    Refusal::DualRole,
                    format!(
                        "{} hold admin and auditor: revoke one role first, or run off --revoke-auditor",
                        holders.join(", ")
                    ),
                ));
            }
            for h in &holders {
                b.revoke(h, UserRole::Auditor, Some("exception off"));
                b.lines.push(format!("`{h}`: auditor revoked (admin kept)"));
            }
            let by = actor
                .host_user
                .as_ref()
                .map(|h| h.as_str().to_string())
                .unwrap_or_else(|| actor.name.clone());
            b.writes.push(Write::EndException {
                ended_at: now,
                ended_by: by,
            });
            b.touch_everyone();
            b.change("*", ChangeKind::ExceptionOff, None, None);
            let mut parts = vec!["off".to_string()];
            if !holders.is_empty() {
                parts.push(format!("auditor revoked {}", holders.len()));
            }
            b.event("users.pilot_exception", "*", parts);
            b.lines.push("pilot exception off".into());
        }
        Op::Import(req) => super::import::plan_import(&mut b, req, rules)?,
        Op::LinkAccount {
            handle,
            kind,
            subject,
        } => {
            if actor.via != Via::Link || actor.name != *handle {
                return Err(refused(
                    Refusal::Invalid,
                    "an account link is the person's own OAuth sign-in",
                ));
            }
            if !matches!(kind, IdentityKind::Atlassian | IdentityKind::Github) {
                return Err(refused(
                    Refusal::Invalid,
                    "only Atlassian and GitHub accounts link this way",
                ));
            }
            let user = b.need(handle)?;
            if !user.active() {
                return Err(refused(
                    Refusal::NotActive,
                    format!("`{handle}` is deactivated"),
                ));
            }
            let subject = subject.trim();
            if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
                return Err(refused(Refusal::Invalid, "not an account id"));
            }
            match user.identity(*kind) {
                Some(i) if i.subject == subject => {
                    return Err(refused(
                        Refusal::Unchanged,
                        "that account is already the linked one",
                    ))
                }
                Some(_) => {
                    return Err(refused(
                        Refusal::DifferentAccount,
                        format!(
                            "`{handle}` linked a different {} account first: an admin unlinks it before another can be linked",
                            if *kind == IdentityKind::Atlassian { "Atlassian" } else { "GitHub" }
                        ),
                    ))
                }
                None => {}
            }
            b.link(handle, *kind, "", subject, false);
            b.event("users.identity_link", handle, vec![kind.as_str().into()]);
            b.lines
                .push(format!("`{handle}`: {} account linked", kind.as_str()));
        }
        Op::EndGrace { handle } => {
            if actor.via != Via::System {
                return Err(refused(Refusal::Invalid, "the sweeper is the system's"));
            }
            let user = b.need(handle)?;
            let due = !user.active()
                && user
                    .deactivated_at
                    .is_some_and(|at| at + super::credential_grace() <= now);
            if !due {
                return Err(refused(
                    Refusal::NotDue,
                    format!("`{handle}` isn't past the grace after a deactivation"),
                ));
            }
            b.effects
                .push(Effect::DeleteSuspendedCredentials(handle.clone()));
            b.effects.push(Effect::CloseHeldDrafts(handle.clone()));
            b.change(handle, ChangeKind::DeleteCredentials, None, None);
            b.event(
                "users.deactivate",
                handle,
                vec!["credentials deleted".into()],
            );
            b.lines.push(format!(
                "`{handle}`: linked accounts deleted and held drafts closed"
            ));
        }
    }
    Ok(b)
}

/// Remove `handle`'s identity of `kind` (§6, §7). The host may remove any; an admin in web may
/// remove another person's, but an Admin's or Auditor's Entra link only on the host. An
/// Atlassian or GitHub identity takes its linked account with it, so another account can be
/// linked (the email or login then decides the next first link).
fn unlink_identity(b: &mut Builder<'_>, handle: &str, kind: IdentityKind) -> UserResult<()> {
    match b.actor.via {
        Via::Host => {}
        Via::Web => {
            b.actor_is_admin()?;
            b.not_self(handle)?;
            if kind == IdentityKind::Entra && b.need(handle)?.protected() {
                return Err(refused(
                    Refusal::HostOnly,
                    "an Admin's or Auditor's Entra link changes on the host only",
                ));
            }
        }
        Via::Bot => {
            return Err(refused(
                Refusal::NotFromSlack,
                "identities never change from Slack",
            ))
        }
        _ => return Err(refused(Refusal::Invalid, "not a client that unlinks")),
    }
    let provider = match kind {
        IdentityKind::Entra => None,
        IdentityKind::Atlassian => Some(crate::vault::Provider::Atlassian),
        IdentityKind::Github => Some(crate::vault::Provider::Github),
        IdentityKind::Slack => {
            return Err(refused(
                Refusal::Invalid,
                "a Slack key isn't unlinked: it comes with the join or the import",
            ))
        }
    };
    if b.need(handle)?.identity(kind).is_none() {
        return Err(refused(
            Refusal::Unchanged,
            format!("`{handle}` has no {} link", kind.as_str()),
        ));
    }
    b.unlink(handle, kind);
    if let Some(p) = provider {
        b.effects
            .push(Effect::DeleteCredential(handle.to_string(), p));
    }
    b.event("users.identity_unlink", handle, vec![kind.as_str().into()]);
    b.lines
        .push(format!("`{handle}`'s {} link removed", kind.as_str()));
    Ok(())
}

/// Set `values` on `handle` in one write (§6 "When addresses may change"). From the web: an
/// active admin, never an address of their own, never an Admin's or Auditor's address (host
/// only), an Atlassian email or GitHub login only while no account of that kind is linked
/// (after the first link its account id decides). The display name may be set by an admin
/// for anyone, themselves included. Setting the sign-in name also confirms it: the same
/// value, unconfirmed, is recorded as a confirmation; the same value, confirmed, is no change.
fn set_fields(b: &mut Builder<'_>, handle: &str, values: &[(Field, String)]) -> UserResult<()> {
    let user = b.need(handle)?;
    match b.actor.via {
        Via::Host => {}
        Via::Web => {
            b.actor_is_admin()?;
            if values.iter().any(|(f, _)| f.is_address()) {
                b.not_self(handle)?;
                if user.protected() {
                    return Err(refused(
                        Refusal::HostOnly,
                        "an Admin's or Auditor's addresses change on the host only",
                    ));
                }
            }
        }
        Via::Bot => {
            return Err(refused(
                Refusal::NotFromSlack,
                "addresses never change from Slack",
            ))
        }
        _ => return Err(refused(Refusal::Invalid, "not a client that sets fields")),
    }
    if values.is_empty() {
        return Err(refused(
            Refusal::Invalid,
            "name at least one field to change",
        ));
    }
    let mut seen = BTreeSet::new();
    if !values.iter().all(|(f, _)| seen.insert(*f)) {
        return Err(refused(Refusal::Invalid, "each field at most once"));
    }
    let web = b.actor.via == Via::Web;
    for (field, kind, name) in [
        (Field::AtlassianEmail, IdentityKind::Atlassian, "Atlassian"),
        (Field::GithubLogin, IdentityKind::Github, "GitHub"),
    ] {
        if web && seen.contains(&field) && user.identity(kind).is_some() {
            return Err(refused(
                Refusal::Linked,
                format!("the {name} account is linked: its account id decides; unlink it first"),
            ));
        }
    }
    let mut row = user.clone();
    let mut changed: Vec<(Field, ChangeKind)> = Vec::new();
    for (field, raw) in values {
        match field {
            Field::SignInName => {
                let value = address(raw, field.label())?;
                let same = row.sign_in_name.as_deref() == Some(value.as_str());
                if same && row.sign_in_name_confirmed_at.is_some() {
                    continue;
                }
                row.sign_in_name = Some(value);
                // Setting it is also the confirmation (§6).
                row.sign_in_name_confirmed_at = Some(b.now);
                row.sign_in_name_confirmed_by = Some(b.actor.name.clone());
                let kind = if same {
                    ChangeKind::ConfirmSignInName
                } else {
                    ChangeKind::SetField
                };
                changed.push((*field, kind));
            }
            Field::AtlassianEmail => {
                let value = address(raw, field.label())?;
                if row.atlassian_email.as_deref() != Some(value.as_str()) {
                    row.atlassian_email = Some(value);
                    changed.push((*field, ChangeKind::SetField));
                }
            }
            Field::GithubLogin => {
                let value = github_login(raw)?;
                if row.github_login.as_deref() != Some(value.as_str()) {
                    row.github_login = Some(value);
                    changed.push((*field, ChangeKind::SetField));
                }
            }
            Field::DisplayName => {
                let value = clean_display_name(raw);
                if row.display_name != value {
                    row.display_name = value;
                    changed.push((*field, ChangeKind::SetField));
                }
            }
        }
    }
    if changed.is_empty() {
        return Err(refused(
            Refusal::Unchanged,
            format!("those are already `{handle}`'s values"),
        ));
    }
    b.update(row);
    let mut parts = Vec::new();
    for (field, kind) in &changed {
        b.change(handle, *kind, None, Some(field.as_str().to_string()));
        parts.push(if *kind == ChangeKind::ConfirmSignInName {
            "sign_in_name confirmed".to_string()
        } else {
            field.as_str().to_string()
        });
        b.lines.push(format!("`{handle}`: {} set", field.label()));
    }
    b.event("users.fields", handle, parts);
    Ok(())
}

/// The whole set of PMs assigned to `reviewer` (§8). From the web only, by an active admin,
/// never with themselves as the reviewer. Each PM added must be an active user holding `pm`,
/// and the reviewer an active user holding `reviewer`, unless the set is empty: clearing is
/// always allowed (a reviewer who lost the role keeps inert assignments until cleared).
fn assign_reviewer(b: &mut Builder<'_>, reviewer: &str, pms: &BTreeSet<String>) -> UserResult<()> {
    match b.actor.via {
        Via::Web => {
            b.actor_is_admin()?;
            b.not_self(reviewer)?;
        }
        Via::Bot => {
            return Err(refused(
                Refusal::NotFromSlack,
                "reviewers are assigned in the web app",
            ))
        }
        _ => {
            return Err(refused(
                Refusal::Invalid,
                "reviewers are assigned in the web app",
            ))
        }
    }
    let rev = b.need(reviewer)?;
    if pms.len() > MAX_ASSIGNED_PMS {
        return Err(refused(
            Refusal::Invalid,
            format!("at most {MAX_ASSIGNED_PMS} PMs per reviewer"),
        ));
    }
    if pms.contains(reviewer) {
        return Err(refused(
            Refusal::Invalid,
            "a reviewer is never assigned to review themselves",
        ));
    }
    if !pms.is_empty() {
        if !rev.active() {
            return Err(refused(
                Refusal::NotActive,
                format!("`{reviewer}` is deactivated"),
            ));
        }
        if !rev.has(UserRole::Reviewer) {
            return Err(refused(
                Refusal::NotReviewer,
                format!("`{reviewer}` doesn't hold the reviewer role"),
            ));
        }
    }
    let current = b.dir.reviews(reviewer);
    for pm in pms.difference(&current) {
        let u = b.need(pm)?;
        if !u.active() {
            return Err(refused(
                Refusal::NotActive,
                format!("`{pm}` is deactivated"),
            ));
        }
        if !u.has(UserRole::Pm) {
            return Err(refused(
                Refusal::NotPm,
                format!("`{pm}` doesn't hold the pm role"),
            ));
        }
    }
    let added: Vec<String> = pms.difference(&current).cloned().collect();
    let removed: Vec<String> = current.difference(pms).cloned().collect();
    if added.is_empty() && removed.is_empty() {
        return Err(refused(
            Refusal::Unchanged,
            format!("those are already `{reviewer}`'s PMs"),
        ));
    }
    b.writes.push(Write::SetAssignments {
        reviewer: reviewer.to_string(),
        pms: pms.clone(),
        at: b.now,
        by: b.actor.name.clone(),
    });
    for pm in &added {
        b.change(
            reviewer,
            ChangeKind::Assign,
            Some(UserRole::Reviewer),
            Some(format!("pm {pm}")),
        );
    }
    for pm in &removed {
        b.change(
            reviewer,
            ChangeKind::Unassign,
            Some(UserRole::Reviewer),
            Some(format!("pm {pm}")),
        );
    }
    b.event(
        "reviewers.assign",
        reviewer,
        vec![format!("{} pms", pms.len())],
    );
    b.lines.push(format!(
        "`{reviewer}` reviews {} PM(s): {} added, {} removed",
        pms.len(),
        added.len(),
        removed.len()
    ));
    Ok(())
}
