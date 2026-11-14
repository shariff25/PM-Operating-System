/**
 * Entry point for the harness worker: `pnpm -C harness build && pnpm -C harness worker`.
 * Reads its configuration from the environment (see harness/README.md) and exits after
 * SIGTERM or SIGINT once the current task is finished or failed.
 */
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createWorker, jsonLogger, type WorkerConfig } from "./worker.js";

export function configFromEnv(env: NodeJS.ProcessEnv = process.env): WorkerConfig {
  const missing: string[] = [];
  const required = (name: string): string => {
    const value = env[name]?.trim();
    if (!value) {
      missing.push(name);
      return "";
    }
    return value;
  };
  const number = (name: string): number | undefined => {
    const value = env[name]?.trim();
    if (!value) {
      return undefined;
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed) || parsed < 0) {
      throw new Error(`${name} must be a non-negative number`);
    }
    return parsed;
  };

  // The content repo sits next to harness/ in the repo; in a sandbox it is mounted.
  const harnessDir = join(dirname(fileURLToPath(import.meta.url)), "..");
  const config: WorkerConfig = {
    botsUrl: required("PMOS_BOTS_URL"),
    workerToken: required("PMOS_WORKER_TOKEN"),
    bot: required("PMOS_WORKER_BOT"),
    gatewayUrl: required("PMOS_GATEWAY_URL"),
    gatewayToken: env.PMOS_GATEWAY_TOKEN?.trim() || undefined,
    toolserverMcpUrl: required("PMOS_TOOLSERVER_MCP_URL"),
    contentRoot: resolve(env.PMOS_CONTENT_ROOT?.trim() || join(harnessDir, "..", "content")),
    workerId: env.PMOS_WORKER_ID?.trim() || undefined,
    waitMs: number("PMOS_WORKER_WAIT_MS"),
    stopGraceMs: number("PMOS_WORKER_STOP_GRACE_MS"),
    maxSteps: number("PMOS_WORKER_MAX_STEPS"),
  };
  if (missing.length > 0) {
    throw new Error(`missing required environment: ${missing.join(", ")}`);
  }
  return config;
}

async function main(): Promise<void> {
  let config: WorkerConfig;
  try {
    config = configFromEnv();
  } catch (error) {
    jsonLogger("error", (error as Error).message);
    process.exitCode = 2;
    return;
  }
  const worker = createWorker(config);
  const stop = (signal: string) => {
    jsonLogger("info", `${signal}: stopping after the current task`);
    void worker.stop();
  };
  process.once("SIGTERM", () => stop("SIGTERM"));
  process.once("SIGINT", () => stop("SIGINT"));
  await worker.run();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
