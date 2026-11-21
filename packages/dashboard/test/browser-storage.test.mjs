import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { readBrowserStorage, writeBrowserStorage, removeBrowserStorage } from "../src/lib/browser-storage.ts";

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
afterEach(() => {
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else delete globalThis.window;
});

test("storage access denied by the browser does not escape", () => {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { get localStorage() { throw new Error("Access denied"); } },
  });
  assert.equal(readBrowserStorage("state"), null);
  assert.doesNotThrow(() => writeBrowserStorage("state", "true"));
  assert.doesNotThrow(() => removeBrowserStorage("state"));
});

test("full or unavailable storage leaves operations usable", () => {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage: {
      getItem() { throw new Error("Unavailable"); },
      setItem() { throw new Error("Quota exceeded"); },
      removeItem() { throw new Error("Unavailable"); },
    } },
  });
  assert.equal(readBrowserStorage("state"), null);
  assert.doesNotThrow(() => writeBrowserStorage("state", "true"));
  assert.doesNotThrow(() => removeBrowserStorage("state"));
  delete globalThis.window;
  assert.equal(readBrowserStorage("state"), null);
});

test("available storage still reads, writes and removes values", () => {
  const values = new Map();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage: {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
      removeItem: (key) => values.delete(key),
    } },
  });
  writeBrowserStorage("state", "true");
  assert.equal(readBrowserStorage("state"), "true");
  removeBrowserStorage("state");
  assert.equal(readBrowserStorage("state"), null);
});
