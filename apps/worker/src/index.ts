import { config } from "dotenv";

config({ path: [".env", "../../.env"], quiet: true });

// TODO(M5): the job loop. It doesn't claim jobs yet, on purpose: a worker that claims a job it
// can't run would mark real submissions FAILED.
//
// What it needs to do, in order (docs/SPEC.md, Pipeline and Security):
//   1. Claim one QUEUED IndexJob with SELECT ... FOR UPDATE SKIP LOCKED, set RUNNING + startedAt.
//   2. Validate the repo source: https only, host in GIT_HOST_ALLOWLIST, resolved IP not private.
//   3. Shallow clone into WORKER_DATA_DIR with hooks disabled, no submodules, size and time caps.
//   4. Shell out to FLYOVER_BIN: index -> layout -> tile. Never execute anything from the repo.
//   5. Upload the tile set to storage, insert TileSet + Layer rows, set SUCCEEDED + stats.
//   6. On any failure set FAILED + error. Always delete the clone.

console.log("flyover worker: job loop not implemented yet (M5). Exiting.");
