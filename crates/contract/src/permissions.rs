//! What each [`Role`] may do: the one permission matrix the console, runtime and tool server
//! check against.
//!
//! A person's permissions are the union of their roles' permissions. [`Role::Unknown`] (a role
//! from a newer `team.yaml`) grants nothing, and an empty role list grants nothing.
//!
//! The matrix says what kind of action a role may take. Rules that depend on who owns the
//! thing are the caller's, because only the caller knows the owner:
//! - **`*Own*` permissions** apply to the viewer's own tasks, approvals, routines, traces and
//!   accounts only. The caller checks ownership.
//! - **No one decides another PM's approval.** There is deliberately no permission for it: a
//!   write runs with the owner's own connector token, so deciding for them is impersonation. A
//!   reviewer may [`Permission::RecommendOnApproval`]; the owner confirms with
//!   [`Permission::DecideOwnApproval`].
//! - **Approving a policy change** ([`Permission::ApprovePolicyChange`]) needs a second
//!   platform owner (POL-2): the caller must refuse an approval by the change's author, even
//!   an admin.
//! - **Auditor** sees audit metadata only: [`Permission::ViewAuditLog`] and
//!   [`Permission::ExportAudit`] never include trace bodies, prompts or Slack text.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::model::Role;

/// One concrete action in PM OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Ask a bot: create a task (team bots, and the viewer's own personal bot).
    AskBot,
    /// See the viewer's own tasks and their results.
    ViewOwnTasks,
    /// See other people's tasks.
    ViewOthersTasks,
    /// Pause, resume or delete the viewer's own routines.
    ManageOwnRoutines,
    /// Approve, edit or reject the viewer's own pending approvals.
    DecideOwnApproval,
    /// See other PMs' pending and past approvals.
    ViewOthersApprovals,
    /// Recommend a decision on another PM's approval. The owner still decides.
    RecommendOnApproval,
    /// See the trace (prompts, model replies, tool results) of the viewer's own tasks.
    ViewOwnTraces,
    /// See the trace of any task.
    ViewTraces,
    /// Search the audit log: metadata only, no trace bodies, prompts or Slack text.
    ViewAuditLog,
    /// Export audit search results (ADM-2, CSV).
    ExportAudit,
    /// Propose changes to bots and skills (as a pull request).
    EditBotsAndSkills,
    /// Propose changes to sandbox and provider policies (as a pull request).
    EditPolicies,
    /// Approve a policy change. The caller refuses the change's own author (POL-2).
    ApprovePolicyChange,
    /// Link or unlink the viewer's own Jira, Confluence, Slack and GitHub accounts.
    LinkOwnAccounts,
    /// Manage connectors for everyone: team bots' accounts, revoking another PM's link.
    ManageConnectors,
    /// Approve, remove and change the roles of members.
    ManageMembers,
    /// Rebuild the context index.
    TriggerReindex,
}

impl Permission {
    /// Every permission, in declaration order.
    pub const ALL: [Permission; 18] = [
        Permission::AskBot,
        Permission::ViewOwnTasks,
        Permission::ViewOthersTasks,
        Permission::ManageOwnRoutines,
        Permission::DecideOwnApproval,
        Permission::ViewOthersApprovals,
        Permission::RecommendOnApproval,
        Permission::ViewOwnTraces,
        Permission::ViewTraces,
        Permission::ViewAuditLog,
        Permission::ExportAudit,
        Permission::EditBotsAndSkills,
        Permission::EditPolicies,
        Permission::ApprovePolicyChange,
        Permission::LinkOwnAccounts,
        Permission::ManageConnectors,
        Permission::ManageMembers,
        Permission::TriggerReindex,
    ];

