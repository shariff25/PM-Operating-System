/// <reference types="vitest/config" />
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Dev server proxies the backend paths to PMOS_WEB_BACKEND (default: crates/web on :8080)
// so the `__Host-` session cookie stays same-origin in real mode.
const backend = process.env.PMOS_WEB_BACKEND ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/auth": backend,
      "/v1": backend,
      "/internal": backend,
    },
  },
  build: { sourcemap: true },
  test: {
    environment: "jsdom",
    setupFiles: ["./test/setup.ts"],
    include: ["test/**/*.test.{ts,tsx}"],
    css: false,
  },
});
