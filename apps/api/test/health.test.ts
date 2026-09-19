import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { buildApp } from "../src/app.js";

describe("api", () => {
  let app: FastifyInstance;

  beforeAll(async () => {
    app = await buildApp({
      NODE_ENV: "test",
      API_PORT: 0,
      API_HOST: "127.0.0.1",
      WEB_ORIGIN: "http://localhost:3000",
      LOG_LEVEL: "error",
    });
  });

  afterAll(async () => {
    await app.close();
  });

  it("GET /health returns ok", async () => {
    const res = await app.inject({ method: "GET", url: "/health" });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ status: "ok" });
  });

  it("unbuilt routes answer 501, not fake data", async () => {
    const res = await app.inject({ method: "GET", url: "/tilesets/abc/manifest" });
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
});
