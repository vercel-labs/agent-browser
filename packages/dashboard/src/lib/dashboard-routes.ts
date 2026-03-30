/**
 * Centralized route building for dashboard API calls.
 * All routes are relative to the current origin so the dashboard
 * works through any reverse-proxy or forwarded URL.
 */

/** Build a dashboard API path (e.g., "/api/sessions"). */
export function getDashboardApiPath(path: string): string {
  return path.startsWith("/") ? path : `/${path}`;
}

/** Build the per-session tabs endpoint proxied through the dashboard. */
export function getSessionTabsPath(port: number): string {
  assertValidPort(port);
  return `/api/session/${port}/tabs`;
}

/** Build the per-session status endpoint proxied through the dashboard. */
export function getSessionStatusPath(port: number): string {
  assertValidPort(port);
  return `/api/session/${port}/status`;
}

/** Build the same-origin WebSocket URL for the session stream. */
export function getSessionStreamUrl(port: number): string {
  assertValidPort(port);
  if (typeof window === "undefined") {
    return `ws://localhost:${port}`;
  }

  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/api/session/${port}/stream`;
}

function assertValidPort(port: number): asserts port is number {
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Assertion failed: Invalid session port: ${port}`);
  }
}
