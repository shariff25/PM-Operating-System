//! `team.yaml` into the user database (spec §3), the way back for a rollback, and the dev seed.
//!
//! **This module is the only code in PM OS that parses that file** (slice U7: no service reads
//! one, `docs/user-database-spec.md` §12, "The proof"). Its model, `TeamFile` and
//! `TeamMember`, is private to the crate; other crates hand the import bytes
//! ([`ImportRequest::parse`]) or a path ([`seed_request`]). The file it reads is one the
//! operator names on the host, or a development stack's seed (never inside the content tree:
//! `config.rs` refuses that); no service reads one to decide anything.
//!
//! The import is keyed by handle and idempotent: a user that exists with the same fields is
//! unchanged; a field that differs is a conflict, reported and never overwritten (the
//! database wins); re-running never removes anyone or revokes a role. It stops before any
//! write on an unknown role, a duplicate key, a `bots.*` or otherwise unusable handle, an
//! `evals` entry with any role but `pm`, or a person holding admin and auditor while no pilot
//! exception is in force. Admins come in only with `--allow-admins` (the host command asks on
//! the terminal) or `--bootstrap` (an empty table only). The file's SHA-256 and the commit
//! the operator names are recorded.
//!
//! The seed (`PMOS_USERS_SEED`) is the same file shape, loaded by the same code into the
//! in-memory store, for development and eval stacks only (`config.rs` refuses it with a
//! database or live connectors).

use std::collections::BTreeSet;
use std::path::Path;

use pmos_contract::model::{valid_slack_id, Role};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::detail;
use super::rules::{self, Builder, Rules};
use super::{Directory, IdentityKind, NewUser, Refusal, UserError, UserResult, UserRole, EVALS};

/// Which admins an import may bring in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// No admins: a person the file lists as `admin` comes in without it (reported).
    Members,
    /// `--allow-admins`: the file's admins, after a terminal confirmation on the host.
    AllowAdmins,
    /// `--bootstrap`: an empty table only; the file's admins without a prompt, everything
    /// in one transaction.
    Bootstrap,
}

impl ImportMode {
    fn admits_admins(self) -> bool {
        matches!(self, Self::AllowAdmins | Self::Bootstrap)
    }
}

/// One person in an imported `team.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct TeamMember {
    pub(crate) handle: String,
    pub(crate) slack_id: String,
    /// One address, imported as both the sign-in name and the Atlassian email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) github: Option<String>,
    /// Read so an old file still parses; never imported (the host sets Entra links).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) entra_oid: Option<String>,
    /// Tolerant of well-formed names this build doesn't know ([`Role::parse_tolerant`]), so
    /// the import can name them and stop; a malformed name doesn't parse.
    #[serde(deserialize_with = "tolerant_roles")]
    pub(crate) roles: Vec<Role>,
}

/// An imported `team.yaml` (or a seed in its shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TeamFile {
    #[serde(default)]
    pub(crate) members: Vec<TeamMember>,
}

impl TeamFile {
    fn from_yaml(text: &str) -> Result<Self, serde_norway::Error> {
        serde_norway::from_str(text)
    }
}

fn tolerant_roles<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Role>, D::Error> {
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|name| {
            Role::parse_tolerant(&name).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "`{name}` is not a role name (lowercase letters, digits and hyphens)"
                ))
            })
        })
        .collect()
}

/// A `team.yaml` with `members`, after `header` (comment lines). One fixed layout, so the
/// result parses back to exactly `members`.
fn render_team_yaml(header: &str, members: &[TeamMember]) -> String {
    let mut out = header.to_string();
    if members.is_empty() {
        out.push_str("members: []\n");
        return out;
    }
    out.push_str("members:\n");
    for m in members {
        out.push_str(&format!("  - handle: {}\n", quote(&m.handle)));
        out.push_str(&format!("    slack-id: {}\n", quote(&m.slack_id)));
        if let Some(email) = &m.email {
            out.push_str(&format!("    email: {}\n", quote(email)));
        }
        if let Some(github) = &m.github {
            out.push_str(&format!("    github: {}\n", quote(github)));
        }
        let roles: Vec<&str> = m.roles.iter().map(|r| r.as_str()).collect();
        out.push_str(&format!("    roles: [{}]\n", roles.join(", ")));
    }
    out
}

