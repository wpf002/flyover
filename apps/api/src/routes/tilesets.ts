import type { ApiError } from "@flyover/types";
import type { FastifyInstance } from "fastify";

const notImplemented = (what: string): ApiError => ({ error: "not_implemented", message: what });

export async function tileSetRoutes(app: FastifyInstance): Promise<void> {
  // TODO(M5): list TileSet rows for a repo.
  app.get("/repos/:id/tilesets", async (_req, reply) => {
    reply.code(501);
    return notImplemented("tile set listing lands in M5 (docs/SPEC.md)");
  });

  // TODO(M5): stream manifest.json from tile storage for this tile set.
  // Needs: the storage driver (fs | s3) behind one interface.
  app.get("/tilesets/:id/manifest", async (_req, reply) => {
    reply.code(501);
    return notImplemented("manifest serving lands in M5 (docs/SPEC.md)");
  });

  // TODO(M5): stream any file under the tile set's storage prefix.
  // Tile sets are immutable, so respond with Cache-Control: public, max-age=31536000, immutable.
  // Reject any path containing ".." or a leading "/" before touching storage.
  app.get("/tilesets/:id/files/*", async (_req, reply) => {
    reply.code(501);
    return notImplemented("tile serving lands in M5 (docs/SPEC.md)");
  });

  // TODO(M8): accept a CSV or JSON layer upload (path,value) and queue a layer build.
  app.post("/tilesets/:id/layers", async (_req, reply) => {
    reply.code(501);
    return notImplemented("layer upload lands in M8 (docs/SPEC.md)");
  });
}
