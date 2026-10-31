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

const ACCESS_TOKEN_FRAGMENT_KEY = "dashboard-access-token";
const ACCESS_TOKEN_COOKIE = "__Host-agent-browser-dashboard-token";
const LEGACY_ACCESS_TOKEN_COOKIE = "agent-browser-dashboard-token";

/**
 * Reads the access token from a dashboard URL fragment and removes it from the
 * address bar before application requests begin. Fragments never reach proxies
 * or servers, avoiding token leakage through HTTP request logs.
 */
function getDashboardAccessToken(): string | null {
  if (typeof window === "undefined") return null;
  const params = new URLSearchParams(window.location.hash.slice(1));
  const token = params.get(ACCESS_TOKEN_FRAGMENT_KEY);
  if (!token) return null;

  params.delete(ACCESS_TOKEN_FRAGMENT_KEY);
  const fragment = params.toString();
  window.history.replaceState(
    null,
    "",
    `${window.location.pathname}${window.location.search}${fragment ? `#${fragment}` : ""}`,
  );
  return token;
}

let dashboardAccessToken: string | null | undefined;

/**
 * Persist external-dashboard tokens only in a Secure, host-bound cookie so
 * HTTP and WebSocket requests authenticate without exposing the token in URLs.
 * Loopback HTTP requests do not require a token.
 */
export function initializeDashboardAccessToken(): void {
  dashboardAccessToken ??= getDashboardAccessToken();
  if (typeof window === "undefined") return;

  // Remove cookies written by older dashboard builds. The legacy cookie was
  // readable by every localhost port when created over plain HTTP.
  document.cookie = `${LEGACY_ACCESS_TOKEN_COOKIE}=; Path=/; Max-Age=0; SameSite=Strict`;

  if (!dashboardAccessToken || window.location.protocol !== "https:") return;
  document.cookie = `${ACCESS_TOKEN_COOKIE}=${encodeURIComponent(dashboardAccessToken)}; Path=/; SameSite=Strict; Secure`;
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
