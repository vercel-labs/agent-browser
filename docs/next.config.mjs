import { createGeistdocs } from "@vercel/geistdocs/next";

const withMDX = createGeistdocs();

const nextConfig = {
  pageExtensions: ["js", "jsx", "ts", "tsx", "md", "mdx"],
  skipProxyUrlNormalize: true,
  serverExternalPackages: ["just-bash", "bash-tool"],
  outputFileTracingIncludes: {
    "/*": ["./content/docs/**/*.mdx"],
  },
  async headers() {
    return process.env.VERCEL_ENV && process.env.VERCEL_ENV !== "production"
      ? [
          {
            source: "/:path*",
            headers: [{ key: "X-Robots-Tag", value: "noindex, nofollow" }],
          },
        ]
      : [];
  },
  async rewrites() {
    return {
      beforeFiles: [
        { source: "/index.md", destination: "/api/docs-md" },
        { source: "/:path*.md", destination: "/api/docs-md/:path*" },
      ],
    };
  },
};

export default withMDX(nextConfig);