/// A double-quoted YAML scalar.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One import of a `team.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub(crate) file: TeamFile,
    /// SHA-256 of the file's bytes, lower-case hex.
    pub sha256: String,
    /// The commit the operator read it at (`--commit`), 7 to 40 hex digits.
    pub commit: Option<String>,
    pub mode: ImportMode,
}

/// SHA-256 of `bytes`, lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A commit id as `--commit` takes it: 7 to 40 hex digits, stored lower-case.
pub fn valid_commit(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl ImportRequest {
    /// Parse `bytes` as `team.yaml`. A file that doesn't parse stops the import.
    pub fn parse(bytes: &[u8], commit: Option<&str>, mode: ImportMode) -> UserResult<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| UserError::refused(Refusal::ImportRefused, "the file isn't UTF-8 text"))?;
        let file = TeamFile::from_yaml(text).map_err(|e| {
            UserError::refused(
                Refusal::ImportRefused,
                format!("the file isn't a team.yaml: {e}"),
            )
        })?;
        let commit = match commit {
            Some(c) if valid_commit(c) => Some(c.to_lowercase()),
            Some(_) => {
                return Err(UserError::refused(
                    Refusal::Invalid,
                    "--commit must be a commit id (7 to 40 hex digits)",
                ))
            }
            None => None,
        };
        Ok(Self {
            file,
            sha256: sha256_hex(bytes),
            commit,
            mode,
        })
    }

    /// The handles the file lists, in its order.
    pub fn handles(&self) -> Vec<String> {
        self.file.members.iter().map(|m| m.handle.clone()).collect()
    }

    /// The people the file lists as admins (the host command lists them before asking).
    pub fn admins(&self) -> Vec<String> {
        self.file
            .members
            .iter()
            .filter(|m| m.roles.contains(&Role::Admin))
            .map(|m| m.handle.clone())
            .collect()
    }
}

