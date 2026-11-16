import { createMockClient } from './mock';
import { createRealClient } from './real';
import type { Api } from './types';

export * from './types';
export { createMockClient } from './mock';
export type { MockOptions } from './mock';
export { createRealClient } from './real';

/** VITE_API=mock|real. Default is mock in dev and real in a production build. */
export function createApi(onSignedOut: () => void): Api {
  const choice = import.meta.env.VITE_API ?? (import.meta.env.DEV ? 'mock' : 'real');
  if (choice === 'real') return createRealClient({ onSignedOut });
  return createMockClient({
    onSignedOut,
    briefBuilt: import.meta.env.VITE_MOCK_BRIEF !== 'off',
  });
}
