import { loadEnv } from 'vite';
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig(({ mode, command }) => {
  // The sample-data client must never ship. Refuse to build for production with it switched on.
  const env = loadEnv(mode, process.cwd(), 'VITE_');
  if (command === 'build' && mode === 'production' && env.VITE_API === 'mock') {
    throw new Error(
      'VITE_API=mock is for development only. A production build must use the real client: unset VITE_API.',
    );
  }
  return {
    plugins: [react()],
    build: {
      // Fonts are always separate files on this origin: the content security policy blocks data: fonts.
      assetsInlineLimit: (file: string) => (/\.(woff2?|ttf|otf)$/.test(file) ? false : undefined),
    },
    server: {
      // With VITE_API=real the app talks to pmos-web; override with PMOS_WEB_URL.
      proxy: { '/api': process.env.PMOS_WEB_URL ?? 'http://127.0.0.1:8787' },
    },
    test: {
      environment: 'jsdom',
      globals: true,
      setupFiles: ['./src/test/setup.ts'],
      include: ['src/**/*.test.{ts,tsx}'],
      css: false,
      // Room for a cold lazy-page import on a slow runner (see src/test/setup.ts).
      testTimeout: 15000,
    },
  };
});
