import type { FastifyInstance } from "fastify";

export async function healthRoutes(app: FastifyInstance): Promise<void> {
  // Liveness only. No database call, so Railway's healthcheck stays cheap.
  app.get("/health", async () => ({ status: "ok" }));
}
