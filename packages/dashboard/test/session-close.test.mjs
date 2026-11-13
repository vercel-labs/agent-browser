import "./register-aliases.mjs";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { atom, createStore } from "jotai/vanilla";

test("failed closes and kills preserve chat and allow retry, including bulk closes", async () => {
  const disposed = [];
  let execute = async () => ({ success: false });
  let kill = async () => ({ success: false });
  mock.module("@/lib/exec", { namedExports: {
    execCommand: (...args) => execute(...args),
    killSession: (...args) => kill(...args),
    sessionArgs: (session, ...args) => ["--session", session, ...args],
  } });
  mock.module("@/store/chat", { namedExports: { disposeChat: (session) => disposed.push(session) } });
  mock.module("@/store/tabs", { namedExports: { tabCacheAtom: atom({}), engineCacheAtom: atom({}) } });
  mock.module("@/store/stream", { namedExports: { streamTabsAtom: atom([]), streamEngineAtom: atom("") } });
  const sessions = await import("../src/store/sessions.ts");
  const store = createStore();
  store.set(sessions.polledSessionsAtom, [
    { session: "alpha", port: 9222, engine: "chrome" },
    { session: "beta", port: 9333, engine: "chrome" },
  ]);
  try {
    let resolve;
    execute = () => new Promise((done) => { resolve = done; });
    const closing = store.set(sessions.closeSessionAtom, 9222);
    assert.equal(store.get(sessions.closingSessionsAtom).has("alpha"), true);
    assert.deepEqual(disposed, []);
    resolve({ success: false });
    await closing;
    assert.equal(store.get(sessions.closingSessionsAtom).has("alpha"), false);
    assert.deepEqual(disposed, []);

    execute = async () => ({ success: true });
    await store.set(sessions.closeSessionAtom, 9222);
    assert.deepEqual(disposed, ["alpha"]);
    store.set(sessions.closingSessionsAtom, new Set());

    await store.set(sessions.killSessionAtom, 9333);
    assert.equal(store.get(sessions.closingSessionsAtom).has("beta"), false);
    assert.deepEqual(disposed, ["alpha"]);
    kill = async () => ({ success: true });
    await store.set(sessions.killSessionAtom, 9333);
    assert.deepEqual(disposed, ["alpha", "beta"]);

    store.set(sessions.closingSessionsAtom, new Set());
    disposed.length = 0;
    execute = async (args) => ({ success: args[1] === "alpha" });
    await store.set(sessions.closeAllSessionsAtom);
    assert.deepEqual(disposed, ["alpha"]);
    assert.deepEqual([...store.get(sessions.closingSessionsAtom)], ["alpha"]);
  } finally {
    mock.restoreAll();
  }
});
