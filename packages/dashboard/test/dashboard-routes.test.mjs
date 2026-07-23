import { describe, it } from "node:test";
import assert from "node:assert/strict";

// Inline the pure functions under test to avoid build-step coupling.
// These must stay in sync with src/lib/dashboard-routes.ts.

function getDashboardApiPath(path) {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  assertDashboardApiPath(normalizedPath);
  return normalizedPath;
}

function getSessionTabsPath(port) {
  assertValidPort(port);
  return `/api/session/${port}/tabs`;
}

function getSessionStreamUrl(port) {
  assertValidPort(port);
  const streamPath = `/api/session/${port}/stream`;
  if (typeof globalThis.window === "undefined") {
    return streamPath;
  }
  const protocol = globalThis.window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${globalThis.window.location.host}${streamPath}`;
}

function assertDashboardApiPath(path) {
  if (!path.startsWith("/api/")) {
    throw new Error(`Assertion failed: Expected dashboard API path, got: ${path}`);
  }
}

function assertValidPort(port) {
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Assertion failed: Invalid session port: ${port}`);
  }
}

describe("getDashboardApiPath", () => {
  it("normalizes a path with leading slash", () => {
    assert.equal(getDashboardApiPath("/api/sessions"), "/api/sessions");
  });

  it("adds a leading slash when missing", () => {
    assert.equal(getDashboardApiPath("api/sessions"), "/api/sessions");
  });

  it("throws for a non-api path", () => {
    assert.throws(() => getDashboardApiPath("/other"), /Expected dashboard API path/);
  });

  it("throws for an empty path after normalization", () => {
    assert.throws(() => getDashboardApiPath(""), /Expected dashboard API path/);
  });
});

describe("getSessionTabsPath", () => {
  it("builds the tabs path for a valid port", () => {
    assert.equal(getSessionTabsPath(9222), "/api/session/9222/tabs");
  });

  it("throws for port 0", () => {
    assert.throws(() => getSessionTabsPath(0), /Invalid session port/);
  });

  it("throws for negative port", () => {
    assert.throws(() => getSessionTabsPath(-1), /Invalid session port/);
  });

  it("throws for port above 65535", () => {
    assert.throws(() => getSessionTabsPath(70000), /Invalid session port/);
  });

  it("throws for non-integer port", () => {
    assert.throws(() => getSessionTabsPath(9222.5), /Invalid session port/);
  });
});

describe("getSessionStreamUrl", () => {
  it("returns a path-only URL when window is undefined (server-side)", () => {
    assert.equal(getSessionStreamUrl(9222), "/api/session/9222/stream");
  });

  it("throws for invalid port", () => {
    assert.throws(() => getSessionStreamUrl(0), /Invalid session port/);
  });
});