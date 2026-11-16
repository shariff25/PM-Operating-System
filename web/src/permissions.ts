// Mirrors crates/contract/src/permissions.rs (`Permission`, `role_allows`). Keep the two in
// step: the Rust file is the source of truth. The UI uses this only to decide what to show;
// the backend enforces every action.

export const ROLES = ["pm", "skill-author", "admin", "reviewer", "auditor"] as const;
export type Role = (typeof ROLES)[number];

export const PERMISSIONS = [
  "ask-bot",
  "view-own-tasks",
  "view-others-tasks",
  "manage-own-routines",
  "decide-own-approval",
  "view-others-approvals",
  "recommend-on-approval",
  "view-own-traces",
  "view-traces",
  "view-audit-log",
  "export-audit",
  "edit-bots-and-skills",
  "edit-policies",
  "approve-policy-change",
  "link-own-accounts",
  "manage-connectors",
  "manage-members",
  "trigger-reindex",
] as const;
export type Permission = (typeof PERMISSIONS)[number];

const PM: readonly Permission[] = [
  "ask-bot",
  "view-own-tasks",
  "manage-own-routines",
  "decide-own-approval",
  "view-own-traces",
  "link-own-accounts",
];

export function roleAllows(role: Role | string, permission: Permission): boolean {
  switch (role) {
    case "admin":
      return true;
    case "pm":
      return PM.includes(permission);
    case "reviewer":
      return (
        PM.includes(permission) ||
        permission === "view-others-approvals" ||
        permission === "recommend-on-approval"
      );
    case "auditor":
      return permission === "view-audit-log" || permission === "export-audit";
    case "skill-author":
      return PM.includes(permission) || permission === "edit-bots-and-skills";
    default:
      return false; // unknown role from a newer team.yaml grants nothing
  }
}

/** A person's permissions are the union of their roles' permissions. */
export function can(roles: Role | readonly (Role | string)[], permission: Permission): boolean {
  const list = typeof roles === "string" ? [roles] : roles;
  return list.some((r) => roleAllows(r, permission));
}

export const ROLE_LABELS: Record<Role, string> = {
  pm: "Product Manager",
  "skill-author": "Skill author",
  admin: "Admin",
  reviewer: "Reviewer",
  auditor: "Auditor",
};

export function roleLabel(role: string): string {
  return (ROLE_LABELS as Record<string, string>)[role] ?? role;
}
