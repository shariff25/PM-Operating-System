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
    expect(await api.listDrafts()).toEqual([{ id: 'a' }]);
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
});
