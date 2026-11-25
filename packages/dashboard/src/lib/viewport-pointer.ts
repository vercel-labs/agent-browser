export interface ViewportRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface DrawnFrame {
  width: number;
  height: number;
  pageScaleFactor: number;
}

export function mapCanvasPointToViewport(
  clientX: number,
  clientY: number,
  rect: ViewportRect,
  drawn: DrawnFrame | null,
  fallbackWidth: number,
  fallbackHeight: number,
): { x: number; y: number } | null {
  if (rect.width <= 0 || rect.height <= 0) return null;
  const targetWidth = drawn?.width ?? fallbackWidth;
  const targetHeight = drawn?.height ?? fallbackHeight;
  const pageScale = drawn?.pageScaleFactor ?? 1;
  const scaleX = targetWidth / rect.width;
  const scaleY = targetHeight / rect.height;
  return {
    x: Math.round(((clientX - rect.left) * scaleX) / pageScale),
    y: Math.round(((clientY - rect.top) * scaleY) / pageScale),
  };
}

export interface RightClickGuardState {
  lastRightMouseAt: number;
}

export const RIGHT_CLICK_DEDUP_MS = 100;

export function createRightClickGuardState(): RightClickGuardState {
  return { lastRightMouseAt: 0 };
}

export function recordRightMouseEvent(
  state: RightClickGuardState,
  eventType: string,
  button: number,
  now: number,
): void {
  if (
    button === 2 &&
    (eventType === "mousePressed" || eventType === "mouseReleased")
  ) {
    state.lastRightMouseAt = now;
  }
}

export function shouldSynthesizeContextMenuRightClick(
  state: RightClickGuardState,
  buttons: number,
  now: number,
): boolean {
  if ((buttons & 2) !== 0 || now - state.lastRightMouseAt <= RIGHT_CLICK_DEDUP_MS) {
    state.lastRightMouseAt = 0;
    return false;
  }
  return true;
}
