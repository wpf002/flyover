import type { PrismaClient } from "@flyover/db";

/**
 * Claim the oldest queued job, or null. Postgres is the queue: FOR UPDATE SKIP LOCKED lets any
 * number of workers poll without claiming the same row.
 */
export async function claimJob(
  prisma: PrismaClient,
): Promise<{ id: string; repoId: string } | null> {
  const rows = await prisma.$queryRaw<{ id: string; repoId: string }[]>`
    UPDATE "IndexJob"
       SET "status" = 'RUNNING'::"JobStatus", "startedAt" = now()
     WHERE "id" = (
       SELECT "id" FROM "IndexJob"
        WHERE "status" = 'QUEUED'::"JobStatus"
        ORDER BY "createdAt" ASC
        FOR UPDATE SKIP LOCKED
        LIMIT 1
     )
    RETURNING "id", "repoId"`;
  return rows[0] ?? null;
}

/** Fail jobs left RUNNING past the timeout, e.g. by a worker that crashed mid-job. */
export async function failStaleJobs(prisma: PrismaClient, timeoutMs: number): Promise<number> {
  const cutoff = new Date(Date.now() - timeoutMs);
  const { count } = await prisma.indexJob.updateMany({
    where: { status: "RUNNING", startedAt: { lt: cutoff } },
    data: {
      status: "FAILED",
      error: "the worker stopped before this job finished",
      finishedAt: new Date(),
    },
  });
  return count;
}
