import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Aborting a model call leaves one unawaited rejection inside @langchain/core, carrying our
    // abort reason. The worker process ignores exactly that rejection (src/main.ts, with
    // `isAbortRejection`); the test run does the same. Every other unhandled error still fails.
    onUnhandledError(error) {
      if (error.type === "Unhandled Rejection" && (error.message === "lost" || error.message === "stopped")) {
        return false;
      }
    },
  },
});
