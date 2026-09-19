// SPEC 6.1/6.2: cloning never executes anything. A hostile global git config points
// core.hooksPath at a post-checkout hook that writes a marker file. A naive clone runs it (the
// control); our clone must not, both with our isolated environment and with the -c flags alone.

import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { cloneArgs, gitEnv, SAFE_CONFIG } from "../src/git.js";
import { runCommand } from "../src/run.js";

let dir: string;
let remote: string;
let marker: string;
let hostileConfig: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "flyover-git-"));
  marker = join(dir, "HOOK_RAN");

  // A hook directory whose post-checkout writes the marker.
  const hooks = join(dir, "hooks");
  mkdirSync(hooks);
  writeFileSync(join(hooks, "post-checkout"), `#!/bin/sh\necho ran > "${marker}"\n`);
  chmodSync(join(hooks, "post-checkout"), 0o755);
  hostileConfig = join(dir, "hostile.gitconfig");
  writeFileSync(hostileConfig, `[core]\n\thooksPath = ${hooks}\n`);

  // A small repo to clone from, over file:// (tests only; production refuses file://).
  const work = join(dir, "work");
  mkdirSync(work);
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: work,
      env: {
        ...gitEnv(dir),
        GIT_AUTHOR_NAME: "t",
        GIT_AUTHOR_EMAIL: "t@t",
        GIT_COMMITTER_NAME: "t",
        GIT_COMMITTER_EMAIL: "t@t",
      },
    });
  git("init", "-q", "-b", "main");
  writeFileSync(join(work, "a.txt"), "hello\n");
  git("add", ".");
  git("commit", "-q", "-m", "init");
  remote = `file://${work}`;
});

afterEach(() => rmSync(dir, { recursive: true, force: true }));

const fileProtocolForTest = ["-c", "protocol.file.allow=always", "-c", "protocol.allow=always"];

describe("clone never runs hooks", () => {
  it("control: a naive clone under the hostile config does run the hook", async () => {
    await runCommand("git", ["clone", "--quiet", remote, join(dir, "naive")], {
      env: {
        PATH: process.env.PATH,
        HOME: dir,
        GIT_CONFIG_GLOBAL: hostileConfig,
        GIT_CONFIG_NOSYSTEM: "1",
      },
      timeoutMs: 30_000,
    });
    expect(existsSync(marker)).toBe(true);
  });

  it("our clone with our isolated environment does not run it", async () => {
    await runCommand("git", cloneArgs({ url: remote }, join(dir, "ours"), fileProtocolForTest), {
      env: gitEnv(dir),
      timeoutMs: 30_000,
    });
    expect(existsSync(join(dir, "ours", "a.txt"))).toBe(true);
    expect(existsSync(marker)).toBe(false);
  });

  it("our -c flags alone still block the hook, even if the hostile config is loaded", async () => {
    await runCommand("git", cloneArgs({ url: remote }, join(dir, "flags"), fileProtocolForTest), {
      env: { ...gitEnv(dir), GIT_CONFIG_GLOBAL: hostileConfig },
      timeoutMs: 30_000,
    });
    expect(existsSync(join(dir, "flags", "a.txt"))).toBe(true);
    expect(existsSync(marker)).toBe(false);
  });
});

describe("clone arguments", () => {
  it("carry every SPEC 6.2 protection and pin the vetted address", () => {
    const args = cloneArgs(
      { url: "https://github.com/a/b.git", host: "github.com", address: "140.82.112.3" },
      "/tmp/x",
    );
    const flags = args.join(" ");
    for (const required of [
      "core.hooksPath=/dev/null",
      "protocol.file.allow=never",
      "protocol.allow=never",
      "protocol.https.allow=always",
      "http.followRedirects=false",
      "http.curloptResolve=github.com:443:140.82.112.3",
      "--depth 1",
      "--no-recurse-submodules",
    ]) {
      expect(flags).toContain(required);
    }
    expect(args.slice(-3)).toEqual(["--", "https://github.com/a/b.git", "/tmp/x"]);
    expect(SAFE_CONFIG.length).toBeGreaterThan(0);
  });

  it("run git with prompts off, LFS smudge skipped, and no system or user config", () => {
    const env = gitEnv("/tmp/home");
    expect(env).toMatchObject({
      GIT_TERMINAL_PROMPT: "0",
      GIT_LFS_SKIP_SMUDGE: "1",
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: "/dev/null",
    });
    expect(env.DATABASE_URL).toBeUndefined();
  });
});
