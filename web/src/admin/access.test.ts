import { describe, expect, it } from 'vitest';
import { MOCK_PERMISSIONS } from '../api/mockAdmin';
import { canAdminister, canEditContent, canEditPolicy, canExport, canSee, sectionsFor } from './access';

const as = (role: keyof typeof MOCK_PERMISSIONS) => ({ permissions: MOCK_PERMISSIONS[role] });

describe('what each role may open', () => {
  it('gives an admin every part, the export, and both kinds of proposal', () => {
    expect(sectionsFor(as('admin'))).toEqual(['overview', 'audit', 'team', 'bots', 'connectors', 'policy', 'content']);
    expect(canAdminister(as('admin'))).toBe(true);
    expect(canExport(as('admin'))).toBe(true);
    expect(canEditPolicy(as('admin'))).toBe(true);
    expect(canEditContent(as('admin'))).toBe(true);
  });

  it('gives an auditor everything but the connectors, and lets them read policy and content but not propose', () => {
    expect(sectionsFor(as('auditor'))).toEqual(['overview', 'audit', 'team', 'bots', 'policy', 'content']);
    expect(canSee(as('auditor'), 'connectors')).toBe(false);
    expect(canAdminister(as('auditor'))).toBe(true);
    expect(canExport(as('auditor'))).toBe(true);
    expect(canEditPolicy(as('auditor'))).toBe(false);
    expect(canEditContent(as('auditor'))).toBe(false);
  });

  it('gives a PM nothing, and an older server (no permissions) nothing either', () => {
    expect(sectionsFor(as('pm'))).toEqual([]);
    expect(canAdminister(as('pm'))).toBe(false);
    expect(canEditPolicy(as('pm'))).toBe(false);
    expect(sectionsFor({})).toEqual([]);
    expect(canAdminister({})).toBe(false);
  });

  it('opens policy and content on their own permissions, each alone', () => {
    expect(sectionsFor({ permissions: ['view-policies'] })).toEqual(['policy']);
    expect(sectionsFor({ permissions: ['view-content'] })).toEqual(['content']);
    expect(canAdminister({ permissions: ['view-content'] })).toBe(true);
    // Editing without viewing does not open the page.
    expect(canSee({ permissions: ['edit-policies'] }, 'policy')).toBe(false);
  });

  it('does not guess from words in a permission name', () => {
    expect(canAdminister({ permissions: ['audit:read', 'admin'] })).toBe(false);
  });
});
