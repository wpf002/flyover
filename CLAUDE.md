# Flyover: rules for Claude Code sessions

Read `README.md` and `docs/SPEC.md` before changing anything. The spec decides architecture and build order. If you think it's wrong, say so and propose the edit. Don't quietly build something else.

## Scope

- Work one milestone at a time, in the order in SPEC section 8. Don't start the next one in the same session.
- Build what the milestone lists. If something outside it looks worth doing, write it under "Found along the way" in your report and leave it alone.
- Don't add a dependency, service, or env var the spec doesn't call for without saying why first.

## Hard rules

- No mock data and no fake implementations. Unbuilt API routes answer 501, unbuilt CLI commands exit 2, both with a `TODO(Mx)` naming what they need.
- Never execute anything from a repo being indexed. Never follow symlinks. Every rule in SPEC section 6 needs a test when the code it covers lands.
- Pipeline output is deterministic. Same input, same bytes. Seed PRNGs from path hashes. No wall-clock time or hash-map iteration order in output.
- `packages/types/src/index.ts` and `crates/flyover-tiles/src/lib.rs` describe the same manifest. Change them together. Bump the format version on any on-disk change.
- Edges carry `confidence`. Heuristic is never shown as exact.
- Pin every dependency exactly. npm without `^` or `~`. Cargo with `=x.y.z` in the root `[workspace.dependencies]`.
- `unsafe` is forbidden workspace-wide. If wgpu interop needs it, isolate it in one module, lift the lint there only, and explain why in a comment.
- No tile decode, file IO, or large allocation on the render thread.
- Every env var the code reads goes in `.env.example` with a comment and in the README table.

## Checks

Run all of these before saying anything is done, and paste the output:

```bash
pnpm typecheck && pnpm lint && pnpm test && pnpm build && pnpm format:check
pnpm rust:lint && pnpm rust:test && cargo fmt --all -- --check
```

## Reporting

End each session with: what was built, the acceptance criteria from the spec with pass or fail next to each and the command output that proves it, measured numbers with hardware and command, what's short of the criteria, and "Found along the way". Update the Status line in README and the numbers in SPEC section 5 in the same commit.

Performance claims are measured or labeled as targets. Never write a number you didn't get from a run.

## Commits

Conventional commits (`feat(index): ...`, `fix(api): ...`). One logical change per commit. Commit the lockfiles.
