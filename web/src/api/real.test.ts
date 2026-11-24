import { describe, expect, it, vi } from 'vitest';
import { createRealClient } from './real';
import { ApiError } from './types';

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

const ME = {
  handle: 'ana', name: 'Ana Silva', roles: ['pm'], csrf_token: 'tok-1',
  first_visit: false, hints_seen: [], nudges: true,
};

describe('real client', () => {
  it('sends credentials and the CSRF header from /api/me on every write', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, ME))
      .mockResolvedValueOnce(json(200, { ...ME, nudges: false }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const api = createRealClient({ fetchImpl });
    await api.me();
    await api.setNudges(false);
    await api.deleteDraft('apr_1');

    const [meCall, putCall, delCall] = fetchImpl.mock.calls;
    expect(meCall![0]).toBe('/api/me');
    expect((meCall![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBeUndefined();
    expect(meCall![1]!.credentials).toBe('include');
    expect(putCall![1]!.method).toBe('PUT');
    expect((putCall![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok-1');
    expect(putCall![1]!.body).toBe(JSON.stringify({ nudges: false }));
    expect(delCall![1]!.method).toBe('DELETE');
    expect((delCall![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok-1');
  });

  it('saves Show Nudge as its own setting, and marks guided-start steps as hints', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, ME))
      .mockResolvedValueOnce(json(200, { ...ME, mascot: false }))
      .mockResolvedValueOnce(json(200, { ...ME, hints_seen: ['tour_asked'] }));
    const api = createRealClient({ fetchImpl });
    await api.me();
    expect((await api.setMascot(false)).mascot).toBe(false);
    expect((await api.markHintSeen('tour_asked')).hints_seen).toEqual(['tour_asked']);
    const [, putCall, postCall] = fetchImpl.mock.calls;
    expect(putCall![0]).toBe('/api/me/settings');
    expect(putCall![1]!.method).toBe('PUT');
    expect(putCall![1]!.body).toBe(JSON.stringify({ mascot: false }));
    expect(postCall![0]).toBe('/api/me/hints');
    expect(postCall![1]!.body).toBe(JSON.stringify({ seen: 'tour_asked' }));
  });

  it('asks for the sign-in providers without a session or a CSRF header', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(200, { providers: ['entra', 'dev'] }));
    const api = createRealClient({ fetchImpl });
    expect(await api.providers()).toEqual(['entra', 'dev']);
    expect(fetchImpl.mock.calls[0]![0]).toBe('/api/auth/providers');
    expect(fetchImpl.mock.calls[0]![1]!.method).toBe('GET');
  });

  it('turns an error body into a typed ApiError with the server sentence', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(
      json(409, { code: 'needs_external_confirm', error: 'This channel is shared outside the company.' }),
    );
    const api = createRealClient({ fetchImpl });
    const err = await api.approve('apr_1').catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 409, code: 'needs_external_confirm', message: 'This channel is shared outside the company.' });
  });

  it('sends confirm_external only when asked', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(200, {}));
    const api = createRealClient({ fetchImpl });
    await api.approve('a', { confirmExternal: true });
    await api.approve('a');
    expect(fetchImpl.mock.calls[0]![1]!.body).toBe(JSON.stringify({ confirm_external: true }));
    expect(fetchImpl.mock.calls[1]![1]!.body).toBe('{}');
  });

  it('returns the app to sign-in on 401', async () => {
    const onSignedOut = vi.fn();
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(401, { code: 'signed_out', error: 'Please sign in.' }));
    const api = createRealClient({ fetchImpl, onSignedOut });
    await expect(api.listDrafts()).rejects.toMatchObject({ code: 'signed_out' });
    expect(onSignedOut).toHaveBeenCalledOnce();
  });

  it('gives a plain network error and a plain fallback for a non-JSON failure', async () => {
    const down = createRealClient({ fetchImpl: vi.fn<typeof fetch>().mockRejectedValue(new TypeError('x')) });
    await expect(down.me()).rejects.toMatchObject({ code: 'network' });
    const html = createRealClient({ fetchImpl: vi.fn<typeof fetch>().mockResolvedValue(new Response('<html>', { status: 502 })) });
    await expect(html.me()).rejects.toMatchObject({ status: 502, code: 'unknown' });
  });

  it('unwraps the contract envelopes', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, { drafts: [{ id: 'a' }] }))
      .mockResolvedValueOnce(json(200, { suggestion: null }))
      .mockResolvedValueOnce(json(202, { suggestion: { status: 'working', instruction: 'x', body: null, error: null } }));
    const api = createRealClient({ fetchImpl });
    expect(await api.listDrafts()).toEqual({ drafts: [{ id: 'a' }], truncated: false, total_pending: null });
    expect(await api.getSuggestion('a')).toBeNull();
    expect((await api.requestChange('a', 'x')).status).toBe('working');
  });

  it('sends the version with approve when there is one, and not otherwise', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(200, {}));
    const api = createRealClient({ fetchImpl });
    await api.approve('a', { version: 'v7', confirmExternal: true });
    await api.approve('a', { confirmExternal: true });
    expect(JSON.parse(fetchImpl.mock.calls[0]![1]!.body as string)).toEqual({ confirm_external: true, version: 'v7' });
    expect(JSON.parse(fetchImpl.mock.calls[1]![1]!.body as string)).toEqual({ confirm_external: true });
  });

  it('turns 409 stale into a typed error', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(409, { code: 'stale', error: 'Changed.' }));
    const api = createRealClient({ fetchImpl });
    await expect(api.approve('a', { version: 'v1' })).rejects.toMatchObject({ status: 409, code: 'stale' });
  });

  it('encodes ids placed in paths (review finding 4)', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(200, {}));
    const api = createRealClient({ fetchImpl });
    const evil = '../tasks?x=1#y';
    await api.getDraft(evil);
    await api.approve(evil);
    await api.getTask(evil);
    await api.dismissFollowup(evil);
    for (const call of fetchImpl.mock.calls) {
      const url = call[0] as string;
      expect(url).not.toMatch(/[?#]/);
      expect(url).not.toMatch(/\/\.\.(\/|$)/);
      expect(url).toContain('..%2Ftasks%3Fx%3D1%23y');
    }
    expect(fetchImpl.mock.calls[0]![0]).toBe('/api/drafts/..%2Ftasks%3Fx%3D1%23y');
  });

  it('refuses an id that is only dots, which a browser would treat as a path step', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(404, { code: 'not_found' }));
    const api = createRealClient({ fetchImpl });
    await expect(api.getDraft('..')).rejects.toMatchObject({ code: 'not_found' });
    expect(fetchImpl.mock.calls[0]![0]).toBe('/api/drafts/%00');
  });

  it('sends the audit filters and the cursor, and leaves empty ones out', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(200, { events: [], next_cursor: null }));
    const api = createRealClient({ fetchImpl });
    await api.adminAudit({ actor: 'ana', outcome: '', limit: 50, before: 'c1.abc', from: '2026-11-10T00:00:00.000Z' });
    expect(fetchImpl.mock.calls[0]![0]).toBe('/api/admin/audit?actor=ana&from=2026-11-10T00%3A00%3A00.000Z&before=c1.abc&limit=50');
    await api.adminAudit({});
    expect(fetchImpl.mock.calls[1]![0]).toBe('/api/admin/audit');
  });

  it('reads the export headers, and never sends a page size or cursor with it', async () => {
    const csv = (truncated: string) =>
      new Response('"at"\r\n', {
        status: 200,
        headers: { 'Content-Type': 'text/csv', 'Content-Disposition': 'attachment; filename="pmos-audit.csv"', 'X-Pmos-Truncated': truncated, 'X-Pmos-Row-Count': '10000' },
      });
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValueOnce(csv('true')).mockResolvedValueOnce(csv('false'));
    const api = createRealClient({ fetchImpl });
    const cut = await api.adminAuditExport({ outcome: 'denied', limit: 50, before: 'c1.x' });
    expect(fetchImpl.mock.calls[0]![0]).toBe('/api/admin/audit/export?outcome=denied');
    expect(cut).toMatchObject({ truncated: true, rowCount: 10000, filename: 'pmos-audit.csv' });
    expect(await cut.blob.text()).toBe('"at"\r\n');
    expect((await api.adminAuditExport({})).truncated).toBe(false);
  });

  it('turns an admin 403 into a typed error', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(403, { code: 'forbidden', error: 'No.' }));
    const api = createRealClient({ fetchImpl });
    await expect(api.adminConnectors()).rejects.toMatchObject({ status: 403, code: 'forbidden' });
  });

  it('unwraps the connector list', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(200, { connectors: [{ name: 'jira' }] }));
    expect(await createRealClient({ fetchImpl }).adminConnectors()).toEqual([{ name: 'jira' }]);
  });

  it('sends each Team write to its route, with the CSRF header and the version', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async (input) =>
      String(input) === '/api/me' ? json(200, ME) : json(200, { version: 43, user: {} }),
    );
    const api = createRealClient({ fetchImpl });
    await api.me();
    await api.adminUsers();
    await api.adminInvite({ handle: 'cy', sign_in_name: 'cy@x.example', atlassian_email: 'cy@x.example', roles: ['pm'] });
    await api.adminSetRoles('ana', ['pm', 'reviewer'], 42);
    await api.adminSetFields('ana', { display_name: 'Ana S.' }, 42);
    await api.adminConfirmSignIn('sam', 'sam@x.example', 42);
    await api.adminDeactivate('ben', 42);
    await api.adminReactivate('ben');
    await api.adminUnlink('ben', 'github');
    await api.adminAssignReviewer('priya', ['ravi'], 42);
    await api.adminSetRoles('..', [], 1);
    const calls = fetchImpl.mock.calls.slice(1).map(([url, init]) => [init!.method, url, init!.body ?? null, (init!.headers as Record<string, string>)['X-Pmos-Csrf'] ?? null]);
    expect(calls).toEqual([
      ['GET', '/api/admin/users', null, null],
      ['POST', '/api/admin/users', JSON.stringify({ handle: 'cy', sign_in_name: 'cy@x.example', atlassian_email: 'cy@x.example', roles: ['pm'] }), 'tok-1'],
      ['PUT', '/api/admin/users/ana/roles', JSON.stringify({ roles: ['pm', 'reviewer'], expected_version: 42 }), 'tok-1'],
      ['PUT', '/api/admin/users/ana/fields', JSON.stringify({ display_name: 'Ana S.', expected_version: 42 }), 'tok-1'],
      ['POST', '/api/admin/users/sam/sign-in-name/confirm', JSON.stringify({ sign_in_name: 'sam@x.example', expected_version: 42 }), 'tok-1'],
      ['POST', '/api/admin/users/ben/deactivate', JSON.stringify({ expected_version: 42 }), 'tok-1'],
      ['POST', '/api/admin/users/ben/reactivate', '{}', 'tok-1'],
      ['DELETE', '/api/admin/users/ben/identities/github', null, 'tok-1'],
      ['PUT', '/api/admin/reviewers/priya', JSON.stringify({ pms: ['ravi'], expected_version: 42 }), 'tok-1'],
      // A handle of dots alone is never sent as a path step.
      ['PUT', '/api/admin/users/%00/roles', JSON.stringify({ roles: [], expected_version: 1 }), 'tok-1'],
    ]);
  });

  it('keeps the code and status of a Team refusal', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(409, { code: 'stale', error: 'The team changed.' }));
    const err = await createRealClient({ fetchImpl }).adminSetRoles('ana', ['pm'], 1).catch((e: unknown) => e);
    expect(err).toMatchObject({ status: 409, code: 'stale' });
  });
});