    /// The kebab-case name, as serialized.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::AskBot => "ask-bot",
            Permission::ViewOwnTasks => "view-own-tasks",
            Permission::ViewOthersTasks => "view-others-tasks",
            Permission::ManageOwnRoutines => "manage-own-routines",
            Permission::DecideOwnApproval => "decide-own-approval",
            Permission::ViewOthersApprovals => "view-others-approvals",
            Permission::RecommendOnApproval => "recommend-on-approval",
            Permission::ViewOwnTraces => "view-own-traces",
            Permission::ViewTraces => "view-traces",
            Permission::ViewAuditLog => "view-audit-log",
            Permission::ExportAudit => "export-audit",
            Permission::EditBotsAndSkills => "edit-bots-and-skills",
            Permission::EditPolicies => "edit-policies",
            Permission::ApprovePolicyChange => "approve-policy-change",
            Permission::LinkOwnAccounts => "link-own-accounts",
            Permission::ManageConnectors => "manage-connectors",
            Permission::ManageMembers => "manage-members",
            Permission::TriggerReindex => "trigger-reindex",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a Product Manager may do: their own work only.
const PM: &[Permission] = &[
    Permission::AskBot,
    Permission::ViewOwnTasks,
    Permission::ManageOwnRoutines,
    Permission::DecideOwnApproval,
    Permission::ViewOwnTraces,
    Permission::LinkOwnAccounts,
];

/// What one role grants on its own.
pub fn role_allows(role: &Role, permission: Permission) -> bool {
    use Permission as P;
    match role {
        // Everything. The caller still refuses an admin approving their own policy change.
        Role::Admin => true,
        Role::Pm => PM.contains(&permission),
        // A PM who also sees others' approvals and recommends; never decides them.
        Role::Reviewer => {
            PM.contains(&permission)
                || matches!(permission, P::ViewOthersApprovals | P::RecommendOnApproval)
        }
        // Audit metadata only.
        Role::Auditor => matches!(permission, P::ViewAuditLog | P::ExportAudit),
        // As before: a member who uses PM OS like a PM, and who changes skills and bots by
        // pull request (reviewed under CODEOWNERS).
        Role::SkillAuthor => PM.contains(&permission) || permission == P::EditBotsAndSkills,
        Role::Unknown(_) => false,
    }
}

/// Whether any of `roles` allows `permission`. See the module docs for the rules the caller
/// adds (ownership, a policy change's author, no deciding for others).
pub fn allows(roles: &[Role], permission: Permission) -> bool {
    roles.iter().any(|r| role_allows(r, permission))
}

/// Every permission `roles` allow, in [`Permission::ALL`] order.
pub fn granted(roles: &[Role]) -> Vec<Permission> {
    Permission::ALL
        .into_iter()
        .filter(|p| allows(roles, *p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission as P;

    /// The expected matrix, written out by hand so a change to `role_allows` has to change it
    /// too.
    fn expected(role: &Role) -> Vec<Permission> {
        match role {
            Role::Admin => P::ALL.to_vec(),
            Role::Pm => vec![
                P::AskBot,
                P::ViewOwnTasks,
                P::ManageOwnRoutines,
                P::DecideOwnApproval,
                P::ViewOwnTraces,
                P::LinkOwnAccounts,
            ],
            Role::Reviewer => vec![
                P::AskBot,
                P::ViewOwnTasks,
                P::ManageOwnRoutines,
                P::DecideOwnApproval,
                P::ViewOthersApprovals,
                P::RecommendOnApproval,
                P::ViewOwnTraces,
                P::LinkOwnAccounts,
            ],
            Role::Auditor => vec![P::ViewAuditLog, P::ExportAudit],
            Role::SkillAuthor => vec![
                P::AskBot,
                P::ViewOwnTasks,
                P::ManageOwnRoutines,
                P::DecideOwnApproval,
                P::ViewOwnTraces,
                P::EditBotsAndSkills,
                P::LinkOwnAccounts,
            ],
            Role::Unknown(_) => vec![],
        }
    }

    fn all_roles() -> Vec<Role> {
        let mut roles: Vec<Role> = Role::KNOWN
            .iter()
            .map(|n| Role::parse(n).unwrap())
            .collect();
        roles.push(Role::Unknown("policy-steward".into()));
        roles
    }

    #[test]
    fn every_role_against_every_permission() {
        for role in all_roles() {
            let want = expected(&role);
            for p in P::ALL {
                assert_eq!(role_allows(&role, p), want.contains(&p), "{role} / {p}");
                assert_eq!(allows(std::slice::from_ref(&role), p), want.contains(&p));
            }
            assert_eq!(granted(std::slice::from_ref(&role)), want, "{role}");
        }
    }

    #[test]
    fn all_lists_every_permission_once() {
        let mut names: Vec<&str> = P::ALL.iter().map(|p| p.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), P::ALL.len());
        for p in P::ALL {
            assert_eq!(serde_norway::to_string(&p).unwrap().trim(), p.as_str());
        }
    }

    #[test]
    fn no_one_but_an_admin_manages_the_platform() {
        let admin_only = [
            P::ViewOthersTasks,
            P::ViewTraces,
            P::EditPolicies,
            P::ApprovePolicyChange,
            P::ManageConnectors,
            P::ManageMembers,
            P::TriggerReindex,
        ];
        for role in all_roles().into_iter().filter(|r| *r != Role::Admin) {
            for p in admin_only {
                assert!(!role_allows(&role, p), "{role} / {p}");
            }
        }
    }

    #[test]
    fn auditor_reads_audit_metadata_and_nothing_else() {
        let auditor = [Role::Auditor];
        assert_eq!(granted(&auditor), vec![P::ViewAuditLog, P::ExportAudit]);
        assert!(!allows(&auditor, P::ViewOwnTraces));
        assert!(!allows(&auditor, P::ViewTraces));
        assert!(!allows(&auditor, P::AskBot));
    }

    #[test]
    fn reviewer_recommends_but_decides_only_their_own() {
        let reviewer = [Role::Reviewer];
        assert!(allows(&reviewer, P::RecommendOnApproval));
        assert!(allows(&reviewer, P::ViewOthersApprovals));
        assert!(allows(&reviewer, P::DecideOwnApproval));
        assert!(!allows(&reviewer, P::ViewOthersTasks));
        assert!(!allows(&[Role::Pm], P::RecommendOnApproval));
    }

    #[test]
    fn roles_combine_as_a_union() {
        let roles = [Role::Pm, Role::Auditor];
        assert!(allows(&roles, P::AskBot));
        assert!(allows(&roles, P::ExportAudit));
        assert!(!allows(&roles, P::ManageMembers));
        // An unknown role neither adds nor removes anything.
        let with_unknown = [Role::Pm, Role::Unknown("x".into())];
        assert_eq!(granted(&with_unknown), granted(&[Role::Pm]));
        assert!(granted(&[]).is_empty());
        assert!(granted(&[Role::Unknown("x".into())]).is_empty());
    }
}
