// SPEC 6.1 and 6.2: clone without ever executing anything from the repo or the machine's git
// config. Every protection is layered: the environment isolates git from system and user config,
// and explicit -c flags disable hooks, fsmonitor, LFS filters, non-https protocols, redirects, and
// symlink checkout. The connection is pinned to the address the SSRF check already vetted.

import { join } from "node:path";

import { dirSize, runCommand } from "./run.js";
import type { ValidatedSource } from "./source.js";

/** -c flags applied to every git invocation on untrusted content. */
export const SAFE_CONFIG: readonly string[] = [
  "core.hooksPath=/dev/null",
  "core.fsmonitor=false",
  "core.symlinks=false",
  "protocol.allow=never",
  "protocol.https.allow=always",
  "protocol.file.allow=never",
  "http.followRedirects=false",
  "filter.lfs.smudge=",
  "filter.lfs.process=",
  "filter.lfs.required=false",
].flatMap((kv) => ["-c", kv]);

/**
 * Arguments for a shallow, hook-free clone. `extraConfig` exists for tests that need a
 * file:// fixture remote; production never passes it.
 */
export function cloneArgs(
  source: ValidatedSource | { url: string; host?: undefined; address?: undefined },
  dir: string,
  extraConfig: readonly string[] = [],
): string[] {
  const pin =
    source.host && source.address
      ? ["-c", `http.curloptResolve=${source.host}:443:${source.address}`]
      : [];
  return [
    ...SAFE_CONFIG,
    ...pin,
    ...extraConfig,
    "clone",
    "--depth",
    "1",
    "--no-recurse-submodules",
    "--single-branch",
    "--no-tags",
    "--quiet",
    "--",
    source.url,
    dir,
  ];
}

/** Environment for git children: no system or user config, no prompts, no LFS downloads. */
export function gitEnv(home: string): NodeJS.ProcessEnv {
  return {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    HOME: home,
    LANG: "C",
    GIT_TERMINAL_PROMPT: "0",
    GIT_LFS_SKIP_SMUDGE: "1",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_ASKPASS: "",
    SSH_ASKPASS: "",
  };
}

export interface CloneOptions {
  home: string;
  timeoutMs: number;
  maxBytes: number;
  signal?: AbortSignal;
}

/** Shallow-clone `source` into `dir`, killing git if the download passes `maxBytes`. */
export async function clone(
  source: ValidatedSource,
  dir: string,
  opts: CloneOptions,
): Promise<number> {
  const packDir = join(dir, ".git", "objects", "pack");
  await runCommand("git", cloneArgs(source, dir), {
    env: gitEnv(opts.home),
    timeoutMs: opts.timeoutMs,
    signal: opts.signal,
    tickMs: 2000,
    onTick: async () => {
      const size = await dirSize(packDir);
      return size > opts.maxBytes
        ? `exceeded the ${Math.round(opts.maxBytes / 1e6)} MB clone cap`
        : null;
    },
  });
  const total = await dirSize(dir);
  if (total > opts.maxBytes) {
    throw new Error(
      `clone is ${Math.round(total / 1e6)} MB, over the ${Math.round(opts.maxBytes / 1e6)} MB cap`,
    );
  }
  return total;
}

/** Read one value from the cloned repo's metadata. Hooks and user config stay off. */
export async function gitRead(dir: string, home: string, args: readonly string[]): Promise<string> {
  const out = await runCommand("git", [...SAFE_CONFIG, "-C", dir, ...args], {
    env: gitEnv(home),
    timeoutMs: 30_000,
  });
  return out.stdout.trim();
}
