# Claude Code build prompt

Paste everything below the line into Claude Code, opened at the repo root. For later sessions, change the milestone in the first line of "This session" and paste it again.

---

You're building Flyover in this repo. Flyover takes any git repo and turns it into a 3D map you can fly through: directories are districts, files are blocks sized by line count, source text is readable up close. It has to work the same on a 5K-line repo and on Chromium (about 51M lines), which is why the renderer only ever sees quadtree tiles streamed by camera position, never the repo. Each repo's map fills its own silhouette (generated from the repo's identity, or an SVG the user uploads), and metrics like churn, ownership, and CVEs are data layers bound to color and height.

The scaffold already exists. TypeScript monorepo (pnpm, Turborepo, Next.js, Fastify, Prisma, Postgres, Railway) for the web app, API, and worker. Rust Cargo workspace for the indexer, layout, tile format, wgpu renderer, and CLI. Today `flyover scan` works and `GET/POST /repos` hit Postgres. Everything else is a stub that answers 501 or exits 2.

## Before you write anything

1. Read `CLAUDE.md`, `README.md`, and `docs/SPEC.md` in full. The spec decides architecture, the tile format, the security rules, and the milestone order. Follow it. If you think part of it is wrong, stop and tell me what you'd change and why. Don't build around it.
2. Prove the scaffold is healthy on this machine. Run the README's local setup and the full check list from `CLAUDE.md`. `pnpm db:migrate --name init` has never been run against a real database, so watch that step, commit the migration it creates, and confirm `POST /repos` then `GET /repos` return real rows. Fix anything broken before starting the milestone and tell me what it was.

## This session

Build milestone **M1** from `docs/SPEC.md` section 8, and only M1.

Work in this order:

1. Restate the milestone's scope and acceptance criteria in your own words, list the files you expect to create or change, and list every dependency you plan to add with its exact version. Wait for my OK if you want to add anything the spec doesn't call for. Otherwise go.
2. Write the failing tests and fixtures first, including the security fixtures the milestone touches (SPEC section 6).
3. Build the thinnest path that goes end to end, run it on this repo, then widen.
4. Run it on a real mid-size repo, not just fixtures. Record wall-clock and peak memory with the hardware and the exact command.
5. Run every check in `CLAUDE.md`. Paste the output.

## Rules that matter most

- No mock data and no fake implementations. If something isn't built, it answers 501 or exits 2 with a `TODO(Mx)` that names what it needs.
- Never execute anything from a repo being indexed. Never follow symlinks. Never read outside the root.
- Deterministic output. Same input, same bytes. Prove it with a test that runs the stage twice and compares.
- Pin every dependency exactly. Look up the current version when you add it. Cargo pins go in the root `[workspace.dependencies]`.
- The manifest is defined twice, in `packages/types/src/index.ts` and `crates/flyover-tiles/src/lib.rs`. Change both together.
- Never write a performance number you didn't measure. Targets stay labeled as targets.
- Stay inside the milestone. Put anything else you notice under "Found along the way" in your report.

## When you finish

Give me a report with these parts, in this order:

1. What you built, in a few sentences.
2. Each acceptance criterion from the spec, marked pass or fail, with the command and output that proves it.
3. Measured numbers, with hardware and command.
4. Anything short of the criteria. Say it plainly. Don't describe partial work as done.
5. Found along the way.

Then update the Status line in `README.md`, put measured numbers into `docs/SPEC.md` section 5, and commit with conventional commit messages. Don't push. Don't start M2.
