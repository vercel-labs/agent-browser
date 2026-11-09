import assert from "node:assert/strict";
import { test } from "node:test";
import { cookieKey, parseCookies, parseStorageEntries } from "../src/lib/storage-data.ts";

test("cookies are read from the CLI data envelope", () => {
  const cookies = parseCookies(JSON.stringify({
    success: true, data: { cookies: [{ name: "token", value: "v", domain: "example.test", path: "/" }] },
  }));
  assert.equal(cookies.length, 1);
  assert.equal(cookies[0].name, "token");
  assert.equal(cookies[0].domain, "example.test");
});

test("Web Storage uses data.data and retains a key named length", () => {
  assert.deepEqual(parseStorageEntries(JSON.stringify({
    success: true, data: { data: { length: "saved", theme: "dark" } },
  })), [{ key: "length", value: "saved" }, { key: "theme", value: "dark" }]);
});

test("malformed cookie rows cannot reach the view", () => {
  const cookies = parseCookies(JSON.stringify({
    data: { cookies: [null, {}, { name: 1, value: "v" }, { name: "valid", value: "v", domain: {} }] },
  }));
  assert.equal(cookies.length, 1);
  assert.equal(cookies[0].domain, undefined);
});

test("cookies with the same name and domain retain distinct path identities", () => {
  assert.notEqual(
    cookieKey({ name: "token", value: "a", domain: "example.test", path: "/" }),
    cookieKey({ name: "token", value: "b", domain: "example.test", path: "/app" }),
  );
});

test("invalid JSON and unsupported response shapes produce empty lists", () => {
  for (const raw of ["broken", "null", "[]", "{}", '{"data":null}', '{"data":{"data":[],"cookies":{}}}']) {
    assert.deepEqual(parseCookies(raw), []);
    assert.deepEqual(parseStorageEntries(raw), []);
  }
});
