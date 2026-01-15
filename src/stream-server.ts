import { WebSocketServer, WebSocket } from 'ws';
import type { BrowserManager, ScreencastFrame } from './browser.js';
import { setScreencastFrameCallback } from './actions.js';

// Message types for WebSocket communication
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

export interface InputMouseMessage {
  type: 'input_mouse';
  eventType: 'mousePressed' | 'mouseReleased' | 'mouseMoved' | 'mouseWheel';
  x: number;
  y: number;
  button?: 'left' | 'right' | 'middle' | 'none';
  clickCount?: number;
  deltaX?: number;
  deltaY?: number;
  modifiers?: number;
}

export interface InputKeyboardMessage {
  type: 'input_keyboard';
  eventType: 'keyDown' | 'keyUp' | 'char';
  key?: string;
  code?: string;
  text?: string;
  modifiers?: number;
}

export interface InputTouchMessage {
  type: 'input_touch';
  eventType: 'touchStart' | 'touchEnd' | 'touchMove' | 'touchCancel';
  touchPoints: Array<{ x: number; y: number; id?: number }>;
  modifiers?: number;
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

export type StreamMessage =
  | FrameMessage
  | InputMouseMessage
  | InputKeyboardMessage
  | InputTouchMessage
  | StatusMessage
  | ErrorMessage;

/**
 * WebSocket server for streaming browser viewport and receiving input
 */
export class StreamServer {
  private wss: WebSocketServer | null = null;
  private clients: Set<WebSocket> = new Set();
  private browser: BrowserManager;
  private port: number;
  private isScreencasting: boolean = false;

  constructor(browser: BrowserManager, port: number = 9223) {
    this.browser = browser;
    this.port = port;
  }

  /**
   * Start the WebSocket server
   */
  start(): Promise<void> {
    return new Promise((resolve, reject) => {
      try {
        this.wss = new WebSocketServer({ port: this.port });

        this.wss.on('connection', (ws) => {
          this.handleConnection(ws);
        });

        this.wss.on('error', (error) => {
          console.error('[StreamServer] WebSocket error:', error);
          reject(error);
        });

        this.wss.on('listening', () => {
          console.log(`[StreamServer] Listening on port ${this.port}`);

          // Set up the screencast frame callback
          setScreencastFrameCallback((frame) => {
            this.broadcastFrame(frame);
          });

          resolve();
        });
      } catch (error) {
        reject(error);
      }
    });
  }

  /**
   * Stop the WebSocket server
   */
  async stop(): Promise<void> {
    // Stop screencasting
    if (this.isScreencasting) {
      await this.stopScreencast();
    }

    // Clear the callback
    setScreencastFrameCallback(null);

    // Close all clients
    for (const client of this.clients) {
      client.close();
    }
    this.clients.clear();

    // Close the server
    if (this.wss) {
      return new Promise((resolve) => {
        this.wss!.close(() => {
          this.wss = null;
          resolve();
        });
      });
    }
  }

  /**
   * Handle a new WebSocket connection
   */
  private handleConnection(ws: WebSocket): void {
    console.log('[StreamServer] Client connected');
    this.clients.add(ws);

    // Send initial status
    this.sendStatus(ws);

    // Start screencasting if this is the first client
    if (this.clients.size === 1 && !this.isScreencasting) {
      this.startScreencast().catch((error) => {
        console.error('[StreamServer] Failed to start screencast:', error);
        this.sendError(ws, error.message);
      });
    }

    // Handle messages from client
    ws.on('message', (data) => {
      try {
        const message = JSON.parse(data.toString()) as StreamMessage;
        this.handleMessage(message, ws);
      } catch (error) {
        console.error('[StreamServer] Failed to parse message:', error);
      }
    });

    // Handle client disconnect
    ws.on('close', () => {
      console.log('[StreamServer] Client disconnected');
      this.clients.delete(ws);

      // Stop screencasting if no more clients
      if (this.clients.size === 0 && this.isScreencasting) {
        this.stopScreencast().catch((error) => {
          console.error('[StreamServer] Failed to stop screencast:', error);
        });
      }
    });

    ws.on('error', (error) => {
      console.error('[StreamServer] Client error:', error);
      this.clients.delete(ws);
    });
  }

