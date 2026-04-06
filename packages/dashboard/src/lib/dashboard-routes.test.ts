import assert from "node:assert/strict";
import test from "node:test";
import {
  getDashboardApiPath,
  getSessionStreamUrl,
  joinDashboardBasePath,
  resolveDashboardBasePath,
} from "./dashboard-routes.ts";

type WindowSnapshot = typeof globalThis & { window?: Window };

function withMockWindow(url: string, fn: () => void) {
  const globals = globalThis as WindowSnapshot;
  const previousWindow = globals.window;
  globals.window = { location: new URL(url) } as Window;
  try {
    fn();
  } finally {
    if (previousWindow === undefined) {
      delete globals.window;
    } else {
      globals.window = previousWindow;
    }
  }
}

test("resolveDashboardBasePath supports root and subpath mounts", () => {
  assert.equal(resolveDashboardBasePath("/"), "");
  assert.equal(resolveDashboardBasePath("/index.html"), "");
  assert.equal(resolveDashboardBasePath("/agent-browser"), "/agent-browser");
  assert.equal(resolveDashboardBasePath("/agent-browser/"), "/agent-browser");
  assert.equal(resolveDashboardBasePath("/agent-browser/index.html"), "/agent-browser");
  assert.equal(resolveDashboardBasePath("/nested/agent-browser/index.html"), "/nested/agent-browser");
});

test("joinDashboardBasePath preserves root-path behavior", () => {
  assert.equal(joinDashboardBasePath("", "/api/sessions"), "/api/sessions");
  assert.equal(joinDashboardBasePath("/agent-browser", "/api/sessions"), "/agent-browser/api/sessions");
  assert.equal(joinDashboardBasePath("/agent-browser/", "api/sessions"), "/agent-browser/api/sessions");
});

test("getDashboardApiPath uses the current dashboard mount path", () => {
  withMockWindow("https://example.com/agent-browser/?port=9222", () => {
    assert.equal(getDashboardApiPath("/api/sessions"), "/agent-browser/api/sessions");
  });

  withMockWindow("https://example.com/agent-browser/index.html?port=9222", () => {
    assert.equal(getDashboardApiPath("/api/sessions"), "/agent-browser/api/sessions");
  });

  withMockWindow("https://example.com/?port=9222", () => {
    assert.equal(getDashboardApiPath("/api/sessions"), "/api/sessions");
  });
});

test("getSessionStreamUrl keeps websocket traffic on same-origin plus base path", () => {
  withMockWindow("https://example.com/agent-browser/?port=9222", () => {
    assert.equal(
      getSessionStreamUrl(9222),
      "wss://example.com/agent-browser/api/session/9222/stream",
    );
  });

  withMockWindow("http://localhost:4848/?port=9222", () => {
    assert.equal(
      getSessionStreamUrl(9222),
      "ws://localhost:4848/api/session/9222/stream",
    );
  });
});
