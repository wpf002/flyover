# Flyover spec

Source of truth for architecture and build order. If code and this file disagree, fix one of them in the same commit.

## 1. Product

Input: a git URL or a local path. Output: a 3D map of that codebase you can fly through, in a native app and in the browser.

Four properties define it.

1. **Any repo, any size.** The renderer never sees the repo. It sees tiles. Frame cost depends on what's on screen, so a 50K-line repo and a 51M-line repo render the same way.
2. **Any language.** Tier 1 indexing (files, lines, symbols, raw import strings) uses tree-sitter and works on everything. Tier 2 (exact cross-file edges) consumes SCIP indexes when one exists and falls back to heuristics when it doesn't.
3. **A shape per repo.** The map fills a silhouette: uploaded by the user as SVG, or generated from the repo's identity so every codebase has its own footprint.
4. **Layers.** Churn, age, ownership, coverage, CVEs, agent-touched files. Each is a data layer bound to color or height. The flythrough is the demo. The layers are the product.

## 2. Pipeline

```
acquire -> index -> layout -> tile -> serve -> render
           (flyover-index)  (flyover-layout, flyover-tiles)   (apps/api)  (flyover-render)
```

Each stage reads the previous stage's output from disk and writes its own. Each is a `flyover` subcommand, so any stage can be re-run alone. The worker (apps/worker) chains them for submitted URLs.

### 2.1 Acquire

Local path, or `git clone --depth 1` at a resolved commit SHA. Rules in section 6.

### 2.2 Index (`flyover index <path> -o <dir>`)

Walk with the `ignore` crate: honors .gitignore, never follows symlinks, skips `.git`. On top of that, a built-in exclusion list for vendored and generated code (`node_modules`, `third_party`, `vendor`, `dist`, lockfiles, `*.min.js`, files with a generated-code header). Exclusions are recorded, not silently dropped, and `--include-vendored` turns them off. Chromium's `third_party` is most of its bulk, so this flag changes the picture a lot.

Per file: repo-relative path, language, bytes, lines, blake3 content hash, binary/oversize flags. Files over 2 MB or with a NUL in the first 8 KB are recorded but not parsed.

Per parsed file, through tree-sitter tags queries: symbols (kind, name, start line, end line) and raw import strings. Start with TypeScript/JavaScript, Python, Rust, Go, C, C++, Java, C#, Ruby, PHP. A language without a grammar still gets file-level data.

Parallel with rayon. Output is one SQLite file, `index.db` (rusqlite, bundled): tables `files`, `symbols`, `imports`, `edges`, `excluded`, `meta`. SQLite because it's one file, queryable while debugging, and supports incremental re-index keyed on content hash. Rows are inserted in path order so the same tree produces the same database content.

Edges (M7). Heuristic resolvers per language: relative paths and tsconfig `paths` for TS/JS, package-relative for Python, suffix match against the file list for C/C++ includes, module paths for Rust/Go/Java. `flyover index --scip <file>` ingests a SCIP index and overwrites heuristic edges with exact ones. Every edge row has `confidence`: `exact` or `heuristic`.

### 2.3 Layout (`flyover layout <index.db> -o <tileset-dir>`)

Input: the directory tree, weight = lines per file (minimum 1).

M2 uses a squarified treemap in a rectangle so the rest of the pipeline can be built against real tiles. M6 replaces it with a weighted Voronoi treemap (power diagram plus Lloyd relaxation) clipped to a polygon, recursing per directory inside its parent's cell, stopping at 1% area error or an iteration cap.

Shape sources:

- `GENERATED`: a closed outline built from a seed. Seed = blake3 of the normalized repo URL (or the root commit SHA for local repos), so it's stable across commits. Radial harmonics with seed-chosen lobe count and amplitudes. One repo, one outline, forever.
- `UPLOADED`: an SVG path. Flatten curves, take the outer ring and its holes, then clean up: morphological open to remove slivers thinner than a minimum feature width, drop holes below a minimum area. Parse path data only. Section 6 has the input rules.
- `RECTANGLE`: M2 default, kept as an option.

Determinism and stability: site positions are seeded from a hash of each node's path. When a previous layout for the same repo exists, seed from its centroids so files stay put between commits.

