import type { ApiError } from "@flyover/types";
import type { FastifyInstance } from "fastify";

const notImplemented = (what: string): ApiError => ({ error: "not_implemented", message: what });

export async function jobRoutes(app: FastifyInstance): Promise<void> {
  // TODO(M5): insert an IndexJob row with status QUEUED for this repo and return IndexJobDto.
  // Needs: the worker claim loop in apps/worker, or queued jobs sit forever.
  app.post("/repos/:id/jobs", async (_req, reply) => {
    reply.code(501);
    return notImplemented("queueing index jobs lands in M5 (docs/SPEC.md)");
  });

  // TODO(M5): list IndexJob rows for a repo, newest first.
  app.get("/repos/:id/jobs", async (_req, reply) => {
    reply.code(501);
    return notImplemented("listing index jobs lands in M5 (docs/SPEC.md)");
  });

  // TODO(M5): return one IndexJobDto so the web app can poll progress.
  app.get("/jobs/:id", async (_req, reply) => {
    reply.code(501);
    return notImplemented("job status lands in M5 (docs/SPEC.md)");
  });
}
