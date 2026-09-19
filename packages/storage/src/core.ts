// Shared storage types and key rules. Kept separate from index.ts so the drivers can import
// them without a module cycle.

import type { Readable } from "node:stream";

export interface StoredObject {
  body: Readable;
  size: number | undefined;
  contentType: string;
}

export interface TileStorage {
  readonly driver: "fs" | "s3";
  put(key: string, body: Uint8Array, contentType?: string): Promise<void>;
  /** The object, or null when it does not exist (or the key is unsafe). */
  get(key: string): Promise<StoredObject | null>;
  exists(key: string): Promise<boolean>;
}

/** Cache header for anything under a tile set prefix: the bytes never change. */
export const IMMUTABLE = "public, max-age=31536000, immutable";

/**
 * SPEC 6.6: validate a path requested under a tile set before touching storage. Rejects `..`,
 * a leading `/`, NUL bytes, backslashes, drive letters, and empty or `.` segments. Returns the
 * path unchanged when safe, otherwise null.
 */
export function safeRelativePath(rel: string): string | null {
  if (rel.length === 0 || rel.length > 1024) return null;
  if (rel.includes("\0") || rel.includes("\\")) return null;
  if (rel.startsWith("/")) return null;
  if (/^[a-zA-Z]:/.test(rel)) return null;
  const parts = rel.split("/");
  if (parts.some((p) => p === "" || p === "." || p === "..")) return null;
  return rel;
}

/** Storage key for a file inside a tile set, or null if the requested path is unsafe. */
export function tileSetKey(prefix: string, rel: string): string | null {
  const safe = safeRelativePath(rel);
  return safe === null ? null : `${prefix}/${safe}`;
}

const TYPES: Record<string, string> = {
  json: "application/json",
  fly: "application/octet-stream",
  flv: "application/octet-stream",
  fle: "application/octet-stream",
  ftx: "application/octet-stream",
  bin: "application/octet-stream",
};

export function contentTypeFor(key: string): string {
  const ext = key.slice(key.lastIndexOf(".") + 1).toLowerCase();
  return TYPES[ext] ?? "application/octet-stream";
}
