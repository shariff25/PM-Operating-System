import type { Me } from '../api';

/** The four parts of the Admin area, in the order they are listed. */
export type AdminSection = 'overview' | 'audit' | 'team' | 'connectors';
export const ADMIN_SECTIONS: readonly AdminSection[] = ['overview', 'audit', 'team', 'connectors'];

const has = (me: Pick<Me, 'permissions'>, ...wanted: string[]) => {
  const given = me.permissions ?? [];
  return wanted.some((w) => given.includes(w));
};

/** May this person open the given part of the Admin area? Mirrors the server's matrix (web-api.md). */
export function canSee(me: Pick<Me, 'permissions'>, section: AdminSection): boolean {
  switch (section) {
    case 'overview':
    case 'team':
      return has(me, 'view-audit-log', 'manage-members');
    case 'audit':
      return has(me, 'view-audit-log');
    case 'connectors':
      return has(me, 'manage-connectors');
  }
}

/** Show the Admin entry when the person may see the audit log or the connectors. */
export function canAdminister(me: Pick<Me, 'permissions'>): boolean {
  return has(me, 'view-audit-log', 'manage-connectors');
}

export const canExport = (me: Pick<Me, 'permissions'>) => has(me, 'export-audit');

/** The parts this person may open, in order. */
export function sectionsFor(me: Pick<Me, 'permissions'>): AdminSection[] {
  return ADMIN_SECTIONS.filter((s) => canSee(me, s));
}

export function sectionHref(section: AdminSection): string {
  return section === 'overview' ? '#/admin' : `#/admin/${section}`;
}
