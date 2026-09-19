import type { NextConfig } from "next";

const config: NextConfig = {
  reactStrictMode: true,
  // Workspace packages ship compiled ESM, so no transpilePackages needed.
  // M5 adds the headers the wasm renderer needs (COOP/COEP for threads).
};

export default config;
