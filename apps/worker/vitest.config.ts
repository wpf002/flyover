import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["test/**/*.test.ts"],
    environment: "node",
    // DB-backed tests share one Postgres; run files one at a time.
    fileParallelism: false,
  },
});
