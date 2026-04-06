import type { NextConfig } from "next";

const DAEMON_ORIGIN = process.env.DAEMON_URL || "http://localhost:4848";

const config: NextConfig = {
  output: "export",
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
