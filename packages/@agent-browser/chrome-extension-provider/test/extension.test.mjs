import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";
import { webcrypto } from "node:crypto";
import ts from "typescript";

// Unit tests exercise the real worker listeners with Chrome API and socket doubles.
// They do not establish compatibility with an actual Chrome debugger.
const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(import.meta.url);
const settle = () => new Promise((resolve) => setImmediate(resolve));

function event() {
  const listeners = [];
  return { addListener: (listener) => listeners.push(listener), emit: (...args) => listeners.map((listener) => listener(...args)) };
}

function worker() {
  const sockets = [];
  const calls = [];
  const intervals = [];
  const stored = {};
  const tabs = new Map([
    [1, { id: 1, windowId: 1, url: "https://one.example/", title: "One" }],
    [2, { id: 2, windowId: 1, url: "about:blank", title: "Blank" }],
    [3, { id: 3, windowId: 1, url: "chrome://settings/", title: "Settings" }],
    [4, { id: 4, windowId: 1, url: "chrome-extension://own/setup.html", title: "Setup" }],
    [5, { id: 5, windowId: 1, url: "https://chromewebstore.google.com/", title: "Store" }],
  ]);
  class Socket {
    static OPEN = 1;
    readyState = 0;
    sent = [];
    events = new Map();
    constructor(url) { this.url = url; sockets.push(this); }
    addEventListener(kind, listener) {
      const listeners = this.events.get(kind) ?? [];
      listeners.push(listener);
      this.events.set(kind, listeners);
    }
    send(raw) { this.sent.push(JSON.parse(raw)); }
    emit(kind, value = {}) { for (const listener of this.events.get(kind) ?? []) listener(value); }
    open() { this.readyState = 1; this.emit("open"); }
    receive(message) { this.emit("message", { data: JSON.stringify({ v: 1, ...message }) }); }
    close() { if (this.readyState === 3) return; this.readyState = 3; this.emit("close"); }
  }
  const chrome = {
    runtime: {
      id: "own", getURL: (path) => `chrome-extension://own/${path}`,
      onMessage: event(), onInstalled: event(),
    },
    action: { onClicked: event(), setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
    tabs: {
      query: async () => [...tabs.values()], get: async (id) => {
        if (!tabs.has(id)) throw new Error("No tab with id");
        return tabs.get(id);
      },
      update: async (id) => tabs.get(id), create: async () => {},
      onRemoved: event(), onUpdated: event(), onReplaced: event(),
    },
    windows: { update: async () => {} },
    storage: { local: { get: async () => stored, set: async (value) => Object.assign(stored, value) } },
    debugger: {
      attach: async (target) => { calls.push(["attach", target]); },
      detach: async (target) => { calls.push(["detach", target]); },
      sendCommand: async (target, method, params) => { calls.push([method, target, params]); return { actual: true }; },
      onEvent: event(), onDetach: event(),
    },
  };
  const context = vm.createContext({
    chrome, WebSocket: Socket, crypto: webcrypto, navigator: { userAgent: "Chrome/147.0.0.0" },
    URL, Error, console, setTimeout: () => 1, clearTimeout: () => {},
    setInterval: (callback, ms) => { intervals.push({ callback, ms }); return intervals.length; },
    clearInterval: () => {},
  });
  const modules = new Map();
  function load(filename) {
    if (modules.has(filename)) return modules.get(filename).exports;
    const module = { exports: {} };
    modules.set(filename, module);
    const source = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
      fileName: filename,
    }).outputText;
    const compiled = vm.runInContext(`(function(require, module, exports) { ${source}\n })`, context);
    compiled((name) => {
      if (name === "wxt/utils/define-background") return { defineBackground: (fn) => fn };
      return name.startsWith(".") ? load(resolve(filename, "..", `${name}.ts`)) : require(name);
    }, module, module.exports);
    return module.exports;
  }
  load(resolve(root, "entrypoints/background.ts")).default();
  const sender = { id: "own", url: "chrome-extension://own/setup.html" };
  function message(value, from = sender) {
    return new Promise((resolve) => {
      if (!chrome.runtime.onMessage.emit(value, from, resolve).some(Boolean)) resolve(undefined);
    });
  }
  async function pair(session = "work") {
    const response = await message({ kind: "setup-pair", pairingCode: `12345:${"A".repeat(43)}` });
    assert.equal(response.ok, true);
    const socket = sockets.at(-1);
    socket.open();
    socket.receive({ kind: "paired", requestId: webcrypto.randomUUID(), scope: { namespace: "test", session } });
    return { id: response.result.sessionId, socket };
  }
  async function grant(connection, tabId = 1) {
    const response = await message({ kind: "setup-grant", sessionId: connection.id, tabId, url: tabs.get(tabId).url });
    assert.equal(response.ok, true, response.error);
    connection.socket.receive({ kind: "granted" });
    return connection;
  }
  function command(connection, method, extra = {}) {
    const reqId = webcrypto.randomUUID();
    connection.socket.receive({ kind: "cdp-command", reqId, tabId: 1, method, params: {}, ...extra });
    return reqId;
  }
  async function attach(connection) { command(connection, "Bridge.attach"); await settle(); }
  return { sockets, calls, chrome, tabs, intervals, message, pair, grant, command, attach, stored };
}

