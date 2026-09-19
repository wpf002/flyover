import cors from "@fastify/cors";
import { createStorage, type TileStorage } from "@flyover/storage";
import Fastify, { type FastifyInstance } from "fastify";
import type { Env } from "./env.js";
import { healthRoutes } from "./routes/health.js";
import { jobRoutes } from "./routes/jobs.js";
import { repoRoutes } from "./routes/repos.js";
import { tileSetRoutes } from "./routes/tilesets.js";

/** `storage` is injectable for tests; by default it comes from TILE_STORAGE_* env vars. */
export async function buildApp(
  env: Env,
  deps: { storage?: TileStorage } = {},
): Promise<FastifyInstance> {
  const app = Fastify({
    logger: env.NODE_ENV === "test" ? false : { level: env.LOG_LEVEL },
  });
  const storage = deps.storage ?? createStorage(env);

  await app.register(cors, { origin: env.WEB_ORIGIN });

  await app.register(healthRoutes);
  await app.register(repoRoutes);
  await app.register(jobRoutes);
  await app.register(tileSetRoutes, { storage });

  return app;
}
