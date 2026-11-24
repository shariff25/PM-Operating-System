import { existsSync } from 'node:fs';
import { defineConfig } from '@playwright/test';

// The test starts its own dev server with the sample data, on its own port.
const PORT = 5191;
// A second server with a brand-new account: no drafts, no requests, an empty brief.
const EMPTY_PORT = 5192;
// A third with the brief not built (501), so the home page lays out without it.
const BRIEF_OFF_PORT = 5193;
// Slice H10: the admin door, reached at a deployment-unique `*.localhost` name (Chromium resolves
// it to loopback), and the public door with the admin door's address for an Admin or Auditor.
export const ADMIN_DOOR_PORT = 5194;
export const PUBLIC_DOOR_PORT = 5195;

// Use a browser that is already installed when there is one (PW_CHROMIUM, or the shared path);
// otherwise Playwright uses the browser it downloaded itself.
const shared = '/opt/pw-browsers/chromium';
const executablePath = process.env.PW_CHROMIUM ?? (existsSync(shared) ? shared : undefined);

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  workers: 2,
  retries: 0,
  reporter: [['list'], ['html', { open: 'never' }]],
  timeout: 30_000,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    launchOptions: executablePath ? { executablePath } : {},
    deviceScaleFactor: 1,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
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
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${BRIEF_OFF_PORT} --strictPort`,
      url: `http://127.0.0.1:${BRIEF_OFF_PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { VITE_API: 'mock', VITE_MOCK_FAST: 'on', VITE_MOCK_BRIEF: 'off' },
    },
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${ADMIN_DOOR_PORT} --strictPort`,
      url: `http://127.0.0.1:${ADMIN_DOOR_PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { VITE_API: 'mock', VITE_MOCK_FAST: 'on', VITE_MOCK_DOOR: 'admin' },
    },
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${PUBLIC_DOOR_PORT} --strictPort`,
      url: `http://127.0.0.1:${PUBLIC_DOOR_PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { VITE_API: 'mock', VITE_MOCK_FAST: 'on', VITE_MOCK_DOOR: 'public' },
    },
  ],
});
