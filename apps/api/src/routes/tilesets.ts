import { getPrisma } from "@flyover/db";
import { IMMUTABLE, safeRelativePath, type TileStorage } from "@flyover/storage";
import type { ApiError, TileSetDto } from "@flyover/types";
import type { FastifyInstance, FastifyReply } from "fastify";

import { toTileSetDto } from "../dto.js";

const notImplemented = (what: string): ApiError => ({ error: "not_implemented", message: what });
const notFound = (what: string): ApiError => ({ error: "not_found", message: `${what} not found` });

export async function tileSetRoutes(
  app: FastifyInstance,
  opts: { storage: TileStorage },
): Promise<void> {
  const { storage } = opts;

  app.get<{ Params: { id: string } }>("/repos/:id/tilesets", async (req): Promise<TileSetDto[]> => {
    const sets = await getPrisma().tileSet.findMany({
      where: { repoId: req.params.id },
      orderBy: { createdAt: "desc" },
    });
    return sets.map(toTileSetDto);
  });

  app.get<{ Params: { id: string } }>("/tilesets/:id", async (req, reply) => {
    const set = await getPrisma().tileSet.findUnique({ where: { id: req.params.id } });
    if (!set) {
      reply.code(404);
      return notFound("tile set");
    }
    return toTileSetDto(set);
  });

  app.get<{ Params: { id: string } }>("/tilesets/:id/manifest", async (req, reply) => {
    return serve(reply, req.params.id, "manifest.json");
  });

  // Any file under the tile set's prefix. SPEC 6.6: the path is validated before the database or
  // storage is touched, and only keys under this tile set's prefix are ever read.
  app.get<{ Params: { id: string; "*": string } }>("/tilesets/:id/files/*", async (req, reply) => {
    const rel = req.params["*"];
    if (safeRelativePath(rel) === null) {
      reply.code(400);
      return { error: "bad_path", message: "path must be relative, without '..', '/', or NUL" };
    }
    return serve(reply, req.params.id, rel);
  });

  // TODO(M8): accept a CSV or JSON layer upload (path,value) and queue a layer build.
  app.post("/tilesets/:id/layers", async (_req, reply) => {
    reply.code(501);
    return notImplemented("layer upload lands in M8 (docs/SPEC.md)");
  });

  async function serve(reply: FastifyReply, tileSetId: string, rel: string) {
    const set = await getPrisma().tileSet.findUnique({ where: { id: tileSetId } });
    if (!set) {
      reply.code(404);
      return notFound("tile set");
    }
    const obj = await storage.get(`${set.storagePrefix}/${rel}`);
    if (!obj) {
      reply.code(404);
      return notFound("file");
    }
    reply.header("cache-control", IMMUTABLE).type(obj.contentType);
    if (obj.size !== undefined) reply.header("content-length", obj.size);
    return reply.send(obj.body);
  }
}
