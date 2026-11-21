import { render } from '@testing-library/react';
import type { Api } from '../api';
import { createMockClient } from '../api/mock';
import type { MockOptions } from '../api/mock';
import { App } from '../App';

export function renderApp(opts: MockOptions & { hash?: string; wrap?: (api: Api) => Api } = {}) {
  const { hash, wrap, ...mock } = opts;
  window.location.hash = hash ?? '';
  let api!: Api;
  const view = render(
    <App
      pollMs={5}
      createApi={(onSignedOut) => {
        const base = createMockClient({ latency: 0, stepMs: 0, signedIn: true, ...mock, onSignedOut });
        api = wrap ? wrap(base) : base;
        return api;
      }}
    />,
  );
  return { ...view, api };
}
