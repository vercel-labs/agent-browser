/**
 * Centralized route building for dashboard API calls.
 * All routes are relative to the current origin so the dashboard
 * works through any reverse-proxy or forwarded URL.
 */

const INDEX_HTML_SUFFIX = "/index.html";

/** Resolve the dashboard mount path from the current browser pathname. */
export function resolveDashboardBasePath(pathname: string): string {
  if (!pathname || pathname === "/") {
    return "";
  }

  let normalized = pathname.startsWith("/") ? pathname : `/${pathname}`;
  if (normalized.endsWith(INDEX_HTML_SUFFIX)) {
    normalized = normalized.slice(0, -INDEX_HTML_SUFFIX.length) || "/";
  }

  const lastSlash = normalized.lastIndexOf("/");
  const lastSegment = lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
  if (!normalized.endsWith("/") && lastSegment.includes(".")) {
    normalized = normalized.slice(0, lastSlash) || "/";
  }

  normalized = normalized.replace(/\/+$/, "");
  return normalized === "/" ? "" : normalized;
}

/** Join a normalized dashboard path onto a dashboard base path. */
export function joinDashboardBasePath(basePath: string, path: string): string {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  const normalizedBase = basePath === "/" ? "" : basePath.replace(/\/+$/, "");
  return normalizedBase ? `${normalizedBase}${normalizedPath}` : normalizedPath;
}

function getDashboardBasePath(): string {
  if (typeof window === "undefined") {
    return "";
  }
  return resolveDashboardBasePath(window.location.pathname);
}

/** Build a dashboard API path (e.g., "/api/sessions"). */
export function getDashboardApiPath(path: string): string {
  return joinDashboardBasePath(getDashboardBasePath(), path);
}

/** Build a dashboard public asset path (e.g., "/providers/browserbase.svg"). */
export function getDashboardAssetPath(path: string): string {
  return joinDashboardBasePath(getDashboardBasePath(), path);
}

/** Build the per-session tabs endpoint proxied through the dashboard. */
export function getSessionTabsPath(port: number): string {
  assertValidPort(port);
  return getDashboardApiPath(`/api/session/${port}/tabs`);
}

/** Build the per-session status endpoint proxied through the dashboard. */
export function getSessionStatusPath(port: number): string {
  assertValidPort(port);
  return getDashboardApiPath(`/api/session/${port}/status`);
}

/** Build the same-origin WebSocket URL for the session stream. */
export function getSessionStreamUrl(port: number): string {
  assertValidPort(port);
  if (typeof window === "undefined") {
    return `ws://localhost:${port}`;
  }

  const url = new URL(
    getDashboardApiPath(`/api/session/${port}/stream`),
    window.location.origin,
  );
  url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

function assertValidPort(port: number): asserts port is number {
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Assertion failed: Invalid session port: ${port}`);
  }
}
