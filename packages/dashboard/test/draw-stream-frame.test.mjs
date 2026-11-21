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
