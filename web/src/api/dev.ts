// Dev server only. This file is reached through `if (import.meta.env.DEV)` in ./index.ts, so a
// production build never includes it or the sample-data client.
import { createMockClient } from './mock';
import { createRealClient } from './real';
import type { Api } from './types';

/** VITE_API=real talks to pmos-web through the dev proxy; anything else uses the sample data. */
export function devApiFactory(): (onSignedOut: () => void) => Api {
  if (import.meta.env.VITE_API === 'real') return (onSignedOut) => createRealClient({ onSignedOut });
  return (onSignedOut) =>
    createMockClient({
      onSignedOut,
      briefBuilt: import.meta.env.VITE_MOCK_BRIEF !== 'off',
      hostile: import.meta.env.VITE_MOCK_HOSTILE === 'on',
      ...(import.meta.env.VITE_MOCK_FAST === 'on' ? { latency: 0, stepMs: 150 } : {}),
    });
}
