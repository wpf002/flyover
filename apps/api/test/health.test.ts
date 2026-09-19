import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { FsStorage } from "@flyover/storage";
import { buildApp } from "../src/app.js";

describe("api", () => {
  let app: FastifyInstance;
  let dir: string;

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "flyover-api-"));
    app = await buildApp(
      {
        NODE_ENV: "test",
        API_PORT: 0,
        API_HOST: "127.0.0.1",
        WEB_ORIGIN: "http://localhost:3000",
        LOG_LEVEL: "error",
        TILE_STORAGE_DRIVER: "fs",
        TILE_STORAGE_DIR: dir,
      },
      { storage: new FsStorage(dir) },
    );
  });

  afterAll(async () => {
    await app.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("GET /health returns ok", async () => {
    const res = await app.inject({ method: "GET", url: "/health" });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ status: "ok" });
  });

  it("unbuilt routes answer 501, not fake data", async () => {
    const res = await app.inject({ method: "POST", url: "/tilesets/abc/layers" });
    expect(res.statusCode).toBe(501);
  });

  it("POST /repos rejects non-https sources before touching the database", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/repos",
      payload: { source: "file:///etc/passwd" },
    });
    expect(res.statusCode).toBe(400);
  });

  // SPEC 6.6: reject traversal before touching the database or storage. These return 400 without
  // a database connection, which proves the check runs first.
  it.each([
    "/tilesets/abc/files/..%2F..%2Fetc%2Fpasswd",
    "/tilesets/abc/files/tiles/..%2F..%2Fsecret",
    "/tilesets/abc/files/%2Fetc%2Fpasswd",
    "/tilesets/abc/files/tiles%00.fly",
    "/tilesets/abc/files/tiles%5C0.fly",
  ])("rejects traversal %s with 400", async (url) => {
    const res = await app.inject({ method: "GET", url });
    expect(res.statusCode).toBe(400);
    expect(res.json().error).toBe("bad_path");
  });
});
