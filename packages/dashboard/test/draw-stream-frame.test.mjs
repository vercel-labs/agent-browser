import assert from "node:assert/strict";
import { afterEach, mock, test } from "node:test";
import { drawStreamFrame } from "../src/lib/draw-stream-frame.ts";

const originalDecoder = globalThis.createImageBitmap;
globalThis.createImageBitmap = async () => { throw new Error("decoder not mocked"); };
afterEach(() => mock.restoreAll());
process.on("exit", () => { globalThis.createImageBitmap = originalDecoder; });

function canvas() {
  const draws = [];
  return { width: 0, height: 0, draws, getContext: () => ({ drawImage: (...args) => draws.push(args) }) };
}

test("a current frame is drawn and its bitmap is released", async () => {
  const target = canvas();
  let closes = 0;
  const bitmap = { width: 20, height: 10, close() { closes++; } };
  mock.method(globalThis, "createImageBitmap", async () => bitmap);
  drawStreamFrame(target, "AA==");
  await Promise.resolve();
  assert.equal(target.width, 20);
  assert.equal(target.height, 10);
  assert.deepEqual(target.draws, [[bitmap, 0, 0]]);
  assert.equal(closes, 1);
});

test("a decode completed after cleanup cannot draw and still releases its bitmap", async () => {
  const target = canvas();
  let resolve;
  let closes = 0;
  mock.method(globalThis, "createImageBitmap", () => new Promise((done) => { resolve = done; }));
  const cancel = drawStreamFrame(target, "AA==");
  cancel();
  resolve({ width: 20, height: 10, close() { closes++; } });
  await Promise.resolve();
  assert.equal(target.width, 0);
  assert.deepEqual(target.draws, []);
  assert.equal(closes, 1);
});

test("a rejected decode does not draw or create an unhandled rejection", async () => {
  const target = canvas();
  mock.method(globalThis, "createImageBitmap", async () => { throw new Error("damaged frame"); });
  drawStreamFrame(target, "AA==");
  await Promise.resolve();
  assert.deepEqual(target.draws, []);
});

test("frame metadata is published only after a successful draw", async () => {
  const target = canvas();
  const bitmap = { width: 40, height: 30, close() {} };
  const published = [];
  mock.method(globalThis, "createImageBitmap", async () => bitmap);
  drawStreamFrame(target, "AA==", () => {
    published.push({ width: target.width, height: target.height, draws: target.draws.length });
  });
  await Promise.resolve();
  assert.deepEqual(published, [{ width: 40, height: 30, draws: 1 }]);
});

test("an obsolete decode cannot replace a newer frame or its metadata", async () => {
  const target = canvas();
  const pending = [];
  const closed = [];
  const published = [];
  mock.method(globalThis, "createImageBitmap", () => new Promise((resolve) => pending.push(resolve)));
  const cancelOld = drawStreamFrame(target, "AA==", () => published.push("old"));
  cancelOld();
  drawStreamFrame(target, "AQ==", () => published.push("new"));
  const newer = { width: 40, height: 30, close() { closed.push("new"); } };
  pending[1](newer);
  await Promise.resolve();
  pending[0]({ width: 20, height: 10, close() { closed.push("old"); } });
  await Promise.resolve();
  assert.deepEqual(target.draws, [[newer, 0, 0]]);
  assert.deepEqual(published, ["new"]);
  assert.deepEqual(closed, ["new", "old"]);
  assert.equal(target.width, 40);
  assert.equal(target.height, 30);
});

test("a failed draw releases the bitmap without publishing metadata", async () => {
  const target = canvas();
  target.getContext = () => ({ drawImage() { throw new Error("canvas unavailable"); } });
  let closes = 0;
  let published = false;
  mock.method(globalThis, "createImageBitmap", async () => ({ width: 20, height: 10, close() { closes++; } }));
  drawStreamFrame(target, "AA==", () => { published = true; });
  await Promise.resolve();
  assert.equal(closes, 1);
  assert.equal(published, false);
});

test("a missing context releases the bitmap without changing metadata or dimensions", async () => {
  const target = canvas();
  target.getContext = () => null;
  let closes = 0;
  let published = false;
  mock.method(globalThis, "createImageBitmap", async () => ({ width: 20, height: 10, close() { closes++; } }));
  drawStreamFrame(target, "AA==", () => { published = true; });
  await Promise.resolve();
  assert.equal(closes, 1);
  assert.equal(published, false);
  assert.equal(target.width, 0);
  assert.equal(target.height, 0);
});
