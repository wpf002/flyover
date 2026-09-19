// The job loop: claim a queued job, run the pipeline, repeat. Postgres is the queue.
// SIGINT/SIGTERM stop claiming and abort the running job, which is then marked FAILED and cleaned
// up like any other failure.

import { existsSync } from "node:fs";
import { resolve } from "node:path";

import { disconnectPrisma, getPrisma } from "@flyover/db";
import { createStorage, repoRoot } from "@flyover/storage";

import { claimJob, failStaleJobs } from "./claim.js";
import { allowlist, loadEnv } from "./env.js";
import { runJob } from "./pipeline.js";

const IDLE_MS = 2000;

function log(msg: string, extra: Record<string, unknown> = {}) {
  console.log(JSON.stringify({ time: new Date().toISOString(), msg, ...extra }));
}

async function main() {
  const env = loadEnv();
  const root = repoRoot();
  const flyoverBin = resolve(root, env.FLYOVER_BIN);
  if (!existsSync(flyoverBin)) {
    throw new Error(`FLYOVER_BIN not found at ${flyoverBin}. Build it with \`pnpm rust:build\`.`);
  }
  const prisma = getPrisma();
  const storage = createStorage(env);
  const timeoutMs = env.JOB_TIMEOUT_MINUTES * 60_000;
  const controller = new AbortController();
  let stopping = false;

  const stop = (signal: string) => {
    if (stopping) return;
    stopping = true;
    log("stopping", { signal });
    controller.abort();
  };
  process.on("SIGINT", () => stop("SIGINT"));
  process.on("SIGTERM", () => stop("SIGTERM"));

  const stale = await failStaleJobs(prisma, timeoutMs);
  if (stale > 0) log("failed stale jobs", { count: stale });
  log("worker ready", {
    flyoverBin,
    storage: storage.driver,
    allowlist: allowlist(env),
  });

  while (!stopping) {
    const job = await claimJob(prisma);
    if (!job) {
      await new Promise((r) => setTimeout(r, IDLE_MS));
      continue;
    }
    log("claimed job", job);
    await runJob(
      {
        prisma,
        storage,
        flyoverBin,
        dataDir: resolve(root, env.WORKER_DATA_DIR),
        allowlist: allowlist(env),
        maxCloneBytes: env.JOB_MAX_CLONE_MB * 1e6,
        timeoutMs,
        signal: controller.signal,
        log,
      },
      job,
    );
  }
  await disconnectPrisma();
}

main().catch((err) => {
  log("worker crashed", { error: err instanceof Error ? err.message : String(err) });
  process.exit(1);
});
