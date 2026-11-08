/** CDP input helpers shared by the dashboard viewport and the PiP view. */

export function cdpModifiers(e: React.MouseEvent | React.WheelEvent): number {
  let m = 0;
  if (e.altKey) m |= 1;
  if (e.ctrlKey) m |= 2;
  if (e.metaKey) m |= 4;
  if (e.shiftKey) m |= 8;
  return m;
}

export const KEY_INFO: Record<string, { text?: string; keyCode: number }> = {
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

export function printableKeyCode(key: string): number {
  if (key.length !== 1) return 0;
  const code = key.charCodeAt(0);
  if (code >= 65 && code <= 90) return code;
  if (code >= 97 && code <= 122) return code - 32;
  if (code >= 48 && code <= 57) return code;
  switch (key) {
    case " ": return 32;
    case ";": case ":": return 186;
    case "=": case "+": return 187;
    case ",": case "<": return 188;
    case "-": case "_": return 189;
    case ".": case ">": return 190;
    case "/": case "?": return 191;
    case "`": case "~": return 192;
    case "[": case "{": return 219;
    case "\\": case "|": return 220;
    case "]": case "}": return 221;
    case "'": case "\"": return 222;
    default: return 0;
  }
}

export function cdpButton(btn: number): string {
  switch (btn) {
    case 0: return "left";
    case 1: return "middle";
    case 2: return "right";
    default: return "none";
  }
}
