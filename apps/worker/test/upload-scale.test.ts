// Regression: a Chromium-sized tile set is hundreds of thousands of files, and the directory
// walk used to collect them with `out.push(...(await listFiles(child)))`. Spreading an array that
// long into a call overflows the stack — it killed a real Chromium job at the upload stage after
// it had already spent 42 minutes cloning, 77 seconds indexing, and 3.7 minutes on layout.
//
// The test builds a directory bigger than the spread limit (~125k arguments on Node 24 with the
// default stack). Creating and removing that many files costs a few seconds, which is the price
// of proving the pipeline scales past the point where it actually broke.

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, expect, it } from "vitest";

import { listFiles } from "../src/pipeline.js";

/** Comfortably past the spread limit measured on this runtime. */
const COUNT = 130_000;

let dir: string;

beforeAll(async () => {
  dir = await mkdtemp(join(tmpdir(), "flyover-upload-scale-"));
  const leaf = join(dir, "text", "0");
  await mkdir(leaf, { recursive: true });
  const batch = 2000;
  for (let start = 0; start < COUNT; start += batch) {
    await Promise.all(
      Array.from({ length: Math.min(batch, COUNT - start) }, (_, i) =>
        writeFile(join(leaf, `${start + i}.ftx`), ""),
      ),
    );
  }
  // One file outside the big directory, so the walk has to merge across levels.
  await writeFile(join(dir, "manifest.json"), "{}");
}, 120_000);

afterAll(async () => {
  await rm(dir, { recursive: true, force: true });
}, 120_000);

it("walks a tile set with more files than a spread can carry", async () => {
  const files = await listFiles(dir);
  expect(files).toHaveLength(COUNT + 1);
  // Sorted, so the upload order is the same on every run.
  expect([...files].sort()).toEqual(files);
  expect(files).toContain(join(dir, "manifest.json"));
  expect(files).toContain(join(dir, "text", "0", "0.ftx"));
}, 120_000);
