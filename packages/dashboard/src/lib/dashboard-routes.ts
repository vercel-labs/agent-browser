/**
 * Centralized route building for dashboard API calls.
 * All routes stay on the current dashboard origin so the UI also works
 * behind forwarded or reverse-proxied URLs.
 */

/** Build a dashboard API path such as "/api/sessions". */
export function getDashboardApiPath(path: string): string {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  assertDashboardApiPath(normalizedPath);
  return normalizedPath;
}

/** Build the same-origin per-session tabs endpoint proxied through the dashboard. */
export function getSessionTabsPath(port: number): string {
  assertValidPort(port);
  return `/api/session/${port}/tabs`;
}

/** Build the same-origin WebSocket URL for a session stream. */
export function getSessionStreamUrl(port: number): string {
  assertValidPort(port);
  const streamPath = `/api/session/${port}/stream`;
  if (typeof window === "undefined") {
    return streamPath;
  }

  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}${streamPath}`;
}

function assertDashboardApiPath(path: string): asserts path is string {
  if (!path.startsWith("/api/")) {
    throw new Error(`Assertion failed: Expected dashboard API path, got: ${path}`);
  }
}

function assertValidPort(port: number): asserts port is number {
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Assertion failed: Invalid session port: ${port}`);
  }
}
