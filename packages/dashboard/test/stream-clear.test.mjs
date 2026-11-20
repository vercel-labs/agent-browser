import "./register-aliases.mjs";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { atom, createStore } from "jotai/vanilla";

test("stream events respect clears, replacement and the retained-row limit", async () => {
  const store = createStore();
  const effects = [];
  mock.module("jotai", { namedExports: { atom } });
  mock.module("react", { namedExports: {
    useCallback: (callback) => callback,
    useEffect: (effect) => effects.push(effect),
    useRef: (current) => ({ current }),
  } });
  mock.module("jotai/react", { namedExports: {
    useSetAtom: (target) => (...args) => store.set(target, ...args),
  } });
  mock.module("@/store/tabs", { namedExports: {
    tabCacheAtom: atom({}), engineCacheAtom: atom({}),
  } });
  mock.module("@/lib/dashboard-routes", { namedExports: {
    getSessionStreamUrl: (port) => `ws://localhost:${port}`,
  } });
  const originalWebSocket = globalThis.WebSocket;
  const sockets = [];
  globalThis.WebSocket = class {
    static OPEN = 1;
    readyState = 1;
    constructor() { sockets.push(this); }
    close() {}
  };
  const stream = await import("../src/store/stream.ts");
  stream.useStreamSync(9222);
  const cleanups = effects.map((effect) => effect());
  const send = (message) => sockets[0].onmessage({ data: JSON.stringify(message) });
  try {
    send({ type: "command", id: "old", action: "snapshot" });
    store.set(stream.clearEventsAtom);
    send({ type: "command", id: "new", action: "title" });
    assert.deepEqual(store.get(stream.streamEventsAtom).map((event) => event.id), ["new"]);

    send({ type: "console", level: "log", text: "old" });
    store.set(stream.clearConsoleLogsAtom);
    send({ type: "page_error", message: "new" });
    assert.equal(store.get(stream.consoleLogsAtom).length, 1);
    assert.equal(store.get(stream.consoleLogsAtom)[0].type, "page_error");

    send({ type: "result", id: "new", success: true });
    assert.deepEqual(store.get(stream.streamEventsAtom).map((event) => event.type), ["result"]);

    for (let i = 0; i < 501; i++) send({ type: "command", id: String(i), action: "title" });
    assert.equal(store.get(stream.streamEventsAtom).length, 500);
    assert.equal(store.get(stream.streamEventsAtom)[0].id, "1");
  } finally {
    for (const cleanup of cleanups) cleanup?.();
    globalThis.WebSocket = originalWebSocket;
    mock.restoreAll();
  }
});
