/**
 * Screencast & Input Injection API
 * For collaborative browsing, pair programming, and real-time monitoring
 */

export interface ScreencastOptions {
  format?: 'jpeg' | 'png';
  quality?: number;
  maxWidth?: number;
  maxHeight?: number;
  everyNthFrame?: number;
}

export interface MouseEventParams {
  type: 'mousePressed' | 'mouseReleased' | 'mouseMoved' | 'mouseWheel';
  x: number;
  y: number;
  button?: 'left' | 'right' | 'middle' | 'none';
  clickCount?: number;
  deltaX?: number;
  deltaY?: number;
  modifiers?: number;
}

export interface KeyboardEventParams {
  type: 'keyDown' | 'keyUp' | 'char';
  key?: string;
  code?: string;
  text?: string;
  modifiers?: number;
}

export interface TouchEventParams {
  type: 'touchStart' | 'touchEnd' | 'touchMove' | 'touchCancel';
  touchPoints: Array<{ x: number; y: number; id?: number }>;
  modifiers?: number;
}

/**
 * Screencast route definitions
 */
export const screencastRoutes = {
  // Screencast control
  'POST /screencast/start': 'screencast_start',
  'GET /screencast/stop': 'screencast_stop',
  'GET /screencast/status': 'screencast_status',

  // Input injection
  'POST /input/mouse': 'input_mouse',
  'POST /input/keyboard': 'input_keyboard',
  'POST /input/touch': 'input_touch',

  // WebSocket stream
  'WS /stream': 'websocket',
};

/**
 * WebSocket message types for real-time streaming
 */
export interface FrameMessage {
  type: 'frame';
  data: string; // base64 encoded image
  metadata: {
    offsetTop: number;
    pageScaleFactor: number;
    deviceWidth: number;
    deviceHeight: number;
    scrollOffsetX: number;
    scrollOffsetY: number;
    timestamp?: number;
  };
}

export interface StatusMessage {
  type: 'status';
  connected: boolean;
  screencasting: boolean;
  viewportWidth?: number;
  viewportHeight?: number;
}

export interface ErrorMessage {
  type: 'error';
  message: string;
}

export type StreamMessage = FrameMessage | StatusMessage | ErrorMessage;

/**
 * Parse screencast request
 */
export function parseScreencastRequest(body: string): ScreencastOptions {
  let params: ScreencastOptions = {};

  if (body) {
    try {
      params = JSON.parse(body);
    } catch {
      // Use defaults
    }
  }

  return {
    format: params.format || 'jpeg',
    quality: params.quality || 80,
    maxWidth: params.maxWidth || 1280,
    maxHeight: params.maxHeight || 720,
    everyNthFrame: params.everyNthFrame || 1,
  };
}

/**
 * Screencast configuration presets
 */
export const screencastPresets = {
  // High quality
  hd: {
    format: 'png' as const,
    quality: 95,
    maxWidth: 1920,
    maxHeight: 1080,
    everyNthFrame: 1,
  },

  // Balanced
  balanced: {
    format: 'jpeg' as const,
    quality: 80,
    maxWidth: 1280,
    maxHeight: 720,
    everyNthFrame: 1,
  },

  // Low bandwidth
  low: {
    format: 'jpeg' as const,
    quality: 60,
    maxWidth: 640,
    maxHeight: 480,
    everyNthFrame: 2,
  },

  // Mobile
  mobile: {
    format: 'jpeg' as const,
    quality: 75,
    maxWidth: 375,
    maxHeight: 667,
    everyNthFrame: 1,
  },
};

/**
 * Get preset configuration
 */
export function getScreencastPreset(presetName: string): ScreencastOptions {
  return (
    (screencastPresets as Record<string, ScreencastOptions>)[presetName] ||
    screencastPresets.balanced
  );
}

/**
 * Helper to create mouse event
 */
export function createMouseEvent(
  type: MouseEventParams['type'],
  x: number,
  y: number,
  button: MouseEventParams['button'] = 'left'
): MouseEventParams {
  return {
    type,
    x,
    y,
    button,
  };
}

/**
 * Helper to create keyboard event
 */
export function createKeyboardEvent(
  type: KeyboardEventParams['type'],
  key: string
): KeyboardEventParams {
  return {
    type,
    key,
  };
}

/**
 * Helper to create touch event
 */
export function createTouchEvent(
  type: TouchEventParams['type'],
  x: number,
  y: number,
  id?: number
): TouchEventParams {
  return {
    type,
    touchPoints: [{ x, y, id }],
  };
}

/**
 * Common input sequences
 */
export const inputSequences = {
  /**
   * Click at coordinates
   */
  click: (x: number, y: number) => [
    createMouseEvent('mousePressed', x, y, 'left'),
    createMouseEvent('mouseReleased', x, y, 'left'),
  ],

  /**
   * Double click
   */
  doubleClick: (x: number, y: number) => [
    createMouseEvent('mousePressed', x, y, 'left'),
    createMouseEvent('mouseReleased', x, y, 'left'),
    createMouseEvent('mousePressed', x, y, 'left'),
    createMouseEvent('mouseReleased', x, y, 'left'),
  ],

  /**
   * Type text (character by character)
   */
  typeText: (text: string) =>
    text.split('').map((char) => ({
      type: 'char' as const,
      text: char,
    })),

  /**
   * Press key
   */
  pressKey: (key: string) => [
    createKeyboardEvent('keyDown', key),
    createKeyboardEvent('keyUp', key),
  ],

  /**
   * Drag from one point to another
   */
  drag: (x1: number, y1: number, x2: number, y2: number) => [
    createMouseEvent('mousePressed', x1, y1, 'left'),
    createMouseEvent('mouseMoved', x2, y2, 'left'),
    createMouseEvent('mouseReleased', x2, y2, 'left'),
  ],

  /**
   * Touch at coordinates
   */
  touch: (x: number, y: number) => [
    createTouchEvent('touchStart', x, y),
    createTouchEvent('touchEnd', x, y),
  ],
};
