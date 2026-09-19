import { createReadStream } from "node:fs";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { dirname, resolve, sep } from "node:path";

import { contentTypeFor, safeRelativePath, type StoredObject, type TileStorage } from "./core.js";

/** Local directory storage. Every key is validated and must resolve inside the root. */
export class FsStorage implements TileStorage {
  readonly driver = "fs" as const;
  private readonly root: string;

  constructor(root: string) {
    this.root = resolve(root);
  }

  private path(key: string): string | null {
    if (safeRelativePath(key) === null) return null;
    const full = resolve(this.root, key);
    return full.startsWith(this.root + sep) ? full : null;
  }

  async put(key: string, body: Uint8Array): Promise<void> {
    const path = this.path(key);
    if (!path) throw new Error(`refusing unsafe storage key: ${JSON.stringify(key)}`);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, body);
  }

  async get(key: string): Promise<StoredObject | null> {
    const path = this.path(key);
    if (!path) return null;
    try {
      const info = await stat(path);
      if (!info.isFile()) return null;
      return { body: createReadStream(path), size: info.size, contentType: contentTypeFor(key) };
    } catch {
      return null;
    }
  }

  async exists(key: string): Promise<boolean> {
    const path = this.path(key);
    if (!path) return false;
    try {
      return (await stat(path)).isFile();
    } catch {
      return false;
    }
  }
}
