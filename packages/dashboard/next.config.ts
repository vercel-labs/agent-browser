import type { NextConfig } from "next";

function normalizeBasePath(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const withLeadingSlash = trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
  const withoutTrailingSlash = withLeadingSlash.replace(/\/+$/, "");
  return withoutTrailingSlash || undefined;
}

const basePath = normalizeBasePath(process.env.AGENT_BROWSER_DASHBOARD_BASE_PATH);

const config: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  devIndicators: false,
  ...(basePath
    ? {
        basePath,
        assetPrefix: basePath,
      }
    : {}),
};

export default config;
