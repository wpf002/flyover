// SPEC 6.3: accept only https:// URLs on an allowlisted host, resolve the host, and refuse
// private, loopback, link-local, and metadata addresses before cloning. The resolved address is
// returned so the clone can be pinned to it (no second DNS lookup to rebind).

import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

export type Resolver = (host: string) => Promise<string[]>;

export const systemResolver: Resolver = async (host) =>
  (await lookup(host, { all: true, verbatim: true })).map((a) => a.address);

export class SourceError extends Error {
  override name = "SourceError";
}

export interface ValidatedSource {
  url: string;
  host: string;
  /** An address the host resolved to, all of which were checked. Used to pin the clone. */
  address: string;
}

export async function validateSource(
  raw: string,
  allowlist: readonly string[],
  resolve: Resolver = systemResolver,
): Promise<ValidatedSource> {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new SourceError("not a valid URL");
  }
  if (url.protocol !== "https:") throw new SourceError("only https:// sources are allowed");
  if (url.username || url.password) throw new SourceError("credentials in the URL are not allowed");
  if (url.port && url.port !== "443")
    throw new SourceError("only the default https port is allowed");
  if (url.search || url.hash) throw new SourceError("query strings and fragments are not allowed");

  const host = url.hostname.toLowerCase();
  if (isIP(host.replace(/^\[|\]$/g, "")) !== 0) {
    throw new SourceError("IP-literal hosts are not allowed");
  }
  if (!allowlist.includes(host)) {
    throw new SourceError(`host ${host} is not in GIT_HOST_ALLOWLIST`);
  }

  let addresses: string[];
  try {
    addresses = await resolve(host);
  } catch {
    throw new SourceError(`could not resolve ${host}`);
  }
  if (addresses.length === 0) throw new SourceError(`could not resolve ${host}`);
  for (const address of addresses) {
    if (isForbiddenAddress(address)) {
      throw new SourceError(`host ${host} resolves to a disallowed address (${address})`);
    }
  }
  // Prefer IPv4 for the pin; curl's resolve list takes it without brackets.
  const address = addresses.find((a) => isIP(a) === 4) ?? addresses[0]!;
  return { url: url.toString(), host, address };
}

/** True for any address the worker must never connect to. */
export function isForbiddenAddress(address: string): boolean {
  const kind = isIP(address);
  if (kind === 4) return forbiddenV4(address);
  if (kind === 6) return forbiddenV6(address);
  return true; // not an IP at all
}

function v4(address: string): number[] {
  return address.split(".").map((p) => Number(p));
}

function forbiddenV4(address: string): boolean {
  const [a = 0, b = 0, c = 0] = v4(address);
  return (
    a === 0 || // "this" network
    a === 10 || // private
    a === 127 || // loopback
    (a === 100 && b >= 64 && b <= 127) || // carrier-grade NAT
    (a === 169 && b === 254) || // link-local, incl. 169.254.169.254 metadata
    (a === 172 && b >= 16 && b <= 31) || // private
    (a === 192 && b === 0 && c === 0) || // IETF protocol assignments
    (a === 192 && b === 0 && c === 2) || // TEST-NET-1
    (a === 192 && b === 168) || // private
    (a === 198 && (b === 18 || b === 19)) || // benchmarking
    (a === 198 && b === 51 && c === 100) || // TEST-NET-2
    (a === 203 && b === 0 && c === 113) || // TEST-NET-3
    a >= 224 // multicast, reserved, broadcast
  );
}

/** Expand an IPv6 address into eight 16-bit groups. */
function v6Groups(address: string): number[] {
  let addr = address.toLowerCase().split("%")[0]!;
  // Embedded dotted IPv4 tail (e.g. ::ffff:127.0.0.1).
  const tail = addr.match(/(\d+\.\d+\.\d+\.\d+)$/);
  if (tail) {
    const [a = 0, b = 0, c = 0, d = 0] = v4(tail[1]!);
    addr =
      addr.slice(0, -tail[1]!.length) +
      `${((a << 8) | b).toString(16)}:${((c << 8) | d).toString(16)}`;
  }
  const [head = "", rest] = addr.split("::");
  const left = head ? head.split(":") : [];
  const right = rest !== undefined && rest !== "" ? rest.split(":") : [];
  const fill = rest !== undefined ? 8 - left.length - right.length : 0;
  return [...left, ...Array(fill).fill("0"), ...right].map((g) => parseInt(g || "0", 16));
}

function forbiddenV6(address: string): boolean {
  const g = v6Groups(address);
  const [g0 = 0, g1 = 0, , , , g5 = 0, g6 = 0, g7 = 0] = g;
  const allZeroUpTo = (n: number) => g.slice(0, n).every((x) => x === 0);

  if (allZeroUpTo(8)) return true; // ::
  if (allZeroUpTo(7) && g7 === 1) return true; // ::1 loopback
  if (allZeroUpTo(5) && g5 === 0xffff) {
    // ::ffff:a.b.c.d, IPv4-mapped: judge the embedded IPv4 address.
    return forbiddenV4(`${g6 >> 8}.${g6 & 0xff}.${g7 >> 8}.${g7 & 0xff}`);
  }
  if (g0 === 0x64 && g1 === 0xff9b) {
    // 64:ff9b::/96 NAT64 maps IPv4; judge the embedded address.
    return forbiddenV4(`${g6 >> 8}.${g6 & 0xff}.${g7 >> 8}.${g7 & 0xff}`);
  }
  if ((g0 & 0xfe00) === 0xfc00) return true; // fc00::/7 unique local (incl. AWS fd00:ec2::254)
  if ((g0 & 0xffc0) === 0xfe80) return true; // fe80::/10 link-local
  if ((g0 & 0xff00) === 0xff00) return true; // ff00::/8 multicast
  if (g0 === 0x2001 && g1 === 0xdb8) return true; // documentation
  return false;
}
