import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

function normalizeBasePath(value) {
  if (!value) return undefined;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const withLeadingSlash = trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
  const withoutTrailingSlash = withLeadingSlash.replace(/\/+$/, "");
  return withoutTrailingSlash || undefined;
}

const scriptDir = dirname(fileURLToPath(import.meta.url));
const dashboardDir = resolve(scriptDir, "..");
const tokenPath = resolve(dashboardDir, "base-path-token.txt");
const nextCli = resolve(dashboardDir, "node_modules", "next", "dist", "bin", "next");

const placeholderBasePath = readFileSync(tokenPath, "utf8").trim();
const configuredBasePath =
  normalizeBasePath(process.env.AGENT_BROWSER_DASHBOARD_BASE_PATH) ??
  placeholderBasePath;

const child = spawn(process.execPath, [nextCli, "build"], {
  cwd: dashboardDir,
  env: {
    ...process.env,
    AGENT_BROWSER_DASHBOARD_BASE_PATH: configuredBasePath,
  },
  stdio: "inherit",
});

child.on("exit", (code) => {
  process.exit(code ?? 1);
});

child.on("error", (error) => {
  console.error("Failed to start dashboard build:", error);
  process.exit(1);
});
