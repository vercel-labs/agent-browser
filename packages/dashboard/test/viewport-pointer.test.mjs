import test from "node:test";
import assert from "node:assert/strict";
import {
  createRightClickGuardState,
  mapCanvasPointToViewport,
  recordRightMouseEvent,
  shouldSynthesizeContextMenuRightClick,
} from "../src/lib/viewport-pointer.ts";

test("mapCanvasPointToViewport ignores nonzero offsetTop because canvas draws bitmap from (0, 0)", () => {
  const rect = { left: 100, top: 50, width: 500, height: 400 };
  const drawn = {
    width: 1000,
    height: 800,
    offsetTop: 80,
    pageScaleFactor: 1,
  };

  assert.deepEqual(
    mapCanvasPointToViewport(350, 50, rect, drawn, 1280, 720),
    { x: 500, y: 0 },
  );
  assert.deepEqual(
    mapCanvasPointToViewport(350, 250, rect, drawn, 1280, 720),
    { x: 500, y: 400 },
  );
  assert.deepEqual(
    mapCanvasPointToViewport(350, 450, rect, drawn, 1280, 720),
    { x: 500, y: 800 },
  );
});

test("mapCanvasPointToViewport applies pageScaleFactor and falls back when no frame is drawn", () => {
  const rect = { left: 0, top: 0, width: 500, height: 400 };
  const zoomed = { width: 1000, height: 800, offsetTop: 0, pageScaleFactor: 2 };

  assert.deepEqual(
    mapCanvasPointToViewport(250, 200, rect, zoomed, 1280, 720),
    { x: 250, y: 200 },
  );
  assert.deepEqual(
    mapCanvasPointToViewport(250, 200, rect, null, 1000, 800),
    { x: 500, y: 400 },
  );
});

test("Windows right-click held >100ms before release does not duplicate on contextmenu", () => {
  const state = createRightClickGuardState();
  const dispatched = [];

  // mousedown(right) at t=1000
  recordRightMouseEvent(state, "mousePressed", 2, 1000);
  dispatched.push("mousePressed");

  // mouseup(right) at t=1250 (>100ms later)
  recordRightMouseEvent(state, "mouseReleased", 2, 1250);
  dispatched.push("mouseReleased");

  // contextmenu fires after mouseup on Windows
  if (shouldSynthesizeContextMenuRightClick(state, 0, 1252)) {
    dispatched.push("mousePressed", "mouseReleased");
  }

  assert.deepEqual(dispatched, ["mousePressed", "mouseReleased"]);
});

test("macOS right-click suppresses contextmenu while touchpad two-finger tap synthesizes", () => {
  const state = createRightClickGuardState();

  // macOS physical right-click: mousedown -> contextmenu -> mouseup
  recordRightMouseEvent(state, "mousePressed", 2, 1000);
  assert.equal(shouldSynthesizeContextMenuRightClick(state, 2, 1001), false);
  recordRightMouseEvent(state, "mouseReleased", 2, 1200);

  // Later two-finger tap without mousedown/mouseup
  assert.equal(shouldSynthesizeContextMenuRightClick(state, 0, 1500), true);
});

test("contextmenu while right button is held >100ms does not synthesize, and release outside canvas does not block later taps", () => {
  const state = createRightClickGuardState();

  recordRightMouseEvent(state, "mousePressed", 2, 1000);
  assert.equal(shouldSynthesizeContextMenuRightClick(state, 2, 1250), false);

  // Right button released outside canvas (no mouseReleased event), later two-finger tap
  assert.equal(shouldSynthesizeContextMenuRightClick(state, 0, 1500), true);
});
