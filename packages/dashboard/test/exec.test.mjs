import assert from "node:assert/strict";
import { afterEach, mock, test } from "node:test";
import { execCommand, killSession, sessionArgs } from "../src/lib/exec.ts";

afterEach(() => mock.restoreAll());

const result = { success: true, exit_code: 0, stdout: "done", stderr: "" };

test("commands retain valid results and send their arguments", async () => {
  mock.method(globalThis, "fetch", async (url, init) => {
    assert.equal(url, "/api/exec");
    assert.deepEqual(JSON.parse(init.body), { args: ["--session", "a", "close"] });
    return Response.json(result);
  });
  assert.deepEqual(await execCommand(sessionArgs("a", "close")), result);
});

test("JSON decoding failures become command errors", async () => {
  mock.method(globalThis, "fetch", async () => new Response("<html>broken</html>"));
  const actual = await execCommand(["snapshot"]);
  assert.equal(actual.success, false);
  assert.match(actual.stderr, /Invalid response/);
});

test("successful HTTP responses still require the command result shape", async () => {
  for (const payload of [null, [], {}, { success: true }, { ...result, stdout: null }]) {
    mock.method(globalThis, "fetch", async () => Response.json(payload));
    const actual = await execCommand(["snapshot"]);
    assert.equal(actual.success, false);
    assert.equal(typeof actual.stderr, "string");
    mock.restoreAll();
  }
});

test("HTTP failures cannot report success and preserve useful server errors", async () => {
  mock.method(globalThis, "fetch", async () =>
    Response.json({ ...result, stderr: "Session is unavailable" }, { status: 503 }));
  const actual = await execCommand(["snapshot"]);
  assert.equal(actual.success, false);
  assert.equal(actual.stderr, "Session is unavailable");
});

test("unreachable servers become command errors", async () => {
  mock.method(globalThis, "fetch", async () => { throw new TypeError("offline"); });
  assert.equal((await execCommand(["snapshot"])).success, false);
});

test("kill responses handle invalid JSON, HTTP errors, malformed shapes and success", async () => {
  for (const response of [
    new Response("<html>broken</html>"),
    Response.json({ success: true }, { status: 500 }),
    Response.json(null),
    Response.json({ success: "true" }),
    Response.json({ success: true, killed_pid: "123" }),
  ]) {
    mock.method(globalThis, "fetch", async () => response);
    assert.deepEqual(await killSession("a"), { success: false });
    mock.restoreAll();
  }
  mock.method(globalThis, "fetch", async () => Response.json({ success: true, killed_pid: 123 }));
  assert.deepEqual(await killSession("a"), { success: true, killed_pid: 123 });
});
