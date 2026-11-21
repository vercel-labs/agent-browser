import type { NextConfig } from "next";

const DAEMON_ORIGIN = process.env.DAEMON_URL || "http://localhost:4848";

const config: NextConfig = {
  output: "export",
  // Relative asset URLs let a reverse proxy mount the dashboard under a path prefix.
  assetPrefix: ".",
  images: { unoptimized: true },
  devIndicators: false,
  env: {
    NEXT_PUBLIC_DAEMON_URL: DAEMON_ORIGIN,
  },
  async rewrites() {
    return [
      {
        source: "/api/:path*",
        destination: `${DAEMON_ORIGIN}/api/:path*`,
      },
    ];
  },
};

export default config;
