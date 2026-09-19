import { config } from "dotenv";
import { z } from "zod";

// One .env at the repo root. Real environment variables always win.
config({ path: [".env", "../../.env"], quiet: true });

const optional = z.string().optional();

const schema = z.object({
  NODE_ENV: z.enum(["development", "test", "production"]).default("development"),
  // Railway injects PORT. API_PORT is the local fallback.
  PORT: z.coerce.number().int().positive().optional(),
  API_PORT: z.coerce.number().int().positive().default(4000),
  API_HOST: z.string().default("0.0.0.0"),
  WEB_ORIGIN: z.string().default("http://localhost:3000"),
  LOG_LEVEL: z.enum(["fatal", "error", "warn", "info", "debug", "trace"]).default("info"),
  // Tile storage the API streams tile sets from (same settings the worker writes with).
  TILE_STORAGE_DRIVER: z.enum(["fs", "s3"]).default("fs"),
  TILE_STORAGE_DIR: z.string().default(".data/tiles"),
  S3_ENDPOINT: optional,
  S3_REGION: optional,
  S3_BUCKET: optional,
  S3_ACCESS_KEY_ID: optional,
  S3_SECRET_ACCESS_KEY: optional,
});

export type Env = z.infer<typeof schema>;

export function loadEnv(): Env {
  return schema.parse(process.env);
}
