export interface KeyEventLike {
  key: string;
  code: string;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  getModifierState?: (key: string) => boolean;
}

const KEY_INFO: Record<string, { text?: string; keyCode: number }> = {
  Enter: { text: "\r", keyCode: 13 },
  Tab: { text: "\t", keyCode: 9 },
  Backspace: { text: "\b", keyCode: 8 },
  Escape: { keyCode: 27 },
  ArrowLeft: { keyCode: 37 },
  ArrowUp: { keyCode: 38 },
  ArrowRight: { keyCode: 39 },
  ArrowDown: { keyCode: 40 },
  Delete: { keyCode: 46 },
  Home: { keyCode: 36 },
  End: { keyCode: 35 },
  PageUp: { keyCode: 33 },
  PageDown: { keyCode: 34 },
};

const CODE_KEY_INFO: Record<string, number> = {
  Space: 32,
  Minus: 189,
  Equal: 187,
  BracketLeft: 219,
  BracketRight: 221,
  Backslash: 220,
  Semicolon: 186,
  Quote: 222,
  Backquote: 192,
  Comma: 188,
  Period: 190,
  Slash: 191,
  ShiftLeft: 16,
  ShiftRight: 16,
  ControlLeft: 17,
  ControlRight: 17,
  AltLeft: 18,
  AltRight: 18,
  MetaLeft: 91,
  MetaRight: 92,
  Insert: 45,
};

function printableKeyCode(key: string): number {
  if (key.length !== 1) return 0;
  const code = key.charCodeAt(0);
  if (code >= 65 && code <= 90) return code;
  if (code >= 97 && code <= 122) return code - 32;
  if (code >= 48 && code <= 57) return code;
  switch (key) {
    case " ":
      return 32;
    case ";":
    case ":":
      return 186;
    case "=":
    case "+":
      return 187;
    case ",":
    case "<":
      return 188;
    case "-":
    case "_":
      return 189;
    case ".":
    case ">":
      return 190;
    case "/":
    case "?":
      return 191;
    case "`":
    case "~":
      return 192;
    case "[":
    case "{":
      return 219;
    case "\\":
    case "|":
      return 220;
    case "]":
    case "}":
      return 221;
    case "'":
    case '"':
      return 222;
    default:
      return 0;
  }
}

export function vkCodeForEvent(e: KeyEventLike): number {
  const { code } = e;
  if (/^Key[A-Z]$/.test(code)) return code.charCodeAt(3);
  if (/^Digit[0-9]$/.test(code)) return 48 + Number(code[5]);
  if (/^Numpad[0-9]$/.test(code)) return 96 + Number(code[6]);
  if (/^F([1-9]|1[0-2])$/.test(code)) return 111 + Number(code.slice(1));
  const fromCode = CODE_KEY_INFO[code];
  if (fromCode !== undefined) return fromCode;
  const info = KEY_INFO[e.key];
  if (info) return info.keyCode;
  return printableKeyCode(e.key);
}

export function macKeyCommands(e: KeyEventLike): string[] | undefined {
  if (!e.metaKey) return undefined;
  const shift = e.shiftKey;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  switch (key) {
    case "a":
      return ["selectAll"];
    case "c":
      return ["copy"];
    case "x":
      return ["cut"];
    case "v":
      return ["paste"];
    case "z":
      return shift ? ["redo"] : ["undo"];
    case "ArrowLeft":
      return [
        shift
          ? "moveToBeginningOfLineAndModifySelection"
          : "moveToBeginningOfLine",
      ];
    case "ArrowRight":
      return [
        shift ? "moveToEndOfLineAndModifySelection" : "moveToEndOfLine",
      ];
    case "ArrowUp":
      return [
        shift
          ? "moveToBeginningOfDocumentAndModifySelection"
          : "moveToBeginningOfDocument",
      ];
    case "ArrowDown":
      return [
        shift
          ? "moveToEndOfDocumentAndModifySelection"
          : "moveToEndOfDocument",
      ];
    case "Backspace":
      return ["deleteToBeginningOfLine"];
    default:
      return undefined;
  }
}

