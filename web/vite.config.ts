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
    },
  };
});