test("only the exact extension setup page can create a socket or list tabs", async () => {
  const w = worker();
  for (const sender of [
    { id: "other", url: "chrome-extension://own/setup.html" },
    { id: "own", url: "https://example.com/" },
    { id: "own", url: "chrome-extension://own/other.html" },
    { id: "own", url: "chrome-extension://own/setup.html#other" },
  ]) {
    assert.equal(await w.message({ kind: "setup-status" }, sender), undefined);
    assert.equal(await w.message({ kind: "setup-pair", pairingCode: `12345:${"A".repeat(43)}` }, sender), undefined);
  }
  assert.equal(w.sockets.length, 0);
  const status = await w.message({ kind: "setup-status" });
  assert.deepEqual(Array.from(status.result.tabs, (tab) => tab.tabId), [1, 2]);
});

test("pairing advertises only identity and never grants or attaches a default page", async () => {
  const w = worker();
  const c = await w.pair();
  assert.equal(c.socket.sent.length, 1);
  assert.equal(c.socket.sent[0].kind, "hello");
  assert.equal(c.socket.sent[0].chromeVersion, "147.0.0.0");
  assert.equal("tabs" in c.socket.sent[0], false);
  assert.equal(w.calls.length, 0);
  assert.deepEqual(Object.keys(w.stored), ["profileId"]);
  const status = await w.message({ kind: "setup-status" });
  assert.equal(status.result.sessions[0].state, "waiting");
  assert.equal(status.result.sessions[0].tab, undefined);
  assert.equal(w.intervals[0].ms, 20_000);
  w.intervals[0].callback();
  assert.equal(c.socket.sent.at(-1).kind, "heartbeat");
});

test("grant requires the displayed exact page; debugger attaches only on a valid relay attach", async () => {
  const w = worker();
  const c = await w.pair();
  assert.equal((await w.message({ kind: "setup-grant", sessionId: c.id, tabId: 1, url: "https://stale.example/" })).ok, false);
  await w.grant(c);
  assert.equal(w.calls.length, 0);
  const denied = w.command(c, "Runtime.enable");
  await settle();
  assert.match(c.socket.sent.find((message) => message.reqId === denied).error.message, /not attached/);
  await w.attach(c);
  assert.equal(w.calls[0][0], "attach");
  const foreign = w.command(c, "Runtime.enable", { tabId: 2 });
  await settle();
  assert.match(c.socket.sent.find((message) => message.reqId === foreign).error.message, /not authorized/);
  assert.equal(w.calls.length, 1);
});

test("multiple sessions may use distinct tabs but cannot both authorize the same tab", async () => {
  const w = worker();
  await w.grant(await w.pair("first"));
  const second = await w.pair("second");
  const conflict = await w.message({ kind: "setup-grant", sessionId: second.id, tabId: 1, url: w.tabs.get(1).url });
  assert.equal(conflict.ok, false);
  assert.match(conflict.error, /Another session/);
  await w.grant(second, 2);
  assert.equal((await w.message({ kind: "setup-status" })).result.sessions.length, 2);
});

test("native child sessions and protocol errors survive the debugger transport", async () => {
  const w = worker();
  const c = await w.grant(await w.pair());
  await w.attach(c);
  w.chrome.debugger.onEvent.emit({ tabId: 1 }, "Target.attachedToTarget", { sessionId: "child", targetInfo: { targetId: "frame", type: "iframe" } });
  w.chrome.debugger.onEvent.emit({ tabId: 1, sessionId: "child" }, "Target.attachedToTarget", { sessionId: "nested", targetInfo: { targetId: "worker", type: "worker" } });
  w.chrome.debugger.onEvent.emit({ tabId: 1, sessionId: "nested" }, "Runtime.consoleAPICalled", { type: "log" });
  assert.equal(c.socket.sent.at(-1).sessionId, "nested");
  w.command(c, "Runtime.enable", { sessionId: "nested" });
  await settle();
  assert.equal(w.calls.at(-1)[1].sessionId, "nested");
  const count = c.socket.sent.length;
  w.chrome.debugger.onEvent.emit({ tabId: 2 }, "Runtime.consoleAPICalled", {});
  w.chrome.debugger.onEvent.emit({ tabId: 1, sessionId: "unknown" }, "Runtime.consoleAPICalled", {});
  assert.equal(c.socket.sent.length, count);
  w.chrome.debugger.sendCommand = async () => { throw new Error('{"code":-32601,"message":"Method unavailable"}'); };
  const errorId = w.command(c, "Runtime.enable");
  await settle();
  assert.deepEqual(c.socket.sent.find((message) => message.reqId === errorId).error, { code: -32601, message: "Method unavailable" });
  w.chrome.debugger.onEvent.emit({ tabId: 1 }, "Target.detachedFromTarget", { sessionId: "child" });
  const stale = w.command(c, "Runtime.enable", { sessionId: "nested" });
  await settle();
  assert.match(c.socket.sent.find((message) => message.reqId === stale).error.message, /not part/);
});