World units: area is proportional to lines. Height and color are not fixed by layout. They're bound to layers at render time. Defaults: color = language, height = log2(lines).

### 2.4 Tile

Quadtree over the world bounds. Tile `(z, x, y)` covers 1/4^z of the world. A tile at zoom z holds the features worth drawing at that zoom:

- A directory whose cell is below the size threshold at z is one aggregated slab (its color is the line-weighted mix of its children under the active layer).
- A directory above the threshold is dropped in favor of its children.
- Files appear individually once their cell passes the threshold.

Subdivide until every leaf tile is under budget: 50K features and 256 KB compressed. `maxZoom` in the manifest is the deepest level produced.

Tile set layout in storage, all paths relative to the tile set prefix:

```
manifest.json                      TileSetManifest, see packages/types and flyover-tiles
tiles/{z}/{x}/{y}.fly              geometry
layers/{key}/{z}/{x}/{y}.flv       one value per feature, same order as the geometry tile
edges/{z}/{x}/{y}.fle              dependency edges, aggregated per zoom (M7)
text/{fileId >> 12}/{fileId}.ftx   source text plus token spans, fetched per file on demand (M4)
index/paths.bin                    fileId -> path and stats, for picking and search
```

`.fly` v1, little-endian, zstd-compressed as a whole:

```
header     magic "FLY1", format version u32, z u8, x u32, y u32, feature count u32
features   per feature: id u32, kind u8 (dir|file), depth u8, parent id u32, lines u32,
           vertex offset u32, vertex count u32, index offset u32, index count u32
vertices   f32 x, f32 y pairs, tile-local coordinates
indices    u32, pre-triangulated (earcut) so the client does no geometry work
```

Extrusion happens in the vertex shader from the bound height layer, so height changes never touch geometry. Layer tiles (`.flv`) are a header plus a `f32` or `u16` array aligned to the feature table. That's why adding a layer never rewrites geometry.

Text tiles (`.ftx`) hold the file's UTF-8 text and token spans (start, length, token class) produced at index time from tree-sitter highlights, so the renderer ships no parsers.

Tile sets are immutable. New commit, new tile set, new prefix.

### 2.5 Serve

`GET /tilesets/:id/manifest` and `GET /tilesets/:id/files/*` stream from the storage driver (`fs` or `s3`, one interface) with `Cache-Control: public, max-age=31536000, immutable`. In production the bucket or a CDN can serve tiles directly. The API route exists so local dev and auth work without one.

### 2.6 Render (flyover-render)

wgpu. One codebase, two targets: native through winit, browser through wasm32 and WebGPU. No WebGL fallback in v1.

- Camera: fly mode (WASD plus mouse look, speed scales with altitude) and map mode (orbit, pan, zoom). Smooth transition from overview into a file.
- Tile selection each frame: walk the quadtree, frustum cull, pick the zoom per region by screen-space error, request what's missing, draw the best loaded ancestor until it arrives. Cross-fade on LOD change.
- Cache: LRU under a GPU memory budget (default 1.5 GB native, 512 MB web). Request queue ordered by screen-space error, cancelled when a tile leaves view.
- Threads: fetch, zstd decode, and buffer prep happen off the render thread (native threads, web workers). The render thread only uploads and draws.
- Drawing: one pipeline for extruded cells, storage buffers for per-feature layer values, so rebinding a layer is a buffer swap.
- Picking: feature ids into an offscreen target, read back one pixel under the cursor. Hover shows path and stats from `index/paths.bin`.
- Text (M4): MSDF atlas of one monospace font. Files whose projected line height is 6 px or more draw real glyphs as instanced quads, colored by token class. Between 1 and 6 px they draw one colored strip per line from the token spans. Below 1 px the roof is flat color.
- Edges (M7): arcs between cells, bundled per zoom level so low zoom shows directory-to-directory flows.

## 3. Layers

A layer is one value per file (later per symbol) plus a descriptor (`LayerDescriptor`): key, label, kind (`categorical` or `scalar`), unit, legend or range. The viewer binds any layer to color and any scalar layer to height.

