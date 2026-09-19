// Shared types for the API, web app, and worker.
// The tile set manifest mirrors crates/flyover-tiles/src/lib.rs. Change both together
// and bump TILE_FORMAT_VERSION when the on-disk format changes.

export const TILE_FORMAT_VERSION = 1;

export type JobStatus = "QUEUED" | "RUNNING" | "SUCCEEDED" | "FAILED";

export type ShapeSource = "GENERATED" | "UPLOADED" | "RECTANGLE";

/** How sure we are about a dependency edge. Heuristic edges never get shown as exact. */
export type EdgeConfidence = "exact" | "heuristic";

export interface Bounds {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

export interface LayerDescriptor {
  /** Stable key, used in the storage path: layers/{key}/{z}/{x}/{y}.flv */
  key: string;
  label: string;
  kind: "categorical" | "scalar";
  /** Unit for scalar layers, e.g. "commits", "days", "lines". */
  unit?: string;
  /** Legend entries for categorical layers, index = value stored in the layer tile. */
  categories?: { label: string; color: string }[];
  /** Observed range for scalar layers. */
  range?: { min: number; max: number };
}

/** manifest.json at the root of every tile set. */
export interface TileSetManifest {
  formatVersion: number;
  repo: { name: string; source: string; commitSha: string };
  generatedAt: string;
  bounds: Bounds;
  /** Deepest quadtree level that has tiles. */
  maxZoom: number;
  shape: { source: ShapeSource; seed?: string };
  stats: { files: number; directories: number; lines: number; bytes: number; languages: number };
  layers: LayerDescriptor[];
  hasEdges: boolean;
  hasText: boolean;
}

export interface RepoDto {
  id: string;
  name: string;
  source: string;
  createdAt: string;
  /** Most recent index job, if any. */
  latestJob: IndexJobDto | null;
  /** Most recent finished tile set, if any. */
  latestTileSet: TileSetDto | null;
}

export interface CreateRepoRequest {
  /** https git URL on an allowlisted host. */
  source: string;
  name?: string;
}

/** Where a running job is, written by the worker as it goes. */
export type JobStage = "cloning" | "indexing" | "layout" | "uploading" | "done";

/** Progress and results a worker records on its job. All fields optional until reached. */
export interface JobStats {
  stage?: JobStage;
  files?: number;
  lines?: number;
  tiles?: number;
  maxZoom?: number;
  cloneMb?: number;
  /** Milliseconds spent per stage. */
  timingsMs?: Partial<Record<JobStage, number>>;
}

export interface IndexJobDto {
  id: string;
  repoId: string;
  status: JobStatus;
  commitSha: string | null;
  error: string | null;
  stats: JobStats | null;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
}

export interface TileSetDto {
  id: string;
  repoId: string;
  commitSha: string;
  formatVersion: number;
  fileCount: number;
  lineCount: number;
  maxZoom: number;
  createdAt: string;
}

export interface ApiError {
  error: string;
  message: string;
}
