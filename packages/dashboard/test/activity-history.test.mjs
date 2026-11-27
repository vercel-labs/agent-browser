import "./register-aliases.mjs";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { atom, createStore } from "jotai/vanilla";

test("activity is restored for a new daemon port without cross-session writes", async () => {
  const store = createStore();
  const session = atom("alpha");
  const port = atom(9222);
  const stream = atom([]);
  const setters = new Map();
  const values = new Map([
    ["ab-activity-alpha", JSON.stringify([{ type: "result", id: "history" }])],
    ["ab-activity-beta", JSON.stringify([{ type: "result", id: "beta" }])],
  ]);
  const originalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  } });
  const dependencies = [];
  let index = 0;
  let pending = [];
  mock.module("jotai", { namedExports: { atom } });
  mock.module("react", { namedExports: {
    useEffect: (effect, deps) => {
      const position = index++;
      const previous = dependencies[position];
      if (!previous || deps.some((value, i) => !Object.is(value, previous[i]))) {
        pending.push(effect);
        dependencies[position] = deps;
      }
    },
  } });
  mock.module("jotai/react", { namedExports: {
    useAtomValue: (target) => store.get(target),
    useSetAtom: (target) => {
      if (!setters.has(target)) setters.set(target, (...args) => store.set(target, ...args));
      return setters.get(target);
    },
  } });
  mock.module("@/store/sessions", { namedExports: {
    activeSessionNameAtom: session, activePortAtom: port,
  } });
  mock.module("@/store/stream", { namedExports: { streamEventsAtom: stream } });
  const activity = await import("../src/store/activity.ts");
  const render = () => {
    index = 0;
    pending = [];
    activity.useActivitySync();
    for (const effect of pending) effect();
  };
  try {
    store.set(activity.persistActivityAtom, true);
    render();
    render();
    store.set(stream, [{ type: "result", id: "live" }]);
    render();
    assert.deepEqual(JSON.parse(values.get("ab-activity-alpha")).map((event) => event.id), ["history", "live"]);

    store.set(port, 9333);
    store.set(stream, []);
    render();
    render();
    assert.deepEqual(store.get(activity.combinedEventsAtom).map((event) => event.id), ["history", "live"]);

    store.set(session, "beta");
    store.set(port, 9444);
    render();
    assert.deepEqual(JSON.parse(values.get("ab-activity-beta")).map((event) => event.id), ["beta"]);
    render();
    assert.deepEqual(store.get(activity.combinedEventsAtom).map((event) => event.id), ["beta"]);
  } finally {
    if (originalStorage) Object.defineProperty(globalThis, "localStorage", originalStorage);
    else delete globalThis.localStorage;
    mock.restoreAll();
  }
});
