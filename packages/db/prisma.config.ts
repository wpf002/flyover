import { config } from "dotenv";
import { defineConfig } from "prisma/config";

// One .env at the repo root serves every workspace.
config({ path: ["../../.env"], quiet: true });

export default defineConfig({
  schema: "prisma/schema.prisma",
  migrations: { path: "prisma/migrations" },
  datasource: {
    // `prisma generate` runs in CI without a database, so don't throw when unset.
    url: process.env.DATABASE_URL ?? "",
  },
});