function matchesPlainCodeChar(e: KeyEventLike): boolean {
  if (/^Key[A-Z]$/.test(e.code)) {
    return e.key.length === 1 && e.key.toUpperCase() === e.code[3];
  }
  if (/^Digit[0-9]$/.test(e.code)) {
    return e.key === e.code[5];
  }
  return false;
}

export function shouldSuppressKeyText(e: KeyEventLike): boolean {
  if (e.metaKey) return true;
  if (e.ctrlKey && !e.altKey) return true;
  if (e.altKey) {
    if (e.getModifierState?.("AltGraph")) return false;
    return matchesPlainCodeChar(e);
  }
  return false;
}

export function keyModifiers(e: KeyEventLike): number {
  let m = 0;
  if (e.altKey) m |= 1;
  if (e.ctrlKey) m |= 2;
  if (e.metaKey) m |= 4;
  if (e.shiftKey) m |= 8;
  return m;
}

export function buildKeyboardInputMessage(
  e: KeyEventLike,
  eventType: string,
): Record<string, unknown> {
  const info = KEY_INFO[e.key];
  const suppressText = shouldSuppressKeyText(e);
  const isDown = eventType === "keyDown" || eventType === "rawKeyDown";
  const text =
    isDown && !suppressText
      ? (info?.text ?? (Array.from(e.key).length === 1 ? e.key : undefined))
      : undefined;
  const resolvedEventType =
    eventType === "keyDown" && text === undefined ? "rawKeyDown" : eventType;
  const commands = isDown ? macKeyCommands(e) : undefined;
  let modifiers = keyModifiers(e);
  // AltGr reports Ctrl+Alt (bits 1|2 = 3), but Chrome ignores `text` on keyDown
  // when Ctrl is set; strip Ctrl|Alt when forwarding printable AltGr text.
  if (resolvedEventType === "keyDown" && text && (modifiers & 3) === 3) {
    modifiers &= ~3;
  }
  return {
    type: "input_keyboard",
    eventType: resolvedEventType,
    key: e.key,
    code: e.code,
    text,
    windowsVirtualKeyCode: vkCodeForEvent(e),
    modifiers,
    ...(commands ? { commands } : {}),
  };
}

export function shouldWriteClipboardText(text: string | undefined): text is string {
  return typeof text === "string" && text.length > 0;
}

export interface InputQueue {
  enqueue(msg: Record<string, unknown>): void;
  enqueueAsync(task: () => Promise<Record<string, unknown> | null>): Promise<void>;
  reset(): void;
}

type QueueItem =
  | { kind: "sync"; msg: Record<string, unknown> }
  | {
      kind: "async";
      task: () => Promise<Record<string, unknown> | null>;
      resolveDone: () => void;
    };

/**
 * Serializes stream input around async operations (such as clipboard readText
 * on paste) so subsequent keystrokes or clicks do not overtake an in-flight
 * paste. Calling `reset()` cancels queued input and discards any in-flight
 * async result so clipboard text never lands in a different session or tab.
 */
export function createInputQueue(
  send: (msg: Record<string, unknown>) => void,
): InputQueue {
  let inFlight = false;
  let epoch = 0;
  const pending: QueueItem[] = [];

  const drain = async () => {
    if (inFlight) return;
    inFlight = true;
    const drainEpoch = epoch;
    try {
      while (pending.length > 0 && epoch === drainEpoch) {
        const next = pending.shift()!;
        if (next.kind === "sync") {
          send(next.msg);
        } else {
          try {
            const msg = await next.task();
            if (epoch === drainEpoch && msg) {
              send(msg);
            }
          } finally {
            next.resolveDone();
          }
        }
      }
    } finally {
      if (epoch === drainEpoch) {
        inFlight = false;
      }
    }
  };

  return {
    enqueue(msg) {
      if (!inFlight && pending.length === 0) {
        send(msg);
        return;
      }
      pending.push({ kind: "sync", msg });
      void drain();
    },
    enqueueAsync(task) {
      let resolveDone!: () => void;
      const done = new Promise<void>((resolve) => {
        resolveDone = resolve;
      });
      pending.push({ kind: "async", task, resolveDone });
      void drain();
      return done;
    },
    reset() {
      epoch++;
      inFlight = false;
      while (pending.length > 0) {
        const item = pending.shift()!;
        if (item.kind === "async") {
          item.resolveDone();
        }
      }
    },
  };
}