| Key        | Source                                                             | Milestone |
| ---------- | ------------------------------------------------------------------ | --------- |
| `language` | index                                                              | M2        |
| `lines`    | index                                                              | M2        |
| `churn`    | `git log --numstat` over a window, commits and lines changed       | M8        |
| `age`      | days since last commit touching the file                           | M8        |
| `owner`    | author with the most commits to the file, emails hashed            | M8        |
| custom     | CSV or JSON upload: `path,value`                                   | M8        |
| `agent`    | share of commits with AI co-author trailers                        | M8        |
| `vulns`    | OSV lookups on lockfiles and manifests, mapped to importing files  | M10       |
| `surface`  | reachability from network and IPC entry points over the edge graph | M10       |

Churn, age, and owner need history, so the worker does a second, deeper fetch only when those layers are requested.

## 4. Services

- **apps/api** Fastify. `repos`, `jobs`, `tilesets`, tile streaming, layer upload. Unbuilt routes answer 501.
- **apps/worker** Node process. Claims `IndexJob` rows with `SELECT ... FOR UPDATE SKIP LOCKED`, runs the pipeline by shelling out to `FLYOVER_BIN`, uploads the tile set, writes `TileSet` and `Layer` rows. Postgres is the queue. No Redis.
- **apps/web** Next.js. Repo list, submit form, job progress, viewer page that mounts the wasm renderer, layer picker.
- **packages/db** Prisma models: `Repo`, `IndexJob`, `TileSet`, `Layer`.

## 5. Scale targets

Targets, not measurements. Replace each with a measured number, the hardware, and the command as milestones land.

| Case                                    | Target                                         |
| --------------------------------------- | ---------------------------------------------- |
| 1M lines, laptop                        | index under 60 s, full pipeline under 2 min    |
| Chromium, 8 cores                       | index under 30 min, peak RSS under 8 GB        |
| Renderer, native, Apple M-series, 1440p | 120 fps sustained while flying, under 3 GB RAM |
| Renderer, web                           | 60 fps, under 1.5 GB                           |
| Any tile                                | under 256 KB compressed, decode under 4 ms     |

### Measured

Hardware: Apple M5 (10 cores), 24 GB, macOS (Darwin 27.0.0). Release build (`pnpm rust:build`).

| Stage (milestone)   | Input                                                 | Wall-clock | Peak RSS | Command                                               |
| ------------------- | ----------------------------------------------------- | ---------- | -------- | ----------------------------------------------------- |
| Index tier 1 (M1)   | postgres, 4,378,916 lines / 7,694 files               | 1.04 s     | 82.8 MiB | `/usr/bin/time -l flyover index <postgres> -o <out>`  |
| Index tier 1 (M1)   | this repo, 4,275 lines / 85 files                     | 0.48 s     | 37.1 MiB | `/usr/bin/time -l flyover index . -o <out>`           |
| Layout + tiles (M2) | postgres index (7,694 files → 7,840 tiles, maxZoom 7) | 8.06 s     | 98.3 MiB | `/usr/bin/time -l flyover layout <index.db> -o <out>` |

Index and layout are both deterministic: two runs on postgres produce byte-identical output (`index.db` sha256 matches; the 23,523-file tile set hashes identically). Target for "1M lines, laptop" is index under 60 s and full pipeline under 2 min; the 4.4M-line repo indexes in ~1 s and lays out in ~8 s. Largest geometry tile is 33 KB, under the 256 KB budget. Renderer rows stay targets until M3.

## 6. Security

The pipeline reads untrusted repos and untrusted uploads. These rules are requirements, and each needs a test.

1. Never execute anything from an indexed repo. No build scripts, no package installs, no git hooks, no LFS smudge filters.
2. Clone with `GIT_TERMINAL_PROMPT=0`, `-c core.hooksPath=/dev/null`, `-c protocol.file.allow=never`, `--depth 1`, `--no-recurse-submodules`, `GIT_LFS_SKIP_SMUDGE=1`.
3. Accept only `https://` URLs whose host is in `GIT_HOST_ALLOWLIST`. Resolve the host and refuse private, loopback, link-local, and metadata addresses before cloning. No redirects to other hosts.
4. Never follow symlinks while walking. Never read outside the clone root. Canonicalize and check the prefix on every path that came from repo content.
5. Cap clone size (`JOB_MAX_CLONE_MB`), wall-clock (`JOB_TIMEOUT_MINUTES`), per-file size, and parse time per file. A tree-sitter parse that runs past its budget is abandoned and the file keeps file-level data only.
6. Tile serving: reject any requested path with `..`, a leading `/`, or a NUL before touching storage. Serve only under the tile set's prefix.
7. SVG upload: size cap, parse with an XML parser that has external entities and DTDs off, read only `d` attributes of `path` elements, ignore everything else. The uploaded file is never stored or served back.
8. Custom layer upload: size cap, row cap, paths matched against the tile set's file list only.
9. Author emails are hashed before they leave the worker. Tile sets for private repos need auth before M5 ships to anything public.
10. Delete the clone when the job ends, on success and on failure.

