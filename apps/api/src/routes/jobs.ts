import { getPrisma } from "@flyover/db";
import type { ApiError, IndexJobDto } from "@flyover/types";
import type { FastifyInstance } from "fastify";

import { toJobDto } from "../dto.js";

const notFound = (what: string): ApiError => ({ error: "not_found", message: `${what} not found` });

export async function jobRoutes(app: FastifyInstance): Promise<void> {
  // Queue an index job. If one is already queued or running for this repo, return it instead of
  // stacking duplicates. The worker (apps/worker) claims QUEUED rows with SKIP LOCKED.
  app.post<{ Params: { id: string } }>("/repos/:id/jobs", async (req, reply) => {
    const prisma = getPrisma();
    const repo = await prisma.repo.findUnique({ where: { id: req.params.id } });
    if (!repo) {
      reply.code(404);
      return notFound("repo");
    }
    const active = await prisma.indexJob.findFirst({
      where: { repoId: repo.id, status: { in: ["QUEUED", "RUNNING"] } },
      orderBy: { createdAt: "desc" },
    });
    if (active) {
      reply.code(200);
      return toJobDto(active);
    }
    const job = await prisma.indexJob.create({ data: { repoId: repo.id } });
    reply.code(201);
    return toJobDto(job);
  });

  app.get<{ Params: { id: string } }>("/repos/:id/jobs", async (req): Promise<IndexJobDto[]> => {
    const jobs = await getPrisma().indexJob.findMany({
      where: { repoId: req.params.id },
      orderBy: { createdAt: "desc" },
      take: 20,
    });
    return jobs.map(toJobDto);
  });

  app.get<{ Params: { id: string } }>("/jobs/:id", async (req, reply) => {
    const job = await getPrisma().indexJob.findUnique({ where: { id: req.params.id } });
    if (!job) {
      reply.code(404);
      return notFound("job");
    }
    return toJobDto(job);
  });
}
