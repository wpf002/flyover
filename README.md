# Flyover

## What this is

Point Flyover at any git repo and it builds a 3D map of the code you can fly through: directories are districts, files are blocks sized by line count, and the source text is readable when you get close. It works the same on a 5K-line repo and on Chromium, each repo gets its own silhouette, and metrics like churn, ownership, and CVEs show up as layers on the map.

## Status

M3 done. The pipeline runs end to end from the CLI: `flyover index` builds `index.db` (files, symbols, imports, exclusions; ten languages), `flyover layout` cuts it into a deterministic quadtree tile set, and `flyover view` opens it in a native wgpu window (map and fly cameras, LOD streaming off the render thread, LRU GPU cache, hover picking, color and height bound to layers). `flyover view --screenshot` and `--bench` render headlessly. The web app registers repos; the in-browser viewer and the job worker land in M5, source text in M4. Milestones are in [docs/SPEC.md](docs/SPEC.md).

## Stack

| Layer                      | Choice                                          |
| -------------------------- | ----------------------------------------------- |
| Language (services, web)   | TypeScript, strict                              |
| Language (indexer, render) | Rust 1.95, wgpu from M3                         |
| Package manager            | pnpm 10                                         |
| Monorepo                   | Turborepo for TS, Cargo workspace for Rust      |
| Web                        | Next.js 16, App Router                          |
| API                        | Fastify 5                                       |
| ORM                        | Prisma 7 with the `pg` driver adapter           |
| Database                   | Postgres 16. Also the job queue (`SKIP LOCKED`) |
| Tile storage               | Local directory in dev, S3-compatible in prod   |
| Hosting                    | Railway                                         |

Rust is the one departure from the standard stack. Parsing 51M lines and drawing them at 120 Hz needs native threads, tight memory control, and a GPU API that also compiles to the browser. wgpu gives one renderer for native and WebGPU.

## Local setup

Needs Node 22, pnpm 10, Rust 1.95 (rustup reads `rust-toolchain.toml`), and Postgres 16.

```bash
git clone git@github.com:wpf002/flyover.git
cd flyover

# Postgres, if you don't have one running
docker run --name flyover-pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=flyover -p 5432:5432 -d postgres:16

pnpm install
cp .env.example .env
pnpm db:migrate --name init
pnpm rust:build   # the worker shells out to target/release/flyover
pnpm dev          # web, api, and the job worker
```

Web is on http://localhost:3000, API on http://localhost:4000.

```bash
# Add a repo and read it back
curl -X POST localhost:4000/repos -H 'content-type: application/json' \
  -d '{"source":"https://github.com/wpf002/flyover.git"}'
curl localhost:4000/repos

# Rust side
cargo test --workspace
cargo run -p flyover-cli -- scan .
```

Map a repo and fly it (release build; any local checkout works):

```bash
pnpm rust:build
target/release/flyover index path/to/repo -o .data/work/repo
target/release/flyover layout .data/work/repo/index.db -o .data/tiles/repo
target/release/flyover view .data/tiles/repo
```

In the viewer: drag to orbit, scroll to zoom, WASD to pan. Tab switches to fly mode (WASD to move, Q/E down/up, drag to look, scroll for speed). Hovering a block puts its path and line count in the title bar. Headless: `--screenshot shot.png [--path-t 0..1]` renders one frame, `--bench --frames 600` prints frame-time percentiles over a scripted camera path.

Checks, all of which must pass before a commit:

```bash
pnpm typecheck && pnpm lint && pnpm test && pnpm build && pnpm format:check
pnpm rust:lint && pnpm rust:test && cargo fmt --all -- --check
```

## Environment variables

| Name                   | Required?      | Where to get it                                                         |
| ---------------------- | -------------- | ----------------------------------------------------------------------- |
| `DATABASE_URL`         | yes            | Local default in `.env.example`. Railway: `${{Postgres.DATABASE_URL}}`  |
| `API_PORT`             | no, 4000       | Local only. Railway's `PORT` wins                                       |
| `API_HOST`             | no, 0.0.0.0    | Leave it                                                                |
| `WEB_ORIGIN`           | yes in prod    | Public URL of the web service, used for CORS                            |
| `LOG_LEVEL`            | no, info       | pino level                                                              |
| `API_URL`              | yes in prod    | URL the web server uses to reach the API. Railway private URL works     |
| `NEXT_PUBLIC_API_URL`  | yes in prod    | Public API URL, used by the browser to fetch tiles                      |
| `TILE_STORAGE_DRIVER`  | no, fs         | `fs` or `s3`                                                            |
| `TILE_STORAGE_DIR`     | when driver=fs | Any writable directory                                                  |
| `S3_ENDPOINT`          | when driver=s3 | Bucket provider                                                         |
| `S3_REGION`            | when driver=s3 | Bucket provider                                                         |
| `S3_BUCKET`            | when driver=s3 | Bucket provider                                                         |
| `S3_ACCESS_KEY_ID`     | when driver=s3 | Bucket provider                                                         |
| `S3_SECRET_ACCESS_KEY` | when driver=s3 | Bucket provider                                                         |
| `WORKER_DATA_DIR`      | no, .data/work | Scratch space for clones and indexes                                    |
| `FLYOVER_BIN`          | worker only    | Path to the built CLI, `target/release/flyover` after `pnpm rust:build` |
| `GIT_HOST_ALLOWLIST`   | worker only    | Comma-separated hosts the worker may clone from                         |
| `JOB_MAX_CLONE_MB`     | no, 20000      | Clone size cap per job                                                  |
| `JOB_TIMEOUT_MINUTES`  | no, 90         | Wall-clock cap per job                                                  |

