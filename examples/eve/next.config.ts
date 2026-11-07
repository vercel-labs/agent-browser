import type { NextConfig } from "next";
import { withEve } from "eve/next";

const nextConfig: NextConfig = {
  turbopack: {
    root: __dirname,
  },
};

export default withEve(nextConfig);
