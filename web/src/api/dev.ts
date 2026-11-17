// Dev server only. This file is reached through `if (import.meta.env.DEV)` in ./index.ts, so a
// production build never includes it or the sample-data client.
import { createMockClient } from './mock';
import { createRealClient } from './real';
import type { Api, AuthProvider } from './types';

/** `?mock_providers=entra,dev` on the dev server picks the sign-in methods the sample data lists. */
function mockProviders(): AuthProvider[] | undefined {
  const raw = new URLSearchParams(window.location.search).get('mock_providers');
  if (raw === null) return undefined;
  return raw.split(',').filter((p): p is AuthProvider => p === 'entra' || p === 'dev');
}

/** VITE_API=real talks to pmos-web through the dev proxy; anything else uses the sample data. */
export function devApiFactory(): (onSignedOut: () => void) => Api {
  if (import.meta.env.VITE_API === 'real') return (onSignedOut) => createRealClient({ onSignedOut });
  return (onSignedOut) =>
    createMockClient({
      onSignedOut,
      providers: mockProviders(),
      briefBuilt: import.meta.env.VITE_MOCK_BRIEF !== 'off',
      hostile: import.meta.env.VITE_MOCK_HOSTILE === 'on',
      toolServerDown: new URLSearchParams(window.location.search).get('mock_admin') === 'degraded',
      ...(import.meta.env.VITE_MOCK_EMPTY === 'on' ? { noDrafts: true, firstVisit: false, emptyBrief: true } : {}),
      ...(import.meta.env.VITE_MOCK_FAST === 'on' ? { latency: 0, stepMs: 150 } : {}),
    });
}
