import http from 'node:http';
import { WebSocketServer, WebSocket } from 'ws';

export interface InspectServerOptions {
  chromeHostPort: string;
  targetId: string;
  chromeWsUrl: string;
}

let nextAttachId = -1000;

export function injectSessionId(json: string, sessionId: string): string {
  const msg = JSON.parse(json);
  msg.sessionId = sessionId;
  return JSON.stringify(msg);
}

export function stripSessionId(json: string): string {
  const msg = JSON.parse(json);
  delete msg.sessionId;
  return JSON.stringify(msg);
}

// The Node.js path opens its own WebSocket to Chrome rather than sharing
// Playwright's internal connection. This avoids interfering with Playwright's
// CDP session management. The Rust/native path takes the opposite approach,
// sharing the daemon's existing browser-level WebSocket via InspectProxyHandle.
export class InspectServer {
  private httpServer: http.Server;
  private wss: WebSocketServer;
  private chromeWs: WebSocket | null = null;
  private sessions = new Map<string, WebSocket>();
  private pendingAttaches = new Map<number, (sessionId: string | null) => void>();
  private _port: number = 0;

  constructor(private options: InspectServerOptions) {
    this.httpServer = http.createServer(this.handleHttp.bind(this));
    this.wss = new WebSocketServer({ server: this.httpServer, path: '/ws' });
    this.wss.on('connection', this.handleWsConnection.bind(this));
  }

  get port(): number {
    return this._port;
  }

  async start(): Promise<void> {
    await this.connectChrome();
    return new Promise((resolve, reject) => {
      this.httpServer.listen(0, '127.0.0.1', () => {
        const addr = this.httpServer.address();
        if (addr && typeof addr !== 'string') {
          this._port = addr.port;
        }
        resolve();
      });
      this.httpServer.on('error', reject);
    });
  }

  stop(): void {
    for (const [sessionId, devtoolsWs] of this.sessions) {
      this.detachSession(sessionId);
      devtoolsWs.close();
    }
    this.sessions.clear();
    this.chromeWs?.close();
    this.chromeWs = null;
    this.wss.close();
    this.httpServer.close();
  }

  private connectChrome(): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.options.chromeWsUrl);
      ws.on('open', () => {
        this.chromeWs = ws;
        resolve();
      });
      ws.on('error', (err) => {
        if (!this.chromeWs) {
          reject(new Error(`Chrome WebSocket connection failed: ${err.message}`));
        } else {
          console.error('[inspect] Chrome WebSocket error:', err.message);
          for (const devtoolsWs of this.sessions.values()) {
            devtoolsWs.close();
          }
          this.sessions.clear();
        }
      });
      ws.on('close', () => {
        this.chromeWs = null;
        for (const devtoolsWs of this.sessions.values()) {
          devtoolsWs.close();
        }
        this.sessions.clear();
      });
      ws.on('message', (data) => this.handleChromeMessage(data));
    });
  }

  private handleChromeMessage(data: unknown): void {
    try {
      const text = String(data);
      const msg = JSON.parse(text);

      // Check if this is a response to a pending attachToTarget request
      if (msg.id != null && msg.id < 0) {
        const resolve = this.pendingAttaches.get(msg.id);
        if (resolve) {
          this.pendingAttaches.delete(msg.id);
          resolve(msg.result?.sessionId ?? null);
          return;
        }
      }

      // Route session-scoped messages to the correct DevTools client
      const sessionId: string | undefined = msg.sessionId;
      if (!sessionId) return;

      const devtoolsWs = this.sessions.get(sessionId);
      if (!devtoolsWs || devtoolsWs.readyState !== WebSocket.OPEN) return;

      devtoolsWs.send(stripSessionId(text));
    } catch (err) {
      console.error('[inspect] Chrome message handling error:', err);
    }
  }

  private handleHttp(req: http.IncomingMessage, res: http.ServerResponse): void {
    if (req.url === '/' || req.url === '') {
      const location = `http://${this.options.chromeHostPort}/devtools/devtools_app.html?ws=127.0.0.1:${this._port}/ws`;
      res.writeHead(302, { Location: location, 'Content-Type': 'text/html' });
      res.end(`<html><body>Redirecting to <a href="${location}">${location}</a></body></html>`);
      return;
    }
    res.writeHead(404);
    res.end();
  }

  private handleWsConnection(devtoolsWs: WebSocket): void {
    if (!this.chromeWs || this.chromeWs.readyState !== WebSocket.OPEN) {
      devtoolsWs.close();
      return;
    }

    const attachId = nextAttachId--;
    const attachMsg = JSON.stringify({
      id: attachId,
      method: 'Target.attachToTarget',
      params: { targetId: this.options.targetId, flatten: true },
    });

    // Track the session ID once attach completes; closed by close/error handlers
    // that are registered immediately (before the async attach resolves) so
    // early disconnects still trigger cleanup.
    let sessionId: string | null = null;

    devtoolsWs.on('close', () => {
      if (sessionId) {
        this.sessions.delete(sessionId);
        this.detachSession(sessionId);
      }
    });

    devtoolsWs.on('error', () => {
      if (sessionId) {
        this.sessions.delete(sessionId);
        this.detachSession(sessionId);
      }
      devtoolsWs.close();
    });

    const messageBuffer: string[] = [];

    devtoolsWs.on('message', (data) => {
      if (!this.chromeWs || this.chromeWs.readyState !== WebSocket.OPEN) return;
      const text = String(data);
      if (!sessionId) {
        messageBuffer.push(text);
        return;
      }
      try {
        this.chromeWs.send(injectSessionId(text, sessionId));
      } catch (err) {
        console.error('[inspect] DevTools message forwarding error:', err);
      }
    });

    const attachPromise = new Promise<string | null>((resolve) => {
      this.pendingAttaches.set(attachId, resolve);
      this.chromeWs!.send(attachMsg);
      setTimeout(() => {
        if (this.pendingAttaches.has(attachId)) {
          this.pendingAttaches.delete(attachId);
          resolve(null);
        }
      }, 5000);
    });

    attachPromise.then((sid) => {
      if (!sid) {
        console.error('[inspect] Failed to attach to target');
        devtoolsWs.close();
        return;
      }

      if (devtoolsWs.readyState !== WebSocket.OPEN) {
        this.detachSession(sid);
        return;
      }

      sessionId = sid;
      this.sessions.set(sid, devtoolsWs);

      for (const buffered of messageBuffer) {
        try {
          this.chromeWs!.send(injectSessionId(buffered, sid));
        } catch (err) {
          console.error('[inspect] DevTools message forwarding error:', err);
        }
      }
      messageBuffer.length = 0;
    });
  }

  private detachSession(sessionId: string): void {
    if (!this.chromeWs || this.chromeWs.readyState !== WebSocket.OPEN) return;
    const detachId = nextAttachId--;
    const detachMsg = JSON.stringify({
      id: detachId,
      method: 'Target.detachFromTarget',
      params: { sessionId },
    });
    try {
      this.chromeWs.send(detachMsg);
    } catch (err) {
      console.error('[inspect] Failed to detach session:', err);
    }
  }
}
