import type { IndexJob, Repo, TileSet } from "@flyover/db";
import type { IndexJobDto, JobStats, RepoDto, TileSetDto } from "@flyover/types";

export function toJobDto(job: IndexJob): IndexJobDto {
  return {
    id: job.id,
    repoId: job.repoId,
    status: job.status,
    commitSha: job.commitSha,
    error: job.error,
    stats: (job.stats as JobStats | null) ?? null,
    createdAt: job.createdAt.toISOString(),
    startedAt: job.startedAt?.toISOString() ?? null,
    finishedAt: job.finishedAt?.toISOString() ?? null,
  };
}

export function toTileSetDto(t: TileSet): TileSetDto {
  return {
    id: t.id,
    repoId: t.repoId,
    commitSha: t.commitSha,
    formatVersion: t.formatVersion,
    fileCount: t.fileCount,
    lineCount: Number(t.lineCount),
    maxZoom: t.maxZoom,
    createdAt: t.createdAt.toISOString(),
  };
}

export function toRepoDto(repo: Repo & { jobs?: IndexJob[]; tileSets?: TileSet[] }): RepoDto {
  const job = repo.jobs?.[0];
  const tileSet = repo.tileSets?.[0];
  return {
    id: repo.id,
    name: repo.name,
    source: repo.source,
    createdAt: repo.createdAt.toISOString(),
    latestJob: job ? toJobDto(job) : null,
    latestTileSet: tileSet ? toTileSetDto(tileSet) : null,
  };
}

/** Prisma include that brings back the newest job and tile set with each repo. */
export const latestInclude = {
  jobs: { orderBy: { createdAt: "desc" as const }, take: 1 },
  tileSets: { orderBy: { createdAt: "desc" as const }, take: 1 },
};
