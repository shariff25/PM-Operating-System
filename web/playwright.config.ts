import { existsSync } from 'node:fs';
import { defineConfig } from '@playwright/test';

// The test starts its own dev server with the sample data, on its own port.
const PORT = 5191;
// A second server with a brand-new account: no drafts, no requests, an empty brief.
const EMPTY_PORT = 5192;

// Use a browser that is already installed when there is one (PW_CHROMIUM, or the shared path);
// otherwise Playwright uses the browser it downloaded itself.
const shared = '/opt/pw-browsers/chromium';
const executablePath = process.env.PW_CHROMIUM ?? (existsSync(shared) ? shared : undefined);

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  workers: 2,
  retries: 0,
  reporter: 'list',
  timeout: 30_000,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    launchOptions: executablePath ? { executablePath } : {},
    deviceScaleFactor: 1,
  },
  webServer: [
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${PORT} --strictPort`,
      url: `http://127.0.0.1:${PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { VITE_API: 'mock', VITE_MOCK_FAST: 'on', VITE_MOCK_HOSTILE: 'on' },
    },
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${EMPTY_PORT} --strictPort`,
      url: `http://127.0.0.1:${EMPTY_PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { VITE_API: 'mock', VITE_MOCK_FAST: 'on', VITE_MOCK_EMPTY: 'on' },
    },
  ],
});
