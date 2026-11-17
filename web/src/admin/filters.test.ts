import { describe, expect, it } from 'vitest';
import { auditHash, countFilters, filtersToSearch, isDay, NO_FILTERS, parseFilters, rangeBackwards, searchOf, toQuery } from './filters';
import { parseRoute } from '../url';

describe('audit filters and the address', () => {
  it('writes filters in a fixed order and leaves empty ones out', () => {
    expect(filtersToSearch(NO_FILTERS)).toBe('');
    expect(filtersToSearch({ ...NO_FILTERS, outcome: 'denied', actor: 'ana', from: '2026-11-10' })).toBe('actor=ana&outcome=denied&from=2026-11-10');
    expect(auditHash(NO_FILTERS)).toBe('/admin/audit');
    expect(auditHash({ ...NO_FILTERS, action: 'jira.create_issue' })).toBe('/admin/audit?action=jira.create_issue');
  });

  it('reads them back, so a bookmark opens the same view', () => {
    const f = { actor: 'ana', action: 'jira.create_issue', outcome: 'approved', from: '2026-11-01', to: '2026-11-16' };
    expect(parseFilters(filtersToSearch(f))).toEqual(f);
    expect(parseFilters('?actor=ana')).toEqual({ ...NO_FILTERS, actor: 'ana' });
  });

  it('drops anything malformed instead of sending it', () => {
    const bad = parseFilters('actor=a%20b&action=x/y&outcome=nope&from=2026-02-31&to=yesterday');
    expect(bad).toEqual(NO_FILTERS);
    expect(parseFilters(`actor=${'a'.repeat(65)}`).actor).toBe('');
    expect(parseFilters(`action=${'a'.repeat(101)}`).action).toBe('');
  });

  it('checks days', () => {
    expect(isDay('2026-11-16')).toBe(true);
    expect(isDay('2026-13-01')).toBe(false);
    expect(isDay('2026-1-1')).toBe(false);
  });

  it('counts filters and spots a backwards range', () => {
    expect(countFilters(NO_FILTERS)).toBe(0);
    expect(countFilters({ ...NO_FILTERS, actor: 'a', to: '2026-11-01' })).toBe(2);
    expect(rangeBackwards({ ...NO_FILTERS, from: '2026-11-10', to: '2026-11-01' })).toBe(true);
    expect(rangeBackwards({ ...NO_FILTERS, from: '2026-11-10', to: '2026-11-10' })).toBe(false);
  });

  it('asks for whole days: from the start of the first, up to the start after the last', () => {
    expect(toQuery({ ...NO_FILTERS, from: '2026-11-10', to: '2026-11-10', outcome: 'ok' })).toEqual({
      outcome: 'ok',
      from: '2026-11-10T00:00:00.000Z',
      to: '2026-11-11T00:00:00.000Z',
    });
    expect(toQuery(NO_FILTERS)).toEqual({});
  });

  it('is part of the route', () => {
    expect(searchOf('/admin/audit?actor=ana')).toBe('actor=ana');
    expect(searchOf('/admin/audit')).toBe('');
    expect(parseRoute('/admin')).toEqual({ name: 'admin', section: 'overview', search: '' });
    expect(parseRoute('/admin/audit?outcome=denied')).toEqual({ name: 'admin', section: 'audit', search: 'outcome=denied' });
    expect(parseRoute('/admin/team')).toEqual({ name: 'admin', section: 'team', search: '' });
    expect(parseRoute('/admin/connectors')).toEqual({ name: 'admin', section: 'connectors', search: '' });
    expect(parseRoute('/admin/nowhere')).toEqual({ name: 'home' });
  });
});
