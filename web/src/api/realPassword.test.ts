import { describe, expect, it, vi } from 'vitest';
import { createRealClient } from './real';

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } });

const ME = { handle: 'ana', name: 'Ana Silva', roles: ['pm'], csrf_token: 'tok-1', first_visit: false, hints_seen: [], nudges: true };

describe('real client: password sign-in (docs/web-api.md, "Password sign-in")', () => {
  it('posts each route with the exact JSON body the contract names', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockImplementation(async () => json(200, {}));
    const api = createRealClient({ fetchImpl });
    await api.passwordSignIn('ana@contoso.example', 'pw-pw-pw-pw-pw');
    await api.passwordSecondStep({ code: '123456' });
    await api.passwordSecondStep({ recovery_code: 'abcd-efgh-ijkl-mnop' });
    await api.passwordSetupBegin('ana@contoso.example', 'ABCDE-FGHIJ-KLMNO-PQRST');
    await api.passwordSetupPassword('a calm river at dawn');
    await api.passwordSetupAuthenticator();
    await api.passwordSetupConfirm('123456');
    const calls = fetchImpl.mock.calls.map(([url, init]) => [url, init!.method, init!.body, (init!.headers as Record<string, string>)['Content-Type']]);
    expect(calls).toEqual([
      ['/api/auth/password/sign-in', 'POST', JSON.stringify({ sign_in_name: 'ana@contoso.example', password: 'pw-pw-pw-pw-pw' }), 'application/json'],
      ['/api/auth/password/second-step', 'POST', JSON.stringify({ code: '123456' }), 'application/json'],
      ['/api/auth/password/second-step', 'POST', JSON.stringify({ recovery_code: 'abcd-efgh-ijkl-mnop' }), 'application/json'],
      ['/api/auth/password/setup/begin', 'POST', JSON.stringify({ sign_in_name: 'ana@contoso.example', setup_code: 'ABCDE-FGHIJ-KLMNO-PQRST' }), 'application/json'],
      ['/api/auth/password/setup/password', 'POST', JSON.stringify({ password: 'a calm river at dawn' }), 'application/json'],
      ['/api/auth/password/setup/authenticator', 'POST', '{}', 'application/json'],
      ['/api/auth/password/setup/confirm', 'POST', JSON.stringify({ code: '123456' }), 'application/json'],
    ]);
    // Nothing secret ever goes in an address.
    for (const [url] of calls) expect(String(url)).not.toMatch(/\?/);
  });

  it('sends the CSRF token on the signed-in routes', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, ME))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(json(200, { recovery_codes: [] }))
      .mockResolvedValueOnce(json(200, { enabled: true, password_set: true, second_step_enrolled: true, recovery_codes_left: 9 }))
      .mockResolvedValueOnce(json(201, { setup_code: 'X', handle: 'ravi', kind: 'setup', expires_at: '2026-11-21T09:00:00Z' }));
    const api = createRealClient({ fetchImpl });
    await api.me();
    await api.passwordChange({ current_password: 'a', code: '123456', new_password: 'b' });
    await api.passwordRecoveryCodes('123456');
    expect((await api.passwordStatus()).recovery_codes_left).toBe(9);
    expect((await api.adminPasswordSetupCode('ravi')).handle).toBe('ravi');
    const [, change, codes, status, setup] = fetchImpl.mock.calls;
    expect(change![0]).toBe('/api/auth/password/change');
    expect(change![1]!.body).toBe(JSON.stringify({ current_password: 'a', code: '123456', new_password: 'b' }));
    expect((change![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok-1');
    expect(codes![0]).toBe('/api/auth/password/recovery-codes');
    expect((codes![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok-1');
    expect(status![0]).toBe('/api/auth/password/status');
    expect(status![1]!.method).toBe('GET');
    expect(setup![0]).toBe('/api/admin/users/ravi/password-setup-code');
    expect(setup![1]!.body).toBe('{}');
    expect((setup![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok-1');
  });

  it.each(['sign_in_failed', 'code_failed', 'sign_in_expired', 'setup_failed', 'setup_expired'])(
    'keeps a 401 %s as the refusal it is, without signing anyone out',
    async (code) => {
      const onSignedOut = vi.fn();
      const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(401, { code, error: 'Server words.', attempts_left: 2 }));
      const err = await createRealClient({ fetchImpl, onSignedOut }).passwordSignIn('a', 'b').catch((e: unknown) => e);
      expect(err).toMatchObject({ status: 401, code });
      expect(onSignedOut).not.toHaveBeenCalled();
    },
  );

  it('still treats a 401 signed_out as signed out', async () => {
    const onSignedOut = vi.fn();
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(401, { code: 'signed_out', error: 'x' }));
    await createRealClient({ fetchImpl, onSignedOut }).passwordStatus().catch(() => {});
    expect(onSignedOut).toHaveBeenCalledTimes(1);
  });

  it('carries attempts_left, the weak reason and Retry-After', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(401, { code: 'code_failed', error: 'x', attempts_left: 1 }))
      .mockResolvedValueOnce(json(422, { code: 'weak_password', error: 'x', reason: 'common' }))
      .mockResolvedValueOnce(json(429, { code: 'too_many_attempts', error: 'x' }, { 'Retry-After': '42' }));
    const api = createRealClient({ fetchImpl });
    expect(await api.passwordSecondStep({ code: '1' }).catch((e: unknown) => e)).toMatchObject({ details: { attempts_left: 1 } });
    expect(await api.passwordSetupPassword('p').catch((e: unknown) => e)).toMatchObject({ details: { reason: 'common' } });
    expect(await api.passwordSignIn('a', 'b').catch((e: unknown) => e)).toMatchObject({ code: 'too_many_attempts', retryAfter: 42 });
  });
});
