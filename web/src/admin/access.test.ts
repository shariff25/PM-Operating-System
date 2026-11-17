import { describe, expect, it } from 'vitest';
import { MOCK_PERMISSIONS } from '../api/mockAdmin';
import { canAdminister, canExport, canSee, sectionsFor } from './access';

const as = (role: keyof typeof MOCK_PERMISSIONS) => ({ permissions: MOCK_PERMISSIONS[role] });

describe('what each role may open', () => {
  it('gives an admin all four parts and the export', () => {
    expect(sectionsFor(as('admin'))).toEqual(['overview', 'audit', 'team', 'connectors']);
    expect(canAdminister(as('admin'))).toBe(true);
    expect(canExport(as('admin'))).toBe(true);
  });

  it('gives an auditor everything but the connectors', () => {
    expect(sectionsFor(as('auditor'))).toEqual(['overview', 'audit', 'team']);
    expect(canSee(as('auditor'), 'connectors')).toBe(false);
    expect(canAdminister(as('auditor'))).toBe(true);
    expect(canExport(as('auditor'))).toBe(true);
  });

  it('gives a PM nothing, and an older server (no permissions) nothing either', () => {
    expect(sectionsFor(as('pm'))).toEqual([]);
    expect(canAdminister(as('pm'))).toBe(false);
    expect(sectionsFor({})).toEqual([]);
    expect(canAdminister({})).toBe(false);
  });

  it('does not guess from words in a permission name', () => {
    expect(canAdminister({ permissions: ['audit:read', 'admin'] })).toBe(false);
  });
});
