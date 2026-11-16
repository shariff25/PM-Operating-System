//! A person in the PM OS user database, and the checks on what an admin may set.

use std::fmt;

use chrono::{DateTime, Utc};
use pmos_contract::model::{valid_handle, Role};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The roles an admin may grant in the web client. `skill-author` stays a `team.yaml`-only
/// role for now (it is granted by pull request, not here).
pub fn assignable(role: &Role) -> bool {
    matches!(
        role,
        Role::Admin | Role::Pm | Role::Reviewer | Role::Auditor
    )
}

/// Roles from an API body: known, assignable, at least one, no duplicates (sorted so two
/// equal sets compare equal).
pub fn parse_roles(names: &[String]) -> Result<Vec<Role>, &'static str> {
    let mut roles = Vec::with_capacity(names.len());
    for name in names {
        let role = Role::parse(name).ok_or("unknown_role")?;
        if !assignable(&role) {
            return Err("role_not_assignable");
        }
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    if roles.is_empty() {
        return Err("no_roles");
    }
    normalize_roles(&mut roles);
    Ok(roles)
}

pub fn normalize_roles(roles: &mut Vec<Role>) {
    roles.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    roles.dedup();
}

/// A handle as `team.yaml` accepts it, so the two can be kept in step.
pub fn check_handle(handle: &str) -> Result<(), &'static str> {
    if valid_handle(handle) {
        Ok(())
    } else {
        Err("invalid_handle")
    }
}

pub const DISPLAY_NAME_MAX: usize = 100;

/// A display name: trimmed, 1 to 100 characters, no control characters.
pub fn check_display_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > DISPLAY_NAME_MAX {
        return Err("invalid_display_name");
    }
    if name.chars().any(char::is_control) {
        return Err("invalid_display_name");
    }
    Ok(name.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
}

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// One account. The password hash never leaves the server: [`UserView`] is what the API
/// returns, and `Debug` leaves the hash out.
#[derive(Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    /// The PM OS handle, the same as in `team.yaml`. Unique; never changes.
    pub handle: String,
    pub display_name: String,
    /// Sorted, no duplicates.
    pub roles: Vec<Role>,
    pub status: UserStatus,
    /// An Argon2id PHC string.
    pub password_hash: String,
    /// Wrong passwords since the last successful sign-in.
    pub failed_attempts: u32,
    pub locked_until: Option<DateTime<Utc>>,
    /// Set by a new account or an admin reset: the person may do nothing but change it.
    pub must_change_password: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub password_changed_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("handle", &self.handle)
            .field("roles", &self.roles)
            .field("status", &self.status)
            .field("password_hash", &"<redacted>")
            .field("failed_attempts", &self.failed_attempts)
            .field("locked_until", &self.locked_until)
            .field("must_change_password", &self.must_change_password)
            .finish_non_exhaustive()
    }
}

impl User {
    pub fn new(
        handle: String,
        display_name: String,
        roles: Vec<Role>,
        password_hash: String,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            handle,
            display_name,
            roles,
            status: UserStatus::Active,
            password_hash,
            failed_attempts: 0,
            locked_until: None,
            must_change_password: true,
            created_at: at,
            updated_at: at,
            password_changed_at: at,
            last_login_at: None,
        }
    }

    pub fn is_admin(&self) -> bool {
        self.roles.contains(&Role::Admin)
    }

    pub fn is_active(&self) -> bool {
        self.status == UserStatus::Active
    }

    /// An active admin: the kind the last-admin rule keeps at least one of.
    pub fn is_active_admin(&self) -> bool {
        self.is_active() && self.is_admin()
    }

    pub fn is_locked(&self, now: DateTime<Utc>) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }

    pub fn view(&self, now: DateTime<Utc>) -> UserView {
        UserView {
            id: self.id,
            handle: self.handle.clone(),
            display_name: self.display_name.clone(),
            roles: self.roles.clone(),
            status: self.status,
            must_change_password: self.must_change_password,
            locked: self.is_locked(now),
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        }
    }
}

/// An account as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserView {
    pub id: Uuid,
    pub handle: String,
    pub display_name: String,
    pub roles: Vec<Role>,
    pub status: UserStatus,
    pub must_change_password: bool,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn roles_are_checked_and_normalized() {
        assert_eq!(
            parse_roles(&names(&["pm", "admin", "pm"])).unwrap(),
            vec![Role::Admin, Role::Pm]
        );
        assert_eq!(
            parse_roles(&names(&["reviewer", "auditor"])).unwrap(),
            vec![Role::Auditor, Role::Reviewer]
        );
        assert_eq!(parse_roles(&names(&["root"])), Err("unknown_role"));
        assert_eq!(
            parse_roles(&names(&["skill-author"])),
            Err("role_not_assignable")
        );
        assert_eq!(parse_roles(&[]), Err("no_roles"));
    }

    #[test]
    fn display_names_and_handles() {
        assert_eq!(check_display_name("  Ana Lima ").unwrap(), "Ana Lima");
        assert!(check_display_name("").is_err());
        assert!(check_display_name("a\u{0}b").is_err());
        assert!(check_display_name(&"x".repeat(101)).is_err());
        assert!(check_handle("ana.lima").is_ok());
        assert!(check_handle("Ana").is_err());
        assert!(check_handle("").is_err());
    }

    #[test]
    fn debug_hides_the_hash() {
        let u = User::new(
            "ana".into(),
            "Ana".into(),
            vec![Role::Pm],
            "$argon2id$v=19$secret".into(),
            Utc::now(),
        );
        assert!(!format!("{u:?}").contains("secret"));
    }
}
