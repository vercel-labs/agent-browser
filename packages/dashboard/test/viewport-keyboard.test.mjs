import test from "node:test";
import assert from "node:assert/strict";
import {
  buildKeyboardInputMessage,
  createInputQueue,
  shouldWriteClipboardText,
} from "../src/lib/viewport-keyboard.ts";

test("AltGr and Option character entry preserve text and strip Ctrl+Alt on AltGr", () => {
  // AltGr+Q -> "@" (Ctrl+Alt reported by browser, modifiers=3).
  const altGr = buildKeyboardInputMessage(
    {
      key: "@",
      code: "KeyQ",
      altKey: true,
      ctrlKey: true,
      metaKey: false,
      shiftKey: false,
    },
    "keyDown",
  );
  assert.equal(altGr.eventType, "keyDown");
  assert.equal(altGr.text, "@");
  assert.equal(Number(altGr.modifiers) & 2, 0);

  // macOS Option+A -> "å" (Alt only).
  const optionA = buildKeyboardInputMessage(
    {
      key: "å",
      code: "KeyA",
      altKey: true,
      ctrlKey: false,
      metaKey: false,
      shiftKey: false,
    },
    "keyDown",
  );
  assert.equal(optionA.eventType, "keyDown");
  assert.equal(optionA.text, "å");

  // Standard shortcuts stay textless rawKeyDown.
  const ctrlC = buildKeyboardInputMessage(
    {
      key: "c",
      code: "KeyC",
      altKey: false,
      ctrlKey: true,
      metaKey: false,
      shiftKey: false,
    },
    "keyDown",
  );
  assert.equal(ctrlC.eventType, "rawKeyDown");
  assert.equal(ctrlC.text, undefined);
});

test("empty clipboard replies do not overwrite the local clipboard", () => {
  assert.equal(shouldWriteClipboardText(""), false);
  assert.equal(shouldWriteClipboardText(undefined), false);
  assert.equal(shouldWriteClipboardText("copied"), true);
});

test("input queue holds subsequent Tab and mouse events behind an async paste read", async () => {
  const sent = [];
  const queue = createInputQueue((msg) => sent.push(msg));

  let resolveClipboard;
  const clipboardPromise = new Promise((resolve) => {
    resolveClipboard = resolve;
  });

  const pasteDone = queue.enqueueAsync(async () => {
    const text = await clipboardPromise;
    return { type: "input_insert_text", text };
  });

  queue.enqueue({ type: "input_keyboard", eventType: "rawKeyDown", key: "Tab" });
  queue.enqueue({ type: "input_mouse", eventType: "mousePressed", x: 10, y: 20 });

  assert.equal(sent.length, 0, "subsequent input must wait while paste is reading clipboard");

  resolveClipboard("PASTED");
  await pasteDone;
  // Let microtask drain finish queued sync items.
  await Promise.resolve();

  assert.deepEqual(
    sent.map((m) => [m.type, m.text ?? m.key ?? m.eventType]),
    [
      ["input_insert_text", "PASTED"],
      ["input_keyboard", "Tab"],
      ["input_mouse", "mousePressed"],
    ],
  );
});
