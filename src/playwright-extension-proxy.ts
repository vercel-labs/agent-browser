import { WebSocketServer, WebSocket } from 'ws';
import { EventEmitter } from 'events';
import { createServer, Server } from 'http';

interface ExtensionMessage {
  method: string;
  id?: number;
  params?: any;
  result?: any;
  error?: any;
}

export interface ProxyInfo {
  relayUrl: string;
  cdpUrl: string;
}

/**
 * CDP Proxy for Playwright MCP Extension.
 * Translates between standard CDP and the extension's JSON-envelope protocol.
 */
export class PlaywrightExtensionProxy extends EventEmitter {
  private relayServer: WebSocketServer;
  private cdpServer: WebSocketServer;
  private relayPort: number = 0;
  private cdpPort: number = 0;
  private relaySocket: WebSocket | null = null;
  private cdpSocket: WebSocket | null = null;
  private targetInfo: any = null;
  private sessionId: string | null = null;
  private pendingCommands = new Map<number, (response: any) => void>();
  private nextId = 1;

  constructor() {
    super();
    // Start relay server (for extension to connect)
    this.relayServer = new WebSocketServer({ port: 0 });
    this.relayServer.on('connection', (ws) => this.onRelayConnection(ws));

    // Start CDP server (for Playwright to connect)
    this.cdpServer = new WebSocketServer({ port: 0 });
    this.cdpServer.on('connection', (ws) => this.onCdpConnection(ws));
  }

  async start(): Promise<ProxyInfo> {
    return new Promise((resolve, reject) => {
      let relayReady = false;
      let cdpReady = false;

      this.relayServer.on('listening', () => {
        const addr = this.relayServer.address();
        if (typeof addr === 'object' && addr) {
          this.relayPort = addr.port;
          relayReady = true;
          if (cdpReady) resolve(this.getInfo());
        }
      });

      this.cdpServer.on('listening', () => {
        const addr = this.cdpServer.address();
        if (typeof addr === 'object' && addr) {
          this.cdpPort = addr.port;
          cdpReady = true;
          if (relayReady) resolve(this.getInfo());
        }
      });

      this.relayServer.on('error', reject);
      this.cdpServer.on('error', reject);
    });
  }

  getInfo(): ProxyInfo {
    return {
      relayUrl: `ws://127.0.0.1:${this.relayPort}`,
      cdpUrl: `ws://127.0.0.1:${this.cdpPort}`,
    };
  }

  private async onRelayConnection(ws: WebSocket) {
    if (this.relaySocket) {
      ws.close(1013, 'Only one extension connection allowed');
      return;
    }

    this.relaySocket = ws;
    ws.on('message', (data) => this.handleRelayMessage(data.toString()));
    ws.on('close', () => {
      this.relaySocket = null;
      this.emit('extension-disconnected');
    });

    // Initiate handshake
    try {
      const response = await this.sendExtensionCommand('attachToTab', {});
      this.targetInfo = response.targetInfo;
      this.sessionId = response.sessionId || null;
      this.emit('extension-ready', this.targetInfo);
    } catch (err) {
      console.error('Handshake failed:', err);
      ws.close();
    }
  }

  private onCdpConnection(ws: WebSocket) {
    if (this.cdpSocket) {
      ws.close(1013, 'Only one Playwright connection allowed');
      return;
    }

    this.cdpSocket = ws;
    ws.on('message', (data) => this.handleCdpMessage(data.toString()));
    ws.on('close', () => {
      this.cdpSocket = null;
      this.emit('playwright-disconnected');
    });
  }

  private async handleRelayMessage(data: string) {
    try {
      const msg: ExtensionMessage = JSON.parse(data);

      // Handle command responses
      if (msg.id !== undefined && this.pendingCommands.has(msg.id)) {
        const resolve = this.pendingCommands.get(msg.id)!;
        this.pendingCommands.delete(msg.id);
        resolve(msg);
        return;
      }

      // Handle events
      if (msg.method === 'forwardCDPEvent') {
        if (this.cdpSocket && this.cdpSocket.readyState === WebSocket.OPEN) {
          const cdpEvent = {
            method: msg.params.method,
            params: msg.params.params,
          };
          this.cdpSocket.send(JSON.stringify(cdpEvent));
        }
        return;
      }
    } catch (err) {
      console.error('Error handling relay message:', err);
    }
  }

  private async handleCdpMessage(data: string) {
    try {
      const msg = JSON.parse(data);
      const { id, method, params } = msg;

      // Intercept Target.getTargetInfo
      if (method === 'Target.getTargetInfo' && this.targetInfo) {
        this.cdpSocket?.send(
          JSON.stringify({
            id,
            result: { targetInfo: this.targetInfo },
          })
        );
        return;
      }

      // Intercept Target.getTargets
      if (method === 'Target.getTargets' && this.targetInfo) {
        this.cdpSocket?.send(
          JSON.stringify({
            id,
            result: { targetInfos: [this.targetInfo] },
          })
        );
        return;
      }

      // Intercept Target.setAutoAttach (Playwright calls this)
      if (method === 'Target.setAutoAttach') {
        this.cdpSocket?.send(JSON.stringify({ id, result: {} }));
        return;
      }

      // Intercept Browser.getVersion
      if (method === 'Browser.getVersion') {
        this.cdpSocket?.send(
          JSON.stringify({
            id,
            result: {
              protocolVersion: '1.3',
              product: 'Chrome/PlaywrightExtension',
              revision: '1.0',
              userAgent: 'Mozilla/5.0 (PlaywrightExtension)',
              jsVersion: '1.0',
            },
          })
        );
        return;
      }

      // Reject Target.createTarget (multi-tab not supported)
      if (method === 'Target.createTarget') {
        this.cdpSocket?.send(
          JSON.stringify({
            id,
            error: {
              message:
                'Multi-tab operations are not supported by the Playwright Extension provider',
            },
          })
        );
        return;
      }

      // Forward to extension
      if (!this.relaySocket || this.relaySocket.readyState !== WebSocket.OPEN) {
        this.cdpSocket?.send(
          JSON.stringify({
            id,
            error: { message: 'Extension not connected' },
          })
        );
        return;
      }

      const response = await this.sendExtensionCommand('forwardCDPCommand', {
        sessionId: this.sessionId,
        method,
        params,
      });

      if (this.cdpSocket && this.cdpSocket.readyState === WebSocket.OPEN) {
        if (response.error) {
          this.cdpSocket.send(JSON.stringify({ id, error: response.error }));
        } else {
          // Extension message 'result' field IS the CDP result
          this.cdpSocket.send(JSON.stringify({ id, result: response }));
        }
      }
    } catch (err) {
      console.error('Error handling CDP message:', err);
    }
  }

  private sendExtensionCommand(method: string, params: any): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!this.relaySocket || this.relaySocket.readyState !== WebSocket.OPEN) {
        return reject(new Error('Extension not connected'));
      }

      const id = this.nextId++;
      const msg = { method, id, params };

      this.pendingCommands.set(id, (resp: ExtensionMessage) => {
        if (resp.error) {
          reject(resp.error);
        } else {
          resolve(resp.result);
        }
      });

      this.relaySocket.send(JSON.stringify(msg));
    });
  }

  async stop() {
    this.relayServer.close();
    this.cdpServer.close();
    if (this.relaySocket) this.relaySocket.close();
    if (this.cdpSocket) this.cdpSocket.close();
    this.pendingCommands.clear();
  }
}
