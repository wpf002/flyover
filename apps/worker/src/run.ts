// Child processes with a hard timeout, an abort signal, an optional watchdog tick, and a clean
// environment (callers pass exactly the variables a child may see; nothing is inherited).

import { spawn } from "node:child_process";
import { lstat, readdir } from "node:fs/promises";
import { join } from "node:path";

export class CommandError extends Error {
  override name = "CommandError";
}

export interface RunOptions {
  cwd?: string;
  env: NodeJS.ProcessEnv;
  timeoutMs: number;
  signal?: AbortSignal;
  /** Called every `tickMs` while running; return a message to kill the child with that error. */
  onTick?: () => Promise<string | null>;
  tickMs?: number;
}

const MAX_OUTPUT = 1024 * 1024;

export function runCommand(
  cmd: string,
  args: readonly string[],
  opts: RunOptions,
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      cwd: opts.cwd,
      env: opts.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let failure: string | null = null;
    const append = (acc: string, chunk: Buffer) =>
      acc.length < MAX_OUTPUT ? acc + chunk.toString("utf8") : acc;
    child.stdout.on("data", (c: Buffer) => (stdout = append(stdout, c)));
    child.stderr.on("data", (c: Buffer) => (stderr = append(stderr, c)));

    const kill = (why: string) => {
      if (failure === null) failure = why;
      child.kill("SIGKILL");
    };
    const timer = setTimeout(
      () => kill(`timed out after ${Math.round(opts.timeoutMs / 1000)}s`),
      opts.timeoutMs,
    );
    const onAbort = () => kill("aborted");
    opts.signal?.addEventListener("abort", onAbort, { once: true });
    let ticking = false;
    const ticker = opts.onTick
      ? setInterval(() => {
          if (ticking) return;
          ticking = true;
          opts.onTick!()
            .then((msg) => {
              if (msg) kill(msg);
            })
            .finally(() => (ticking = false));
        }, opts.tickMs ?? 1000)
      : null;

    const done = () => {
      clearTimeout(timer);
      if (ticker) clearInterval(ticker);
      opts.signal?.removeEventListener("abort", onAbort);
    };
    child.on("error", (err) => {
      done();
      reject(new CommandError(`${cmd}: ${err.message}`));
    });
    child.on("close", (code) => {
      done();
      if (failure !== null) {
        reject(new CommandError(`${cmd} ${failure}`));
      } else if (code !== 0) {
        const detail = stderr.trim().split("\n").slice(-5).join("\n");
        reject(new CommandError(`${cmd} exited with ${code}${detail ? `: ${detail}` : ""}`));
      } else {
        resolve({ stdout, stderr });
      }
    });
  });
}

/** Total size in bytes of regular files under `dir`. Symlinks are not followed. */
export async function dirSize(dir: string): Promise<number> {
  let total = 0;
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return 0;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) total += await dirSize(path);
    else if (entry.isFile()) total += (await lstat(path)).size;
  }
  return total;
}
