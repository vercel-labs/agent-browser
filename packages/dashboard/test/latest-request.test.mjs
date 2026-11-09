import assert from "node:assert/strict";
import { test } from "node:test";
import { createLatestRequest } from "../src/lib/latest-request.ts";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

test("an older response cannot replace data or clear a newer request's loading state", async () => {
  const requests = createLatestRequest();
  const first = deferred();
  const second = deferred();
  const visible = [];
  let loading = true;
  const old = requests.run(() => first.promise, (value) => visible.push(value), () => { loading = false; });
  const current = requests.run(() => second.promise, (value) => visible.push(value), () => { loading = false; });
  first.resolve("previous session");
  await old;
  assert.deepEqual(visible, []);
  assert.equal(loading, true);
  second.resolve("current session");
  await current;
  assert.deepEqual(visible, ["current session"]);
  assert.equal(loading, false);
});

test("cleanup prevents both data and loading changes after a request resolves", async () => {
  const requests = createLatestRequest();
  const response = deferred();
  const visible = [];
  let completed = false;
  const pending = requests.run(() => response.promise, (value) => visible.push(value), () => { completed = true; });
  requests.invalidate();
  response.resolve("late");
  await pending;
  assert.deepEqual(visible, []);
  assert.equal(completed, false);
});

test("a current failure settles loading without applying data", async () => {
  const requests = createLatestRequest();
  const response = deferred();
  let applied = false;
  let completed = false;
  const pending = requests.run(() => response.promise, () => { applied = true; }, () => { completed = true; });
  response.reject(new Error("failed"));
  await assert.rejects(pending, /failed/);
  assert.equal(applied, false);
  assert.equal(completed, true);
});
