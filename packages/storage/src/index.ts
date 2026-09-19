// Tile storage behind one interface: a local directory in dev (fs), any S3-compatible bucket in
// prod (s3). The worker writes tile sets; the API streams them back. Tile sets are immutable, so
// every object is written once under its tile set's prefix.

import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import type { TileStorage } from "./core.js";
import { FsStorage } from "./fs.js";
import { S3Storage } from "./s3.js";

export * from "./core.js";
export { FsStorage } from "./fs.js";
export { S3Storage } from "./s3.js";

/** The monorepo root (where pnpm-workspace.yaml lives), so relative dirs agree across apps. */
export function repoRoot(start: string = process.cwd()): string {
  let dir = resolve(start);
  for (;;) {
    if (existsSync(join(dir, "pnpm-workspace.yaml"))) return dir;
    const parent = dirname(dir);
    if (parent === dir) return resolve(start);
    dir = parent;
  }
}

export interface StorageEnv {
  TILE_STORAGE_DRIVER?: string | undefined;
  TILE_STORAGE_DIR?: string | undefined;
  S3_ENDPOINT?: string | undefined;
  S3_REGION?: string | undefined;
  S3_BUCKET?: string | undefined;
  S3_ACCESS_KEY_ID?: string | undefined;
  S3_SECRET_ACCESS_KEY?: string | undefined;
}

/** Build the configured driver. `fs` resolves TILE_STORAGE_DIR against the repo root. */
export function createStorage(env: StorageEnv): TileStorage {
  const driver = env.TILE_STORAGE_DRIVER || "fs";
  if (driver === "fs") {
    return new FsStorage(resolve(repoRoot(), env.TILE_STORAGE_DIR || ".data/tiles"));
  }
  if (driver === "s3") {
    const missing = ["S3_REGION", "S3_BUCKET", "S3_ACCESS_KEY_ID", "S3_SECRET_ACCESS_KEY"].filter(
      (k) => !env[k as keyof StorageEnv],
    );
    if (missing.length > 0) {
      throw new Error(`TILE_STORAGE_DRIVER=s3 needs ${missing.join(", ")}`);
    }
    return S3Storage.fromConfig({
      endpoint: env.S3_ENDPOINT || undefined,
      region: env.S3_REGION!,
      bucket: env.S3_BUCKET!,
      accessKeyId: env.S3_ACCESS_KEY_ID!,
      secretAccessKey: env.S3_SECRET_ACCESS_KEY!,
    });
  }
  throw new Error(`unknown TILE_STORAGE_DRIVER "${driver}" (use fs or s3)`);
}
