import { config } from "dotenv";
import { z } from "zod";

// One .env at the repo root. Real environment variables always win.
config({ path: [".env", "../../.env"], quiet: true });

const optional = z.string().optional();

const schema = z.object({
  DATABASE_URL: z.string().min(1),
  // Scratch space for clones and pipeline output, relative to the repo root.
  WORKER_DATA_DIR: z.string().default(".data/work"),
  // The flyover CLI, relative to the repo root. Build it with `pnpm rust:build`.
  FLYOVER_BIN: z.string().default("target/release/flyover"),
  // Hosts the worker may clone from. Everything else is refused (SPEC 6.3).
  GIT_HOST_ALLOWLIST: z.string().default("github.com,gitlab.com,bitbucket.org"),
  JOB_MAX_CLONE_MB: z.coerce.number().int().positive().default(20000),
  JOB_TIMEOUT_MINUTES: z.coerce.number().positive().default(90),
  TILE_STORAGE_DRIVER: z.enum(["fs", "s3"]).default("fs"),
  TILE_STORAGE_DIR: z.string().default(".data/tiles"),
  S3_ENDPOINT: optional,
  S3_REGION: optional,
  S3_BUCKET: optional,
  S3_ACCESS_KEY_ID: optional,
  S3_SECRET_ACCESS_KEY: optional,
});

export type WorkerEnv = z.infer<typeof schema>;

export function loadEnv(): WorkerEnv {
  return schema.parse(process.env);
}

export function allowlist(env: Pick<WorkerEnv, "GIT_HOST_ALLOWLIST">): string[] {
  return env.GIT_HOST_ALLOWLIST.split(",")
    .map((h) => h.trim().toLowerCase())
    .filter(Boolean);
}