The API reads the `TILE_STORAGE_*` and `S3_*` variables to serve tile sets; the worker reads `DATABASE_URL`, `WORKER_DATA_DIR`, `FLYOVER_BIN`, `GIT_HOST_ALLOWLIST`, `JOB_MAX_CLONE_MB`, `JOB_TIMEOUT_MINUTES`, and the same storage variables. Relative directories resolve against the repo root, so the API and worker agree on one `.data/`.

## Project structure

```
apps/web                 Next.js: landing, repo list, add-repo form (POST /repos); wasm viewer page in M5
apps/api                 Fastify: repos, index jobs, tile sets, and immutable tile streaming from storage
apps/worker              Claims index jobs (SKIP LOCKED), hardened shallow clone, runs the CLI, uploads tile sets
packages/db              Prisma schema, migrations, and the client (getPrisma)
packages/types           Shared TS types: DTOs and the tile set manifest
packages/storage         Tile storage behind one interface: fs (dev) and S3-compatible (prod), path guard
packages/config          Shared tsconfig bases and the ESLint flat config
crates/flyover-tiles     Tile set format: manifest, .fly geometry, .flv layers, paths.bin
crates/flyover-index     Repo walk, exclusions, tree-sitter symbols and imports -> index.db
crates/flyover-layout    Squarified treemap and quadtree tiler; Voronoi-in-a-shape in M6
crates/flyover-render    wgpu renderer: native window + headless screenshot/bench; wasm in M5
crates/flyover-cli       The `flyover` binary: scan, index, layout, view
docs/SPEC.md             Architecture, tile format, security rules, milestones with acceptance criteria
docs/BUILD_PROMPT.md     The prompt to paste into Claude Code, one milestone per session
CLAUDE.md                Rules for Claude Code sessions in this repo
```

## Deploy

Railway project `flyover`, four services:

| Service    | Source                                         | Notes                                                           |
| ---------- | ---------------------------------------------- | --------------------------------------------------------------- |
| `Postgres` | Railway Postgres plugin                        |                                                                 |
| `api`      | this repo, config file `apps/api/railway.json` | Runs `pnpm db:deploy` before each deploy. Healthcheck `/health` |
| `web`      | this repo, config file `apps/web/railway.json` |                                                                 |
| `worker`   | not set up                                     | Needs a Dockerfile that builds the Rust CLI. Lands in M5        |

For `api` and `web`, leave the service root directory at the repo root and set the config-as-code path to the `railway.json` above. Both build from the monorepo root so workspace packages resolve.

Wire `DATABASE_URL` on `api` (and later `worker`) as a reference variable: `${{Postgres.DATABASE_URL}}`. Don't paste the connection string.

```bash
railway link            # pick the flyover project
railway up --service api
railway up --service web
```

Nothing has been deployed yet.

## Conventions

- A stub answers `501` (API) or exits `2` (CLI) and carries a `TODO(Mx)` saying what it needs. No mock data anywhere, ever.
- `packages/types/src/index.ts` and `crates/flyover-tiles/src/lib.rs` define the same manifest. Change both in one commit and bump the format version if the on-disk format changes.
- Everything the pipeline produces is deterministic. Same commit in, same bytes out. Seed any PRNG from a hash of the path.
- The indexer never executes anything from a repo it's reading and never follows symlinks. The worker only clones from `GIT_HOST_ALLOWLIST`. Full list in docs/SPEC.md under Security.
- Dependency edges carry `confidence: exact | heuristic`. A heuristic edge is never displayed as exact.
- Tile sets are immutable. A new commit makes a new tile set. Adding a layer never rewrites geometry tiles.
- Dependencies are pinned exactly. npm: no `^` or `~`. Cargo: `=x.y.z` in the root `[workspace.dependencies]`, members use `workspace = true`.
- The one `.env` lives at the repo root. Every variable the code reads is in `.env.example` with a comment.
- TS workspaces import each other through built `dist/`. `pnpm dev` builds packages first. After changing `packages/*`, rebuild it or restart `pnpm dev`.
- ESLint 10 only reads flat config, so there's `eslint.config.mjs` and no `.eslintrc`.
- Performance numbers in docs are measured on named hardware with the command that produced them, or they're labeled as targets.
