// One job, end to end: validate the source, clone, index, lay out, upload, and record the tile
// set. Progress is written to the job's stats as each stage starts. The job's scratch directory is
// deleted when the job ends, on success and on failure (SPEC 6.10).

import { mkdir, readdir, readFile, rm } from "node:fs/promises";
import { join, relative, sep } from "node:path";

import type { PrismaClient } from "@flyover/db";
import type { TileStorage } from "@flyover/storage";
import type { JobStage, JobStats } from "@flyover/types";

import { clone, gitRead } from "./git.js";
import { runCommand } from "./run.js";
import { validateSource, type Resolver } from "./source.js";

export interface JobContext {
  prisma: PrismaClient;
  storage: TileStorage;
  /** Absolute path to the flyover CLI. */
  flyoverBin: string;
  /** Absolute scratch root; each job gets `<dataDir>/<jobId>`. */
  dataDir: string;
  allowlist: readonly string[];
  maxCloneBytes: number;
  timeoutMs: number;
  signal?: AbortSignal;
  resolver?: Resolver;
  log?: (msg: string, extra?: Record<string, unknown>) => void;
}

interface ManifestLite {
  formatVersion: number;
  maxZoom: number;
  stats: { files: number; lines: number };
  layers: { key: string; label: string; kind: string }[];
}

/** Environment for flyover children: just enough to run, no secrets from ours. */
function cliEnv(): NodeJS.ProcessEnv {
  return { PATH: process.env.PATH ?? "/usr/bin:/bin", LANG: "C" };
}

export async function runJob(ctx: JobContext, job: { id: string; repoId: string }): Promise<void> {
  const { prisma } = ctx;
  const started = Date.now();
  const deadline = started + ctx.timeoutMs;
  const remaining = () => Math.max(1000, deadline - Date.now());
  const jobDir = join(ctx.dataDir, job.id);
  const cloneDir = join(jobDir, "clone");
  const indexDir = join(jobDir, "index");
  const tilesDir = join(jobDir, "tiles");
  const stats: JobStats = { timingsMs: {} };
  let stageStart = Date.now();

  const enter = async (stage: JobStage) => {
    if (stats.stage) stats.timingsMs![stats.stage] = Date.now() - stageStart;
    stats.stage = stage;
    stageStart = Date.now();
    await prisma.indexJob.update({ where: { id: job.id }, data: { stats: { ...stats } } });
    ctx.log?.(`job ${job.id}: ${stage}`);
  };

  try {
    const repo = await prisma.repo.findUniqueOrThrow({ where: { id: job.repoId } });
    await rm(jobDir, { recursive: true, force: true });
    await mkdir(jobDir, { recursive: true });

    const source = await validateSource(repo.source, ctx.allowlist, ctx.resolver);

    await enter("cloning");
    const cloneBytes = await clone(source, cloneDir, {
      home: jobDir,
      timeoutMs: remaining(),
      maxBytes: ctx.maxCloneBytes,
      signal: ctx.signal,
    });
    stats.cloneMb = Math.round(cloneBytes / 1e6);
    const commitSha = await gitRead(cloneDir, jobDir, ["rev-parse", "HEAD"]);
    // The commit time, not the wall clock, so a tile set is reproducible from its commit.
    const committedAt = await gitRead(cloneDir, jobDir, ["log", "-1", "--format=%cI"]);
    await prisma.indexJob.update({ where: { id: job.id }, data: { commitSha } });

    await enter("indexing");
    const index = await runCommand(ctx.flyoverBin, ["index", cloneDir, "-o", indexDir, "--json"], {
      env: cliEnv(),
      timeoutMs: remaining(),
      signal: ctx.signal,
    });
    const indexed = JSON.parse(index.stdout) as { files: number; lines: number };
    stats.files = indexed.files;
    stats.lines = indexed.lines;
    // The clone is no longer needed; free the disk before layout writes the tile set.
    await rm(cloneDir, { recursive: true, force: true });

    await enter("layout");
    const layout = await runCommand(
      ctx.flyoverBin,
      [
        "layout",
        join(indexDir, "index.db"),
        "-o",
        tilesDir,
        "--repo-name",
        repo.name,
        "--repo-source",
        repo.source,
        "--commit-sha",
        commitSha,
        "--generated-at",
        committedAt,
        "--json",
      ],
      { env: cliEnv(), timeoutMs: remaining(), signal: ctx.signal },
    );
    const laid = JSON.parse(layout.stdout) as { tiles: number; maxZoom: number };
    stats.tiles = laid.tiles;
    stats.maxZoom = laid.maxZoom;

    await enter("uploading");
    const prefix = `tilesets/${job.id}`;
    await uploadDir(ctx.storage, tilesDir, prefix, ctx.signal);
    const manifest = JSON.parse(
      await readFile(join(tilesDir, "manifest.json"), "utf8"),
    ) as ManifestLite;

    await enter("done");
    stats.timingsMs!.done = 0;
    await prisma.$transaction(async (tx) => {
      const tileSet = await tx.tileSet.create({
        data: {
          repoId: job.repoId,
          jobId: job.id,
          commitSha,
          formatVersion: manifest.formatVersion,
          storagePrefix: prefix,
          shapeSource: "RECTANGLE",
          fileCount: manifest.stats.files,
          lineCount: BigInt(manifest.stats.lines),
          maxZoom: manifest.maxZoom,
        },
      });
      for (const layer of manifest.layers) {
        await tx.layer.create({
          data: {
            tileSetId: tileSet.id,
            key: layer.key,
            label: layer.label,
            kind: layer.kind,
            meta: JSON.parse(JSON.stringify(layer)),
          },
        });
      }
      await tx.indexJob.update({
        where: { id: job.id },
        data: { status: "SUCCEEDED", finishedAt: new Date(), stats: { ...stats } },
      });
    });
    ctx.log?.(`job ${job.id}: succeeded`, { commitSha, files: stats.files, tiles: stats.tiles });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (stats.stage) stats.timingsMs![stats.stage] = Date.now() - stageStart;
    await prisma.indexJob
      .update({
        where: { id: job.id },
        data: {
          status: "FAILED",
          error: message.slice(0, 2000),
          finishedAt: new Date(),
          stats: { ...stats },
        },
      })
      .catch(() => undefined);
    ctx.log?.(`job ${job.id}: failed`, { error: message });
  } finally {
    await rm(jobDir, { recursive: true, force: true });
  }
}

/** Upload every file under `dir` to `prefix/<relative path>`, a bounded number at a time. */
export async function uploadDir(
  storage: TileStorage,
  dir: string,
  prefix: string,
  signal?: AbortSignal,
): Promise<number> {
  const files = await listFiles(dir);
  let next = 0;
  const worker = async () => {
    while (next < files.length) {
      if (signal?.aborted) throw new Error("aborted");
      const file = files[next++]!;
      const rel = relative(dir, file).split(sep).join("/");
      await storage.put(`${prefix}/${rel}`, await readFile(file));
    }
  };
  await Promise.all(Array.from({ length: 16 }, worker));
  return files.length;
}

/**
 * Every file under `dir`, sorted. The walk is iterative and appends one path at a time: a tile
 * set for a repo the size of Chromium holds hundreds of thousands of files, and `push(...array)`
 * on a list that long overflows the call stack.
 */
export async function listFiles(dir: string): Promise<string[]> {
  const out: string[] = [];
  const pending = [dir];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) pending.push(path);
      else if (entry.isFile()) out.push(path);
    }
  }
  return out.sort();
}
