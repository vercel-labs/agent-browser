import "./register-aliases.mjs";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { atom, createStore } from "jotai/vanilla";

test("session polling waits for completion and ignores responses after cleanup", async () => {
  const store = createStore();
  const tabCacheAtom = atom({ 9222: [{ tabId: "stale" }] });
  const effects = [];
  const requests = [];
  const flush = () => new Promise(setImmediate);
  mock.module("jotai", { namedExports: { atom } });
  mock.module("react", { namedExports: {
    useCallback: (callback) => callback,
    useEffect: (effect) => effects.push(effect),
    useRef: (current) => ({ current }),
  } });
  mock.module("jotai/utils", { namedExports: {
    useAtomCallback: (callback) => (...args) => callback(store.get, store.set, ...args),
  } });
  mock.module("@/store/chat", { namedExports: { disposeChat() {} } });
  mock.module("@/store/tabs", { namedExports: { tabCacheAtom, engineCacheAtom: atom({}) } });
  mock.module("@/store/stream", { namedExports: { streamTabsAtom: atom([]), streamEngineAtom: atom("") } });
  mock.module("@/lib/dashboard-routes", { namedExports: {
    getDashboardApiPath: (path) => path,
    getSessionTabsPath: (port) => `/tabs/${port}`,
  } });
  mock.method(globalThis, "fetch", (url, init = {}) => new Promise((resolve) => {
    requests.push({ url, signal: init.signal, resolve });
  }));
  mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const sessions = await import("../src/store/sessions.ts");
  sessions.useSessionsSync(10);
  let cleanup = effects[0]();
  try {
    assert.equal(requests.length, 1);
    mock.timers.tick(100);
    assert.equal(requests.length, 1, "an unresolved session request must not overlap another poll");
    requests[0].resolve(Response.json([{ session: "alpha", port: 9222, engine: "chrome" }]));
    await flush();
    assert.equal(requests.length, 2);
    mock.timers.tick(100);
    assert.equal(requests.length, 2, "an unresolved tab request must finish before polling again");
    requests[1].resolve(Response.json([]));
    await flush();
    assert.deepEqual(store.get(tabCacheAtom)[9222], [], "empty tab responses clear stale cached tabs");
    mock.timers.tick(9);
    assert.equal(requests.length, 2);
    mock.timers.tick(1);
    assert.equal(requests.length, 3);
    cleanup();
    assert.equal(requests[2].signal.aborted, true);
    requests[2].resolve(Response.json([{ session: "late", port: 9333, engine: "chrome" }]));
    await flush();
    assert.deepEqual(store.get(sessions.polledSessionsAtom).map((session) => session.session), ["alpha"]);
    mock.timers.tick(100);
    assert.equal(requests.length, 3);

    // A response body can finish after abort even when fetch itself already completed.
    cleanup = effects[0]();
    requests[3].resolve(Response.json([{ session: "alpha", port: 9222, engine: "chrome" }]));
    await flush();
    let resolveTabs;
    requests[4].resolve({ ok: true, json: () => new Promise((resolve) => { resolveTabs = resolve; }) });
    await flush();
    cleanup();
    assert.equal(requests[4].signal.aborted, true);
    resolveTabs([{ tabId: "late" }]);
    await flush();
    assert.deepEqual(store.get(tabCacheAtom)[9222], [], "late tab JSON cannot restore stale cache state");
    mock.timers.tick(100);
    assert.equal(requests.length, 5);
  } finally {
    cleanup();
    mock.timers.reset();
    mock.restoreAll();
  }
});
