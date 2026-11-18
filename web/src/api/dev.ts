// Dev server only. This file is reached through `if (import.meta.env.DEV)` in ./index.ts, so a
// production build never includes it or the sample-data client.
import { createMockClient } from './mock';
import type { MockPolicyOptions } from './mockPolicy';
import { createRealClient } from './real';
import type { Api, AuthProvider } from './types';

/** `?mock_providers=entra,dev` on the dev server picks the sign-in methods the sample data lists. */
function mockProviders(): AuthProvider[] | undefined {
  const raw = new URLSearchParams(window.location.search).get('mock_providers');
  if (raw === null) return undefined;
  return raw.split(',').filter((p): p is AuthProvider => p === 'entra' || p === 'dev');
}

/** `?mock_brief=never` or `stale` on the dev server shows the home page of a PM whose brief has not run. */
function mockBrief(): 'today' | 'stale' | 'never' | undefined {
  const raw = new URLSearchParams(window.location.search).get('mock_brief');
  return raw === 'never' || raw === 'stale' || raw === 'today' ? raw : undefined;
}

/**
 * Policy and content admin on the dev server: `?mock_proposal=<error code>` refuses every proposal
 * with that code, `?mock_protection=unverified|no_reviews|not_linked` sets what branch protection
 * reads as, and `?mock_proposals=off` turns proposals off.
 */
function mockPolicyOptions(): MockPolicyOptions {
  const q = new URLSearchParams(window.location.search);
  const protection = q.get('mock_protection');
  return {
    proposalError: q.get('mock_proposal') ?? undefined,
    protection: protection === 'unverified' || protection === 'no_reviews' || protection === 'not_linked' ? protection : undefined,
    proposalsOff: q.get('mock_proposals') === 'off',
  };
}

/**
 * The Team screen on the dev server: `?mock_users=<error code>` refuses every write with that code,
 * `?mock_pilot=lapsed` puts the pilot exception in the past, `?mock_deactivations=capped` starts
 * with the hourly deactivation cap reached.
 */
function mockUsersOptions(): { usersError?: string; pilotLapsed: boolean; deactivationCapReached: boolean } {
  const q = new URLSearchParams(window.location.search);
  return {
    usersError: q.get('mock_users') ?? undefined,
    pilotLapsed: q.get('mock_pilot') === 'lapsed',
    deactivationCapReached: q.get('mock_deactivations') === 'capped',
  };
}

/** VITE_API=real talks to pmos-web through the dev proxy; anything else uses the sample data. */
export function devApiFactory(): (onSignedOut: () => void) => Api {
  if (import.meta.env.VITE_API === 'real') return (onSignedOut) => createRealClient({ onSignedOut });
  return (onSignedOut) =>
    createMockClient({
      onSignedOut,
      providers: mockProviders(),
      briefDelivery: mockBrief(),
      audienceDrafts: new URLSearchParams(window.location.search).get('mock_slack') === 'audience',
      listCap: new URLSearchParams(window.location.search).get('mock_drafts') === 'truncated' ? 3 : undefined,
      briefBuilt: import.meta.env.VITE_MOCK_BRIEF !== 'off',
      hostile: import.meta.env.VITE_MOCK_HOSTILE === 'on',
      toolServerDown: new URLSearchParams(window.location.search).get('mock_admin') === 'degraded',
      // `?mock_reviews=<error code>` answers every recommendation with that code (`unknown_outcome` records it first).
      reviewError: new URLSearchParams(window.location.search).get('mock_reviews') ?? undefined,
      ...mockUsersOptions(),
      ...mockPolicyOptions(),
      ...(import.meta.env.VITE_MOCK_EMPTY === 'on' ? { noDrafts: true, firstVisit: false, emptyBrief: true } : {}),
      ...(import.meta.env.VITE_MOCK_FAST === 'on' ? { latency: 0, stepMs: 150 } : {}),
    });
}