test("auto-attach is executed with a restricted filter and shared workers are detached", async () => {
  const w = worker();
  const c = await w.grant(await w.pair());
  await w.attach(c);
  const params = { autoAttach: true, flatten: true, waitForDebuggerOnStart: false, filter: [{ type: "iframe", exclude: false }, { type: "worker", exclude: false }, { exclude: true }] };
  const allowed = w.command(c, "Target.setAutoAttach", { params });
  await settle();
  assert.equal(w.calls.at(-1)[0], "Target.setAutoAttach");
  assert.deepEqual(c.socket.sent.find((message) => message.reqId === allowed).result, { actual: true });
  const denied = w.command(c, "Target.setAutoAttach", { params: { autoAttach: true, flatten: true } });
  await settle();
  assert.match(c.socket.sent.find((message) => message.reqId === denied).error.message, /limited to/);
  const sent = c.socket.sent.length;
  w.chrome.debugger.onEvent.emit({ tabId: 1 }, "Target.attachedToTarget", { sessionId: "shared", targetInfo: { targetId: "shared", type: "shared_worker" } });
  await settle();
  assert.equal(w.calls.at(-1)[0], "Target.detachFromTarget");
  assert.equal(c.socket.sent.length, sent);
});

test("Stop invalidates immediately, cancels commands, and detaches an attach that finishes late", async () => {
  const w = worker();
  let finishAttach;
  w.chrome.debugger.attach = () => new Promise((resolve) => { finishAttach = resolve; });
  const c = await w.grant(await w.pair());
  const pending = w.command(c, "Bridge.attach");
  const stop = w.message({ kind: "setup-stop", sessionId: c.id });
  await settle();
  assert.match(c.socket.sent.find((message) => message.reqId === pending).error.message, /authorization ended/);
  const next = await w.pair("next");
  assert.equal((await w.message({ kind: "setup-grant", sessionId: next.id, tabId: 1, url: w.tabs.get(1).url })).ok, false);
  finishAttach();
  await stop;
  assert.equal(w.calls.at(-1)[0], "detach");
  assert.equal(c.socket.readyState, 3);
  assert.equal((await w.message({ kind: "setup-status" })).result.sessions[0].state, "stopped");
  await w.grant(next);
});

test("a failed debugger detach stays reserved until Chrome confirms detachment", async () => {
  const w = worker();
  const c = await w.grant(await w.pair());
  await w.attach(c);
  w.chrome.debugger.detach = async () => { throw new Error("Detach failed"); };
  const id = w.command(c, "Bridge.detach");
  await settle();
  assert.equal(c.socket.sent.find((message) => message.reqId === id).error.message, "Detach failed");
  const next = await w.pair("next");
  assert.equal((await w.message({ kind: "setup-grant", sessionId: next.id, tabId: 1, url: w.tabs.get(1).url })).ok, false);
  w.chrome.debugger.onDetach.emit({ tabId: 1 }, "canceled_by_user");
  await w.grant(next);
});

for (const [name, revoke] of [
  ["socket close", (w, c) => c.socket.close()],
  ["Chrome debugger detach", (w) => w.chrome.debugger.onDetach.emit({ tabId: 1 }, "canceled_by_user")],
  ["tab removal", (w) => w.chrome.tabs.onRemoved.emit(1)],
  ["tab replacement", (w) => w.chrome.tabs.onReplaced.emit(2, 1)],
  ["relay release", (w, c) => c.socket.receive({ kind: "released", reason: "CLI closed" })],
  ["restricted navigation", (w) => w.chrome.tabs.onUpdated.emit(1, { url: "chrome://settings/" }, { ...w.tabs.get(1), url: "chrome://settings/" })],
]) {
  test(`${name} revokes without reconnect or automatic reattach`, async () => {
    const w = worker();
    const c = await w.grant(await w.pair());
    await w.attach(c);
    revoke(w, c);
    await settle();
    assert.equal((await w.message({ kind: "setup-status" })).result.sessions[0].state, "stopped");
    w.command(c, "Bridge.attach");
    await settle();
    assert.equal(w.calls.filter(([method]) => method === "attach").length, 1);
    assert.equal(w.sockets.length, 1);
    assert.equal(c.socket.readyState, 3);
  });
}
