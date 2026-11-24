import { createRealClient } from './real';
import type { Api } from './types';

export * from './types';
export type { MockOptions } from './mock';
export { createRealClient } from './real';

/**
 * Pick the client. The sample-data client is imported only in dev mode: `import.meta.env.DEV` is
 * replaced by `false` in a production build, so the import below is dead code and the bundle has
 * no mock. vite.config.ts also refuses to build in production with VITE_API=mock.
 */
export async function resolveApiFactory(): Promise<(onSignedOut: () => void) => Api> {
  if (import.meta.env.DEV) {
    const { devApiFactory } = await import('./dev');
    return devApiFactory();
  }
  return (onSignedOut) => createRealClient({ onSignedOut });
}