  /**
   * Handle incoming messages from clients
   */
  private async handleMessage(message: StreamMessage, ws: WebSocket): Promise<void> {
    try {
      switch (message.type) {
        case 'input_mouse':
          await this.browser.injectMouseEvent({
            type: message.eventType,
            x: message.x,
            y: message.y,
            button: message.button,
            clickCount: message.clickCount,
            deltaX: message.deltaX,
            deltaY: message.deltaY,
            modifiers: message.modifiers,
          });
          break;

        case 'input_keyboard':
          await this.browser.injectKeyboardEvent({
            type: message.eventType,
            key: message.key,
            code: message.code,
            text: message.text,
            modifiers: message.modifiers,
          });
          break;

        case 'input_touch':
          await this.browser.injectTouchEvent({
            type: message.eventType,
            touchPoints: message.touchPoints,
            modifiers: message.modifiers,
          });
          break;

        case 'status':
          // Client is requesting status
          this.sendStatus(ws);
          break;
      }
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      this.sendError(ws, errorMessage);
    }
  }

  /**
   * Broadcast a frame to all connected clients
   */
  private broadcastFrame(frame: ScreencastFrame): void {
    const message: FrameMessage = {
      type: 'frame',
      data: frame.data,
      metadata: frame.metadata,
    };

    const payload = JSON.stringify(message);

    for (const client of this.clients) {
      if (client.readyState === WebSocket.OPEN) {
        client.send(payload);
      }
    }
  }

  /**
   * Send status to a client
   */
  private sendStatus(ws: WebSocket): void {
    let viewportWidth: number | undefined;
    let viewportHeight: number | undefined;

    try {
      const page = this.browser.getPage();
      const viewport = page.viewportSize();
      viewportWidth = viewport?.width;
      viewportHeight = viewport?.height;
    } catch {
      // Browser not launched yet
    }

    const message: StatusMessage = {
      type: 'status',
      connected: true,
      screencasting: this.isScreencasting,
      viewportWidth,
      viewportHeight,
    };

    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(message));
    }
  }

  /**
   * Send an error to a client
   */
  private sendError(ws: WebSocket, errorMessage: string): void {
    const message: ErrorMessage = {
      type: 'error',
      message: errorMessage,
    };

    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(message));
    }
  }

  /**
   * Start screencasting
   */
  private async startScreencast(): Promise<void> {
    // Set flag immediately to prevent race conditions with concurrent calls
    if (this.isScreencasting) return;
    this.isScreencasting = true;

    try {
      // Check if browser is launched
      if (!this.browser.isLaunched()) {
        throw new Error('Browser not launched');
      }

      await this.browser.startScreencast((frame) => this.broadcastFrame(frame), {
        format: 'jpeg',
        quality: 80,
        maxWidth: 1280,
        maxHeight: 720,
        everyNthFrame: 1,
      });

      // Notify all clients
      for (const client of this.clients) {
        this.sendStatus(client);
      }
    } catch (error) {
      // Reset flag on failure so caller can retry
      this.isScreencasting = false;
      throw error;
    }
  }

  /**
   * Stop screencasting
   */
  private async stopScreencast(): Promise<void> {
    if (!this.isScreencasting) return;

    await this.browser.stopScreencast();
    this.isScreencasting = false;

    // Notify all clients
    for (const client of this.clients) {
      this.sendStatus(client);
    }
  }

  /**
   * Get the port the server is running on
   */
  getPort(): number {
    return this.port;
  }

  /**
   * Get the number of connected clients
   */
  getClientCount(): number {
    return this.clients.size;
  }
}