/// The whole-file checks: anything here stops the import before any write.
fn problems(
    dir: &Directory,
    req: &ImportRequest,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let (mut handles, mut slack, mut emails, mut githubs) = (
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    for m in &req.file.members {
        let h = &m.handle;
        if !rules::valid_handle(h) {
            problems.push(format!(
                "`{h}` isn't a usable handle (lowercase letters, digits, `.`, `_`, `-`; not bots.*)"
            ));
        }
        if !handles.insert(h.clone()) {
            problems.push(format!("`{h}` is listed twice"));
        }
        if !valid_slack_id(&m.slack_id) {
            problems.push(format!("`{h}`: the Slack id isn't a Slack member id"));
        } else if !slack.insert(m.slack_id.clone()) {
            problems.push(format!("`{h}`: the Slack id is listed for two people"));
        }
        if let Some(e) = &m.email {
            match rules::address(e, "email") {
                Ok(e) => {
                    if !emails.insert(e) {
                        problems.push(format!("`{h}`: the email is listed for two people"));
                    }
                }
                Err(_) => problems.push(format!("`{h}`: the email isn't an email address")),
            }
        }
        if let Some(g) = &m.github {
            if rules::github_login(g).is_err() {
                problems.push(format!("`{h}`: the GitHub login isn't one"));
            } else if !githubs.insert(g.to_lowercase()) {
                problems.push(format!("`{h}`: the GitHub login is listed for two people"));
            }
        }
        for r in &m.roles {
            if let Role::Unknown(name) = r {
                problems.push(format!("`{h}`: unknown role `{name}`"));
            }
        }
        if h == EVALS && m.roles.iter().any(|r| *r != Role::Pm) {
            problems.push("the evals account may hold pm only".to_string());
        }
        if m.roles.contains(&Role::Admin)
            && m.roles.contains(&Role::Auditor)
            && !dir.exception_in_force(now)
        {
            problems.push(format!(
                "`{h}` holds admin and auditor, and no pilot exception is in force"
            ));
        }
    }
    problems
}

/// The roles `m` gets, and notes on what was dropped.
fn mapped_roles(m: &TeamMember, mode: ImportMode) -> (BTreeSet<UserRole>, Vec<String>) {
    let mut roles = BTreeSet::new();
    let mut notes = Vec::new();
    for r in &m.roles {
        match r {
            Role::Pm => {
                roles.insert(UserRole::Pm);
            }
            Role::Reviewer => {
                roles.insert(UserRole::Reviewer);
            }
            Role::Auditor => {
                roles.insert(UserRole::Auditor);
            }
            Role::Admin if mode.admits_admins() => {
                roles.insert(UserRole::Admin);
            }
            Role::Admin => notes.push(
                "admin not imported (import --allow-admins, or grant-admin on the host)".into(),
            ),
            // Owner Q4: dropped; the person keeps pm.
            Role::SkillAuthor => {
                roles.insert(UserRole::Pm);
                notes.push("skill-author dropped; keeps pm".into());
            }
            // Refused by `problems` before this runs.
            Role::Unknown(_) => {}
        }
    }
    (roles, notes)
}

/// Plan an import into `b` (see the module docs).
pub(crate) fn plan_import(
    b: &mut Builder<'_>,
    req: &ImportRequest,
    rules: &Rules,
) -> UserResult<()> {
    let dir = b.dir;
    let now = b.now;
    if !matches!(b.actor.via, super::Via::Import) {
        return Err(UserError::refused(
            Refusal::HostOnly,
            "the import is a host command (pmos-admin users import)",
        ));
    }
    let found = problems(dir, req, now);
    if !found.is_empty() {
        return Err(UserError::refused(
            Refusal::ImportRefused,
            format!(
                "the import stopped before any write:\n  - {}",
                found.join("\n  - ")
            ),
        ));
    }
    if req.mode == ImportMode::Bootstrap && !dir.users.is_empty() {
        return Err(UserError::refused(
            Refusal::NotEmpty,
            format!(
                "import --bootstrap needs an empty users table; it has {} (deactivated users count): use import without --bootstrap",
                dir.users.len()
            ),
        ));
    }
    let (mut created, mut unchanged, mut conflicts) = (0usize, 0usize, 0usize);
    let mut new_admin = false;
    for m in &req.file.members {
        let h = &m.handle;
        let (roles, mut notes) = mapped_roles(m, req.mode);
        if m.entra_oid.is_some() {
            notes.push("entra-oid ignored: the host sets Entra links (set-entra)".into());
        }
        let is_evals = h == EVALS;
        let with_slack = !is_evals || rules.evals_slack_allowed;
        if !with_slack {
            notes.push("Slack id not imported (the evals account has no keys)".into());
        }
        let email = m.email.as_deref().map(|e| e.trim().to_lowercase());
        match dir.get(h) {
            Some(u) => {
                let mut fields = Vec::new();
                if u.sign_in_name != email {
                    fields.push("sign_in_name");
                }
                if u.atlassian_email != email {
                    fields.push("atlassian_email");
                }
                if u.github_login.as_ref().map(|g| g.to_lowercase())
                    != m.github.as_ref().map(|g| g.to_lowercase())
                {
                    fields.push("github_login");
                }
                if with_slack
                    && u.identity(IdentityKind::Slack).map(|i| i.subject.as_str())
                        != Some(m.slack_id.as_str())
                {
                    fields.push("slack");
                }
                let (mut file_roles, mut db_roles) = (roles.clone(), u.role_set());
                if !req.mode.admits_admins() {
                    file_roles.remove(&UserRole::Admin);
                    db_roles.remove(&UserRole::Admin);
                }
                if file_roles != db_roles {
                    fields.push("roles");
                }
                if fields.is_empty() {
                    unchanged += 1;
                    b.lines.push(format!("unchanged  `{h}`"));
                } else {
                    conflicts += 1;
                    b.lines.push(format!(
                        "conflict   `{h}`: {} differ (the database keeps its values)",
                        fields.join(", ")
                    ));
                }
            }
            None => {
                let mut taken = Vec::new();
                for other in dir.users.values() {
                    if email.is_some()
                        && (other.sign_in_name == email || other.atlassian_email == email)
                    {
                        taken.push(format!("the email (`{}`)", other.handle));
                    }
                    if m.github.is_some()
                        && other.github_login.as_ref().map(|g| g.to_lowercase())
                            == m.github.as_ref().map(|g| g.to_lowercase())
                    {
                        taken.push(format!("the GitHub login (`{}`)", other.handle));
                    }
                    if with_slack
                        && other
                            .identity(IdentityKind::Slack)
                            .is_some_and(|i| i.subject == m.slack_id)
                    {
                        taken.push(format!("the Slack id (`{}`)", other.handle));
                    }
                }
                // A handle spelled as an existing one, or one created earlier in this file, is
                // (`ana_k` beside `ana.k`): the model gateway and a bot's name would share it.
                if let Some(other) = b.spelling_clash(h) {
                    conflicts += 1;
                    b.lines.push(format!(
                        "conflict   `{h}`: not created; it is spelled `{}` in a bot's name and by \
                         the model gateway, as `{other}` is",
                        pmos_contract::onboarding::spelled_handle(h)
                    ));
                } else if !taken.is_empty() {
                    conflicts += 1;
                    b.lines.push(format!(
                        "conflict   `{h}`: not created; {} belong to another user",
                        taken.join(", ")
                    ));
                } else {
                    let user = NewUser {
                        handle: h.clone(),
                        display_name: String::new(),
                        sign_in_name: email.clone(),
                        atlassian_email: email.clone(),
                        github_login: m.github.clone(),
                        slack_id: Some(m.slack_id.clone()),
                        roles: roles.clone(),
                    };
                    // Imported sign-in names count as confirmed: a reviewed pull request put
                    // them in the file.
                    b.create(&user, true, with_slack)?;
                    created += 1;
                    new_admin |= roles.contains(&UserRole::Admin);
                    b.lines.push(format!(
                        "create     `{h}` ({}); sign-in name and Atlassian email {}{}",
                        if roles.is_empty() {
                            "no roles".to_string()
                        } else {
                            super::role_list(&roles)
                        },
                        email.as_deref().unwrap_or("(none)"),
                        m.github
                            .as_deref()
                            .map(|g| format!("; GitHub {g}"))
                            .unwrap_or_default()
                    ));
                }
            }
        }
        for n in notes {
            b.lines.push(format!("           note: {n}"));
        }
    }
    let will_have_users = !dir.users.is_empty() || created > 0;
    if will_have_users && !dir.has_active_admin() && !new_admin {
        return Err(UserError::refused(
            Refusal::LastAdmin,
            "the import would leave users but no active admin: run bootstrap-admin first, or import with --allow-admins",
        ));
    }
    let bootstrap = req.mode == ImportMode::Bootstrap;
    b.change(
        "*",
        super::ChangeKind::Import,
        None,
        Some(detail::import_change(
            &req.sha256,
            req.commit.as_deref(),
            bootstrap,
        )),
    );
    let mut parts = vec![
        format!("sha256 {}", req.sha256),
        format!("commit {}", req.commit.as_deref().unwrap_or("none")),
    ];
    if bootstrap {
        parts.push("bootstrap".into());
    }
    parts.push(format!("created {created}"));
    parts.push(format!("unchanged {unchanged}"));
    parts.push(format!("conflicts {conflicts}"));
    b.event("users.import", "*", parts);
    b.lines.push(format!(
        "{created} to create, {unchanged} unchanged, {conflicts} conflicts (team.yaml sha256 {})",
        req.sha256
    ));
    if conflicts > 0 {
        b.lines.push(
            "conflicts are never overwritten: correct the database with set-sign-in-name or set-atlassian-email, or the file"
                .into(),
        );
    }
    // With nothing to create, the run is still recorded: every applied import is evidence.
    Ok(())
}

/// The database as a `team.yaml`, for a rollback (§3): active users with their roles, Slack
/// id and GitHub login; `email` gets the Atlassian email. Returns the file and warnings (a
/// sign-in name the old file can't hold; a user with no Slack id, left out).
pub fn export_team_yaml(dir: &Directory) -> (String, Vec<String>) {
    let mut members = Vec::new();
    let mut warnings = Vec::new();
    for u in dir.users.values().filter(|u| u.active()) {
        let Some(slack) = u.identity(IdentityKind::Slack) else {
            warnings.push(format!(
                "`{}` has no Slack id and is left out (team.yaml needs one)",
                u.handle
            ));
            continue;
        };
        if u.sign_in_name.is_some() && u.sign_in_name != u.atlassian_email {
            warnings.push(format!(
                "`{}`: the sign-in name differs from the Atlassian email; team.yaml keeps the Atlassian email",
                u.handle
            ));
        }
        let roles = u
            .roles
            .keys()
            .map(|r| match r {
                UserRole::Admin => Role::Admin,
                UserRole::Pm => Role::Pm,
                UserRole::Reviewer => Role::Reviewer,
                UserRole::Auditor => Role::Auditor,
            })
            .collect();
        members.push(TeamMember {
            handle: u.handle.clone(),
            slack_id: slack.subject.clone(),
            email: u.atlassian_email.clone(),
            github: u.github_login.clone(),
            entra_oid: None,
            roles,
        });
    }
    let header =
        "# Exported from the PM OS user database (pmos-admin users export) for a rollback.\n";
    (render_team_yaml(header, &members), warnings)
}

/// The dev seed: `path` as a bootstrap import.
pub fn seed_request(path: &Path) -> Result<ImportRequest, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("PMOS_USERS_SEED {}: {e}", path.display()))?;
    ImportRequest::parse(&bytes, None, ImportMode::Bootstrap)
        .map_err(|e| format!("PMOS_USERS_SEED {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member_yaml(roles: &str) -> String {
        format!("members:\n  - handle: ana\n    slack-id: U0ANA\n    roles: {roles}\n")
    }

    #[test]
    fn reads_reviewer_and_auditor() {
        let team = TeamFile::from_yaml(&member_yaml("[pm, reviewer]")).unwrap();
        assert_eq!(team.members[0].roles, vec![Role::Pm, Role::Reviewer]);
        let team = TeamFile::from_yaml(&member_yaml("[auditor]")).unwrap();
        assert_eq!(team.members[0].roles, vec![Role::Auditor]);
    }

    #[test]
    fn an_unknown_role_parses_so_the_import_can_name_it() {
        let team = TeamFile::from_yaml(&member_yaml("[pm, policy-steward]")).unwrap();
        assert_eq!(
            team.members[0].roles,
            vec![Role::Pm, Role::Unknown("policy-steward".into())]
        );
    }

    #[test]
    fn malformed_role_names_and_unknown_fields_are_refused() {
        for bad in [
            "[Admin]",
            "[\"pm, admin\"]",
            "[\"a b\"]",
            "[\"-x\"]",
            "[\"\"]",
        ] {
            assert!(TeamFile::from_yaml(&member_yaml(bad)).is_err(), "{bad}");
        }
        let extra =
            "members:\n  - handle: ana\n    slack-id: U0ANA\n    roles: [pm]\n    rank: 1\n";
        assert!(TeamFile::from_yaml(extra).is_err());
    }

    #[test]
    fn the_export_round_trips_and_keeps_the_header() {
        let members = vec![
            TeamMember {
                handle: "ana".into(),
                slack_id: "U012AB3CD".into(),
                email: Some("ana+x@example.com".into()),
                github: Some("Ana-F".into()),
                entra_oid: None,
                roles: vec![Role::Pm, Role::Reviewer],
            },
            TeamMember {
                handle: "raj".into(),
                slack_id: "W99".into(),
                email: Some("we\"ird\\@x.com".into()),
                github: None,
                entra_oid: None,
                roles: vec![Role::Admin],
            },
        ];
        let text = render_team_yaml("# Exported.\n", &members);
        assert!(text.starts_with("# Exported.\nmembers:\n"));
        assert_eq!(TeamFile::from_yaml(&text).unwrap().members, members);
        let empty = render_team_yaml("", &[]);
        assert_eq!(empty, "members: []\n");
        assert!(TeamFile::from_yaml(&empty).unwrap().members.is_empty());
    }
}
