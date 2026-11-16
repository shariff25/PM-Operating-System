import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  server: {
    // With VITE_API=real the app talks to pmos-web; override with PMOS_WEB_URL.
    proxy: { '/api': process.env.PMOS_WEB_URL ?? 'http://127.0.0.1:8787' },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    css: false,
  },
});
