import cors from "@fastify/cors";
import Fastify, { type FastifyInstance } from "fastify";
import type { Env } from "./env.js";
import { healthRoutes } from "./routes/health.js";
import { jobRoutes } from "./routes/jobs.js";
import { repoRoutes } from "./routes/repos.js";
import { tileSetRoutes } from "./routes/tilesets.js";

export async function buildApp(env: Env): Promise<FastifyInstance> {
  const app = Fastify({
    logger: env.NODE_ENV === "test" ? false : { level: env.LOG_LEVEL },
  });

  await app.register(cors, { origin: env.WEB_ORIGIN });

  await app.register(healthRoutes);
  await app.register(repoRoutes);
  await app.register(jobRoutes);
  await app.register(tileSetRoutes);

  return app;
}
