// DB-backed: job claiming with SKIP LOCKED, and cleanup on failure (SPEC 6.10). Uses the real
// Postgres from DATABASE_URL and skips when it is unreachable.

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { disconnectPrisma, getPrisma } from "@flyover/db";
import { FsStorage } from "@flyover/storage";

import { claimJob } from "../src/claim.js";
import "../src/env.js";
import { runJob } from "../src/pipeline.js";

async function dbReachable(): Promise<boolean> {
  if (!process.env.DATABASE_URL) return false;
  try {
    await getPrisma().$queryRaw`SELECT 1`;
    return true;
  } catch {
    return false;
  }
}

const reachable = await dbReachable();
const tag = `test-${process.pid}-${Date.now()}`;

describe.skipIf(!reachable)("worker (database)", () => {
  const repoIds: string[] = [];
  let dir: string;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), "flyover-worker-"));
  });

  afterAll(async () => {
    await getPrisma().repo.deleteMany({ where: { id: { in: repoIds } } });
    await disconnectPrisma();
    rmSync(dir, { recursive: true, force: true });
  });

  async function repoWithJob(source: string) {
    const repo = await getPrisma().repo.create({ data: { name: tag, source } });
    repoIds.push(repo.id);
    const job = await getPrisma().indexJob.create({ data: { repoId: repo.id } });
    return { repo, job };
  }

  it("claims each queued job once, even when claims race", async () => {
    const a = await repoWithJob(`https://github.com/${tag}/a.git`);
    const b = await repoWithJob(`https://github.com/${tag}/b.git`);
    const mine = new Set([a.job.id, b.job.id]);

    // Claim until both of ours are taken (other queued rows may exist in a dev database).
    const claimed: string[] = [];
    for (let i = 0; i < 50 && claimed.filter((id) => mine.has(id)).length < 2; i++) {
      const results = await Promise.all([claimJob(getPrisma()), claimJob(getPrisma())]);
      for (const r of results) if (r) claimed.push(r.id);
    }
    const ours = claimed.filter((id) => mine.has(id));
    expect(ours.sort()).toEqual([...mine].sort());
    expect(new Set(claimed).size).toBe(claimed.length);
    const rows = await getPrisma().indexJob.findMany({ where: { id: { in: [...mine] } } });
    expect(rows.every((r) => r.status === "RUNNING" && r.startedAt)).toBe(true);
  });

  it("fails a job on a disallowed host and deletes its scratch directory", async () => {
    const { job } = await repoWithJob(`https://evil.example.com/${tag}/x.git`);
    const dataDir = join(dir, "data");
    await runJob(
      {
        prisma: getPrisma(),
        storage: new FsStorage(join(dir, "tiles")),
        flyoverBin: "/nonexistent/flyover",
        dataDir,
        allowlist: ["github.com"],
        maxCloneBytes: 1e9,
        timeoutMs: 60_000,
      },
      { id: job.id, repoId: job.repoId },
    );
    const row = await getPrisma().indexJob.findUniqueOrThrow({ where: { id: job.id } });
    expect(row.status).toBe("FAILED");
    expect(row.error).toMatch(/GIT_HOST_ALLOWLIST/);
    expect(row.finishedAt).not.toBeNull();
    expect(existsSync(join(dataDir, job.id))).toBe(false);
  });
});
