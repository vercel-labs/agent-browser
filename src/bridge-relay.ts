import { createServer, type Server as HttpServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { WebSocketServer, WebSocket } from 'ws';

export interface CdpMessage {
  id?: number;
  method?: string;
  params?: any;
  sessionId?: string;
  result?: any;
  error?: { message?: string } | string;
}

export interface RelayMessage {
  id?: number;
  method?: string;
  params?: any;
  result?: any;
  error?: { message?: string } | string;
}

interface RelayCommandCallback {
  resolve: (value: any) => void;
  reject: (error: Error) => void;
}

export interface BridgeCdpShimOptions {
  sendToCdp: (message: Record<string, unknown>) => void;
  sendToRelay: (message: Record<string, unknown>) => boolean;
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
}

function createDeferred<T>(): Deferred<T> {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/**
 * Translates between Playwright CDP messages and Playwright MCP Bridge relay messages.
 */
export class BridgeCdpShim {
  private readonly sendToCdp: (message: Record<string, unknown>) => void;
  private readonly sendToRelay: (message: Record<string, unknown>) => boolean;
  private relayCallbacks = new Map<number, RelayCommandCallback>();
  private relayMessageId = 0;
  private nextSessionId = 1;
  private connectedTabInfo:
    | {
        targetInfo: Record<string, unknown>;
        sessionId: string;
      }
    | undefined;

  constructor(options: BridgeCdpShimOptions) {
    this.sendToCdp = options.sendToCdp;
    this.sendToRelay = options.sendToRelay;
  }

  async handleCdpMessage(message: CdpMessage): Promise<void> {
    if (typeof message.id !== 'number' || !message.method) return;

    const { id, method, params, sessionId } = message;
    try {
      const result = await this.handleCdpCommand(method, params, sessionId);
      const response: Record<string, unknown> = { id, result };
      if (sessionId) response.sessionId = sessionId;
      this.sendToCdp(response);
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      const response: Record<string, unknown> = {
        id,
        error: { message: err.message },
      };
      if (sessionId) response.sessionId = sessionId;
      this.sendToCdp(response);
    }
  }

  async handleRelayMessage(message: RelayMessage): Promise<void> {
    if (typeof message.id === 'number') {
      const callback = this.relayCallbacks.get(message.id);
      if (!callback) return;

      this.relayCallbacks.delete(message.id);
      if (message.error) {
        callback.reject(this.toError(message.error));
      } else {
        callback.resolve(message.result);
      }
      return;
    }

    if (message.method === 'forwardCDPEvent') {
      const params = (message.params ?? {}) as {
        sessionId?: string;
        method?: string;
        params?: Record<string, unknown>;
      };
      if (!params.method) return;
      this.sendToCdp({
        sessionId: params.sessionId ?? this.connectedTabInfo?.sessionId,
        method: params.method,
        params: params.params ?? {},
      });
    }
  }

  private async handleCdpCommand(
    method: string,
    params: any,
    sessionId?: string
  ): Promise<Record<string, unknown>> {
    switch (method) {
      case 'Browser.getVersion':
        return {
          protocolVersion: '1.3',
          product: 'Chrome/Extension-Bridge',
          userAgent: 'CDP-Bridge-Server/1.0.0',
        };
      case 'Browser.setDownloadBehavior':
        return {};
      case 'Target.setAutoAttach': {
        if (sessionId) break;
        const result = (await this.sendRelayCommand('attachToTab', {})) as {
          targetInfo?: Record<string, unknown>;
        };
        const targetInfo = result?.targetInfo;
        if (!targetInfo) {
          throw new Error('attachToTab did not return targetInfo');
        }
        this.connectedTabInfo = {
          targetInfo,
          sessionId: `pw-tab-${this.nextSessionId++}`,
        };
        this.sendToCdp({
          method: 'Target.attachedToTarget',
          params: {
            sessionId: this.connectedTabInfo.sessionId,
            targetInfo: {
              ...targetInfo,
              attached: true,
            },
            waitingForDebugger: false,
          },
        });
        return {};
      }
      case 'Target.getTargetInfo':
        return this.connectedTabInfo?.targetInfo
          ? { targetInfo: this.connectedTabInfo.targetInfo }
          : {};
      default:
        break;
    }

    let mappedSessionId = sessionId;
    if (this.connectedTabInfo?.sessionId === mappedSessionId) {
      mappedSessionId = undefined;
    }

    const relayResult = await this.sendRelayCommand('forwardCDPCommand', {
      sessionId: mappedSessionId,
      method,
      params,
    });
    return relayResult ?? {};
  }

  private sendRelayCommand(method: string, params: any): Promise<any> {
    this.relayMessageId += 1;
    const id = this.relayMessageId;

    return new Promise((resolve, reject) => {
      this.relayCallbacks.set(id, { resolve, reject });
      let sent = false;
      try {
        sent = this.sendToRelay({ id, method, params });
      } catch {
        sent = false;
      }
      if (!sent) {
        this.relayCallbacks.delete(id);
        reject(new Error('Bridge extension is not connected'));
      }
    });
  }

  notifyRelayDisconnected(reason: string = 'Bridge extension disconnected'): void {
    if (this.relayCallbacks.size === 0) return;
    for (const callback of this.relayCallbacks.values()) {
      callback.reject(new Error(reason));
    }
    this.relayCallbacks.clear();
  }

  private toError(error: RelayMessage['error']): Error {
    if (!error) return new Error('Unknown relay error');
    if (typeof error === 'string') return new Error(error);
    return new Error(error.message ?? 'Unknown relay error');
  }
}

export interface BridgeRelayServerOptions {
  port: number;
  host?: string;
}

export interface BridgeConnectUrlOptions {
  extensionId: string;
  token?: string;
  clientName: string;
  clientVersion: string;
  protocolVersion?: number;
}

export class BridgeRelayServer {
  private readonly port: number;
  private readonly host: string;
  private readonly cdpPath: string;
  private readonly extensionPath: string;
  private httpServer: HttpServer | null = null;
  private wsServer: WebSocketServer | null = null;
  private cdpSocket: WebSocket | null = null;
  private extensionSocket: WebSocket | null = null;
  private extensionReadyDeferred = createDeferred<void>();
  private readonly shim: BridgeCdpShim;
  private started = false;

  constructor(options: BridgeRelayServerOptions) {
    this.port = options.port;
    this.host = options.host ?? '127.0.0.1';
    const uuid = randomUUID();
    this.cdpPath = `/cdp/${uuid}`;
    this.extensionPath = `/extension/${uuid}`;

    this.shim = new BridgeCdpShim({
      sendToCdp: (message) => {
        if (this.cdpSocket?.readyState === WebSocket.OPEN) {
          this.cdpSocket.send(JSON.stringify(message));
        }
      },
      sendToRelay: (message) => {
        if (this.extensionSocket?.readyState !== WebSocket.OPEN) return false;
        try {
          this.extensionSocket.send(JSON.stringify(message));
          return true;
        } catch {
          return false;
        }
      },
    });
  }

  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;

    this.httpServer = createServer();
    this.wsServer = new WebSocketServer({ server: this.httpServer });
    this.wsServer.on('connection', (socket, request) => {
      const rawPath = (request.url ?? '').split('?')[0] ?? '';
      if (rawPath === this.cdpPath) {
        this.handleCdpConnection(socket);
        return;
      }
      if (rawPath === this.extensionPath) {
        this.handleExtensionConnection(socket);
        return;
      }
      socket.close(1008, 'Invalid path');
    });

    await new Promise<void>((resolve, reject) => {
      this.httpServer!.once('error', reject);
      this.httpServer!.listen(this.port, this.host, () => {
        this.httpServer!.off('error', reject);
        resolve();
      });
    });
  }

  cdpEndpoint(): string {
    return `ws://${this.host}:${this.port}${this.cdpPath}`;
  }

  extensionEndpoint(): string {
    return `ws://${this.host}:${this.port}${this.extensionPath}`;
  }

  buildConnectUrl(options: BridgeConnectUrlOptions): string {
    const url = new URL(`chrome-extension://${options.extensionId}/connect.html`);
    url.searchParams.set('mcpRelayUrl', this.extensionEndpoint());
    url.searchParams.set(
      'client',
      JSON.stringify({
        name: options.clientName,
        version: options.clientVersion,
      })
    );
    url.searchParams.set('protocolVersion', String(options.protocolVersion ?? 1));
    if (options.token) {
      url.searchParams.set('token', options.token);
    }
    return url.toString();
  }

  async waitForExtensionConnection(timeoutMs: number): Promise<void> {
    await Promise.race([
      this.extensionReadyDeferred.promise,
      new Promise<void>((_, reject) =>
        setTimeout(() => reject(new Error('Extension connection timeout')), timeoutMs)
      ),
    ]);
  }

  async stop(reason: string = 'Bridge relay stopped'): Promise<void> {
    this.shim.notifyRelayDisconnected(reason);
    this.extensionReadyDeferred.reject(new Error(reason));
    this.extensionReadyDeferred.promise.catch(() => {});
    const sockets = [this.cdpSocket, this.extensionSocket];
    for (const socket of sockets) {
      if (!socket) continue;
      if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING) {
        socket.close(1000, reason);
      }
    }
    this.cdpSocket = null;
    this.extensionSocket = null;

    if (this.wsServer) {
      await new Promise<void>((resolve) => this.wsServer!.close(() => resolve()));
      this.wsServer = null;
    }

    if (this.httpServer) {
      await new Promise<void>((resolve) => this.httpServer!.close(() => resolve()));
      this.httpServer = null;
    }

    this.started = false;
  }

  private handleCdpConnection(socket: WebSocket): void {
    if (this.cdpSocket) {
      socket.close(1000, 'Another CDP client already connected');
      return;
    }
    this.cdpSocket = socket;
    socket.on('message', async (data) => {
      try {
        const message = JSON.parse(data.toString()) as CdpMessage;
        await this.shim.handleCdpMessage(message);
      } catch {
        socket.close(1002, 'Invalid CDP message');
      }
    });
    socket.on('close', () => {
      if (this.cdpSocket === socket) this.cdpSocket = null;
    });
  }

  private handleExtensionConnection(socket: WebSocket): void {
    if (this.extensionSocket) {
      socket.close(1000, 'Another extension connection already established');
      return;
    }

    this.extensionSocket = socket;
    this.extensionReadyDeferred.resolve();

    socket.on('message', async (data) => {
      try {
        const message = JSON.parse(data.toString()) as RelayMessage;
        await this.shim.handleRelayMessage(message);
      } catch {
        socket.close(1002, 'Invalid relay message');
      }
    });
    socket.on('close', () => {
      if (this.extensionSocket === socket) {
        this.shim.notifyRelayDisconnected('Bridge extension disconnected');
        this.extensionSocket = null;
        this.extensionReadyDeferred = createDeferred<void>();
      }
    });
  }
}
