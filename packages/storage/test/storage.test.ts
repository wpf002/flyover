import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { FsStorage, safeRelativePath, tileSetKey } from "../src/index.js";

async function readAll(stream: NodeJS.ReadableStream): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk as Buffer));
  return Buffer.concat(chunks).toString("utf8");
}

describe("safeRelativePath (SPEC 6.6)", () => {
  it.each([
    "../secret",
    "tiles/../../etc/passwd",
    "a/..",
    "/etc/passwd",
    "a\0b",
    "a\\b",
    "C:/windows",
    "./manifest.json",
    "a//b",
    "",
  ])("rejects %j", (bad) => {
    expect(safeRelativePath(bad)).toBeNull();
  });

  it.each(["manifest.json", "tiles/0/0/0.fly", "layers/language/3/2/1.flv", "index/paths.bin"])(
    "accepts %j",
    (good) => {
      expect(safeRelativePath(good)).toBe(good);
    },
  );

  it("tileSetKey joins only safe paths under the prefix", () => {
    expect(tileSetKey("tilesets/abc", "tiles/0/0/0.fly")).toBe("tilesets/abc/tiles/0/0/0.fly");
    expect(tileSetKey("tilesets/abc", "../other/manifest.json")).toBeNull();
  });
});

describe("FsStorage", () => {
  let dir: string;
  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  it("round-trips an object and reports existence", async () => {
    dir = mkdtempSync(join(tmpdir(), "flyover-storage-"));
    const storage = new FsStorage(join(dir, "root"));
    await storage.put("tilesets/t1/manifest.json", new TextEncoder().encode('{"ok":true}'));

    expect(await storage.exists("tilesets/t1/manifest.json")).toBe(true);
    expect(await storage.exists("tilesets/t1/missing.json")).toBe(false);
    const obj = await storage.get("tilesets/t1/manifest.json");
    expect(obj?.contentType).toBe("application/json");
    expect(await readAll(obj!.body)).toBe('{"ok":true}');
    expect(await storage.get("tilesets/t1/missing.json")).toBeNull();
  });

  it("never reads outside its root, even for a key that would escape", async () => {
    dir = mkdtempSync(join(tmpdir(), "flyover-storage-"));
    writeFileSync(join(dir, "secret.txt"), "TOPSECRET");
    const storage = new FsStorage(join(dir, "root"));

    expect(await storage.get("../secret.txt")).toBeNull();
    expect(await storage.exists("../secret.txt")).toBe(false);
    await expect(storage.put("../escape.txt", new Uint8Array([1]))).rejects.toThrow(/unsafe/);
    // the secret is untouched and nothing was written beside it
    expect(readFileSync(join(dir, "secret.txt"), "utf8")).toBe("TOPSECRET");
  });
});