## 7. Shapes and trademarks

Users upload their own marks for their own repos. Anything we publish (demos, screenshots, marketing) uses generated shapes or marks we have written permission to use.

## 8. Milestones

One milestone per working session. Each ends with its acceptance checks run and their output pasted into the session report. Anything short of the criteria gets reported as short.

**M0 Scaffold.** Done by `bootstrap.sh`. Monorepo builds, `flyover scan` works, `GET/POST /repos` hit Postgres.

**M1 Indexer, tier 1.** `flyover index <path> -o <dir>` writes `index.db` with files, symbols, imports (raw strings), exclusions. Ten languages.
Accept: fixture repos under `crates/flyover-index/tests/fixtures` with golden row counts. Two runs on the same tree produce identical table contents. Symlink-escape and oversize fixtures pass. Runs on this repo and on a shallow clone of a repo over 1M lines, with wall-clock and peak RSS recorded in section 5.

**M2 Layout and tiles, rectangular.** Squarified treemap, quadtree tiler, `.fly` v1 encode and decode in flyover-tiles, `manifest.json`, `language` and `lines` layers, `index/paths.bin`.
Accept: encode/decode round-trip tests. Every file in the index appears in exactly one leaf tile. Sum of cell areas equals world area within 0.1%. No tile over budget. Identical output across two runs.

**M3 Native renderer.** `flyover view <tileset-dir>` opens a window: fly and map cameras, LOD selection, LRU cache, async loading, picking with hover info, color by layer, height by layer.
Accept: opens the M2 tile set of the 1M-line repo. Frame time logged with `--bench` over a scripted camera path. No tile decode on the render thread (assert in debug builds).

**M4 Text.** Token spans at index time, `.ftx` tiles, MSDF atlas, glyph and strip rendering with the 6 px and 1 px thresholds.
Accept: a file's text is legible at close range and matches the source byte for byte. Frame time on the scripted path stays inside the M3 budget plus 2 ms.

**M5 Web, end to end.** wasm build of the renderer, viewer page, storage drivers (`fs`, `s3`), job routes, the worker loop with every rule in section 6, submit form and progress UI, worker Dockerfile, Railway deploy of all services.
Accept: submit a public GitHub URL in the browser, watch the job finish, fly the result. SSRF, path traversal, hook execution, and symlink tests pass. A deployed URL exists.

**M6 Shapes.** Weighted Voronoi treemap in a polygon, generated silhouettes, SVG upload with cleanup, stability seeding from the previous layout.
Accept: area error per cell under 1% on fixtures. Same repo gives the same outline across commits. Re-layout after a small commit moves unaffected file centroids by under 1% of world width. Malicious SVG fixtures are refused or neutralized.

**M7 Edges.** Heuristic resolvers, SCIP ingestion, `.fle` tiles with per-zoom bundling, edge rendering with confidence shown.
Accept: resolver precision and recall measured against SCIP ground truth on two repos and recorded here. Heuristic edges are visually distinct from exact ones.

**M8 Layers.** churn, age, owner, agent, custom upload, layer picker UI, legends.
Accept: adding a layer to an existing tile set writes only under `layers/`. Geometry tile hashes unchanged.

**M9 Scale pass.** Chromium. Streaming index writes, memory caps, incremental re-index by content hash, tile budget tuning, renderer profiling.
Accept: section 5 targets met or the gap measured and explained.

**M10 Security layers.** `vulns` from OSV, `surface` from entry-point reachability.
Accept: known-vulnerable fixture lockfiles light up the right files. Every finding links to its OSV id.
