// SPEC 6.3 SSRF: only https on allowlisted hosts, and never a private, loopback, link-local, or
// metadata address. The resolver is injected, so these tests never touch the network.

import { describe, expect, it } from "vitest";

import { isForbiddenAddress, SourceError, validateSource, type Resolver } from "../src/source.js";

const ALLOW = ["github.com", "gitlab.com"];
const resolvesTo =
  (...ips: string[]): Resolver =>
  async () =>
    ips;

describe("validateSource", () => {
  it("accepts an allowlisted https host with a public address and pins it", async () => {
    const src = await validateSource(
      "https://github.com/wpf002/flyover.git",
      ALLOW,
      resolvesTo("140.82.112.3"),
    );
    expect(src).toEqual({
      url: "https://github.com/wpf002/flyover.git",
      host: "github.com",
      address: "140.82.112.3",
    });
  });

  it.each([
    ["http://github.com/a/b.git", /https/],
    ["git://github.com/a/b.git", /https/],
    ["ssh://git@github.com/a/b.git", /https/],
    ["file:///etc/passwd", /https/],
    ["https://evil.example.com/a/b.git", /GIT_HOST_ALLOWLIST/],
    ["https://github.com.evil.com/a.git", /GIT_HOST_ALLOWLIST/],
    ["https://user:pass@github.com/a/b.git", /credentials/],
    ["https://github.com:8443/a/b.git", /port/],
    ["https://github.com/a/b.git?x=1", /query/],
    ["https://127.0.0.1/a.git", /IP-literal/],
    ["https://[::1]/a.git", /IP-literal/],
    ["not a url", /valid URL/],
  ])("refuses %s", async (url, why) => {
    await expect(validateSource(url, ALLOW, resolvesTo("140.82.112.3"))).rejects.toThrow(why);
  });

  it.each([
    "127.0.0.1",
    "10.1.2.3",
    "172.16.0.9",
    "192.168.1.1",
    "169.254.169.254",
    "100.64.0.1",
    "0.0.0.0",
    "::1",
    "fe80::1",
    "fd00:ec2::254",
    "::ffff:127.0.0.1",
    "::ffff:169.254.169.254",
    "64:ff9b::a9fe:a9fe",
  ])("refuses an allowlisted host that resolves to %s (DNS rebinding / metadata)", async (ip) => {
    await expect(
      validateSource("https://github.com/a/b.git", ALLOW, resolvesTo("140.82.112.3", ip)),
    ).rejects.toThrow(SourceError);
  });

  it("refuses a host that does not resolve", async () => {
    const fails: Resolver = async () => {
      throw new Error("ENOTFOUND");
    };
    await expect(validateSource("https://github.com/a/b.git", ALLOW, fails)).rejects.toThrow(
      /resolve/,
    );
  });
});

describe("isForbiddenAddress", () => {
  it.each(["140.82.112.3", "8.8.8.8", "2606:50c0:8000::153", "2a00:1450:4001::200e"])(
    "allows public %s",
    (ip) => {
      expect(isForbiddenAddress(ip)).toBe(false);
    },
  );
});
