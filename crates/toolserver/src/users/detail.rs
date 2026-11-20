//! The fixed formats of a user write's audit `detail` (spec §10) and `role_changes.detail`.
//!
//! An audit detail is parts joined by `"; "`: first `host_user <name>` for a host command,
//! then parts from a fixed grammar (role changes, field names, identity kinds, counts, a file
//! hash, a date, a refusal code). Never an address, an `oid`, a Slack id or free text.
//! [`is_safe`] recognises exactly these, so the admin and auditor reads
//! (`app/admin.rs`, `safe_detail`) can show them.

use super::{HostUser, IdentityKind, Refusal, UserRole};

/// The marker on an audit-log read (or export) by a person holding Admin and Auditor under
/// the pilot exception (§6).
pub const DUAL_ROLE: &str = "dual_role admin+auditor";

/// The audit detail: `host_user <name>` (when given) and `parts`, joined by `"; "`.
pub fn join(host_user: Option<&HostUser>, parts: &[String]) -> Option<String> {
    let all: Vec<String> = host_user
        .map(|h| format!("host_user {}", h.as_str()))
        .into_iter()
        .chain(parts.iter().cloned())
        .collect();
    (!all.is_empty()).then(|| all.join("; "))
}

/// `+reviewer -pm`: known role names only.
pub fn role_delta(added: &[UserRole], removed: &[UserRole]) -> String {
    added
        .iter()
        .map(|r| format!("+{}", r.as_str()))
        .chain(removed.iter().map(|r| format!("-{}", r.as_str())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `roles admin,pm`.
pub fn roles_part(roles: &[UserRole]) -> String {
    let names: Vec<&str> = roles.iter().map(|r| r.as_str()).collect();
    format!("roles {}", names.join(","))
}

/// The role changes' detail for an import: `team.yaml sha256 <hex> commit <sha|none>`, or
/// `bootstrap sha256 <hex>` for `import --bootstrap` (§3).
pub fn import_change(sha256: &str, commit: Option<&str>, bootstrap: bool) -> String {
    if bootstrap {
        format!("bootstrap sha256 {sha256}")
    } else {
        format!(
            "team.yaml sha256 {sha256} commit {}",
            commit.unwrap_or("none")
        )
    }
}

/// A pilot exception's date, as `YYYY-MM-DD` in UTC.
pub fn date(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

fn hex(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn count(s: &str) -> bool {
    !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() && s.len() == 10
}

fn role_token(t: &str) -> bool {
    t.strip_prefix('+')
        .or_else(|| t.strip_prefix('-'))
        .and_then(UserRole::parse)
        .is_some()
}

fn is_safe_part(part: &str) -> bool {
    const FIXED: [&str; 20] = [
        "off",
        // `role_changes` of password sign-in: which kind of code, which credential.
        "setup",
        "reset",
        "sign_in_name confirmed",
        // The Admin API's refusals before the store sees a write (`app/users.rs`).
        "bad_request",
        "assertion_required",
        "bad_assertion",
        "owns_bots",
        "bootstrap",
        "sign_in_name",
        "atlassian_email",
        "github_login",
        "display_name",
        "sign_in unconfirmed",
        "credentials suspended",
        "credentials deleted",
        "credentials restored",
        DUAL_ROLE,
        "host",
        "on deactivation",
    ];
    if FIXED.contains(&part)
        || IdentityKind::parse(part).is_some()
        || Refusal::parse(part).is_some()
    {
        return true;
    }
    if let Some(name) = part.strip_prefix("host_user ") {
        return HostUser::parse(name).is_some();
    }
    if let Some(list) = part.strip_prefix("roles ") {
        return list.split(',').all(|r| UserRole::parse(r).is_some());
    }
    if let Some(rest) = part.strip_prefix("sha256 ") {
        return hex(rest, 64, 64);
    }
    if let Some(rest) = part.strip_prefix("commit ") {
        return rest == "none" || hex(rest, 7, 40);
    }
    if let Some(rest) = part.strip_prefix("on until ") {
        return is_date(rest);
    }
    for prefix in ["created ", "unchanged ", "conflicts ", "auditor revoked "] {
        if let Some(rest) = part.strip_prefix(prefix) {
            return count(rest);
        }
    }
    // `reviewers.assign`: how many PMs the reviewer now has.
    if let Some(n) = part.strip_suffix(" pms") {
        return count(n);
    }
    // A role delta, possibly followed by `on deactivation`: `+reviewer -pm`.
    let tokens = part
        .strip_suffix(" on deactivation")
        .unwrap_or(part)
        .split(' ');
    let mut any = false;
    for t in tokens {
        if !role_token(t) {
            return false;
        }
        any = true;
    }
    any
}

/// Is `detail` one of the user writes' fixed formats?
pub fn is_safe(detail: &str) -> bool {
    !detail.is_empty() && detail.split("; ").all(is_safe_part)
}

/// The retention job's own audit detail (migration 0015): `purged <n> role changes`.
pub fn is_purge_detail(detail: &str) -> bool {
    detail
        .strip_prefix("purged ")
        .and_then(|r| r.strip_suffix(" role changes"))
        .is_some_and(|n| !n.is_empty() && n.len() <= 12 && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixed_formats_are_recognised_and_nothing_else() {
        let raj = HostUser::parse("raj").unwrap();
        let sha = "a".repeat(64);
        for parts in [
            vec!["roles admin,pm".to_string()],
            vec!["+reviewer -pm".into()],
            vec!["-admin on deactivation".into()],
            vec![
                format!("sha256 {sha}"),
                "commit 1a2b3c4".into(),
                "created 3".into(),
            ],
            vec!["on until 2027-01-31".into()],
            vec!["off".into(), "auditor revoked 2".into()],
            vec!["entra".into()],
            vec!["last_admin".into()],
            vec!["sign_in_name".into()],
            vec!["sign_in_name confirmed".into(), "display_name".into()],
            vec!["3 pms".into()],
        ] {
            let d = join(Some(&raj), &parts).unwrap();
            assert!(is_safe(&d), "{d}");
            assert!(is_safe(&parts.join("; ")), "{parts:?}");
        }
        for bad in [
            "",
            "host_user Raj",
            "raj@contoso.com",
            "roles admin,owner",
            "sha256 xyz",
            "on until tomorrow",
            "U0RAJ",
            "+superuser",
            "entra; 00000000-0000-0000-0000-000000000000",
            "created lots",
            "many pms",
            "ana pms",
        ] {
            assert!(!is_safe(bad), "{bad}");
        }
        assert!(is_purge_detail("purged 12 role changes"));
        assert!(!is_purge_detail("purged twelve role changes"));
        assert_eq!(join(None, &[]), None);
    }
}
