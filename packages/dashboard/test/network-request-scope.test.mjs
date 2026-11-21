import assert from "node:assert/strict";
import { test } from "node:test";
import { createLatestRequest } from "../src/lib/latest-request.ts";

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

test("independent list and detail requests do not cancel each other", async () => {
  const list = createLatestRequest();
  const detail = createLatestRequest();
  const listResponse = deferred();
  const firstDetail = deferred();
  const nextDetail = deferred();
  const applied = [];
  const listing = list.run(() => listResponse.promise, (value) => applied.push(value), () => {});
  const stale = detail.run(() => firstDetail.promise, (value) => applied.push(value), () => {});
  const current = detail.run(() => nextDetail.promise, (value) => applied.push(value), () => {});
  nextDetail.resolve("selected request");
  await current;
  firstDetail.resolve("previous request");
  await stale;
  listResponse.resolve("request list");
  await listing;
  assert.deepEqual(applied, ["selected request", "request list"]);
});
