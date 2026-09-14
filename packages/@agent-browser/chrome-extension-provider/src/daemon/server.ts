import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { Duplex } from "node:stream";
import { WebSocket, WebSocketServer } from "ws";
import type { BridgeTab, CdpRequest, LeaseResult, Params, Scope, ScopeStatus, SetupResult } from "../protocol.js";
import { autoAttachParams, parseRequest, ProtocolError, record, scopedCookiesResult, scopedPageParams, targetInfoFor, validTab } from "./cdp.js";

const PAIRING_MS = 5 * 60_000;
const RESERVATION_MS = 60_000;
const COMMAND_MS = 30_000;
const HEARTBEAT_MS = 15_000;
const token = () => randomBytes(32).toString("base64url");
const scopeKey = (scope: Scope) => JSON.stringify([scope.namespace, scope.session]);
type Child = { sessionId: string; nativeSessionId: string; parent?: string; frameId?: string; info: Params };
type Lease = {
  id: string; token: string; scope: Connection; timer: NodeJS.Timeout; client?: WebSocket;
  ready?: Promise<unknown>; rootSession: string; targetId: string; frameId?: string; attached: boolean;
  discovery: boolean; children: Map<string, Child>; autoAttach: Map<string, Params>; requests: Set<number>;
};
type Connection = {
  scope: Scope; requestId: string; pairingToken: string; expires: number; timer: NodeJS.Timeout;
  extension?: WebSocket; originId?: string; profileId?: string; chromeVersion?: string;
  tab?: BridgeTab; lease?: Lease;
};
type Pending = {
  scope: Connection; lease: Lease; resolve: (result: unknown) => void; reject: (error: Error) => void;
  timer: NodeJS.Timeout; sessionId?: string;
};

/** Authenticated loopback relay. Each lease exposes exactly one user-approved page. */
export class BridgeDaemon {
  private server?: Server;
  private wss = new WebSocketServer({ noServer: true, maxPayload: 32 * 1024 * 1024 });
  private port = 0;
  private scopes = new Map<string, Connection>();
  private pairings = new Map<string, Connection>();
  private leases = new Map<string, Lease>();
  private leaseTokens = new Map<string, Lease>();
  private claims = new Map<string, Connection>();
  private pending = new Map<string, Pending>();
  private alive = new Map<WebSocket, boolean>();
  private heartbeat?: NodeJS.Timeout;

  constructor(private readonly options: { port?: number; controlToken: string }) {
    if (options.controlToken.length < 32) throw new Error("A high-entropy control token is required");
  }

  async start(): Promise<number> {
    if (this.server) return this.port;
    const server = createServer((req, res) => void this.http(req, res));
    this.server = server;
    server.requestTimeout = 10_000;
    server.headersTimeout = 10_000;
    server.keepAliveTimeout = 1000;
    server.setTimeout(10_000, socket => socket.destroy());
    server.on("upgrade", (req, socket, head) => this.upgrade(req, socket, head));
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(this.options.port ?? 0, "127.0.0.1", () => {
        server.removeListener("error", reject);
        resolve();
      });
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Relay did not bind a loopback port");
    this.port = address.port;
    this.heartbeat = setInterval(() => {
      for (const [ws, alive] of this.alive) {
        if (!alive) ws.terminate();
        else { this.alive.set(ws, false); ws.ping(); }
      }
    }, HEARTBEAT_MS);
    this.heartbeat.unref();
    return this.port;
  }

  async stop(): Promise<void> {
    clearInterval(this.heartbeat);
    for (const scope of [...this.scopes.values()]) this.forget(scope, "Relay stopped");
    for (const ws of this.wss.clients) ws.terminate();
    await new Promise<void>(resolve => this.wss.close(() => resolve()));
    if (this.server) {
      this.server.closeAllConnections();
      await new Promise<void>((resolve, reject) => this.server!.close(error => error ? reject(error) : resolve()));
      this.server = undefined;
    }
  }

  private hostAllowed(req: IncomingMessage): boolean { return req.headers.host === `127.0.0.1:${this.port}`; }
  private authenticated(req: IncomingMessage): boolean {
    const received = Buffer.from(req.headers.authorization ?? "");
    const expected = Buffer.from(`Bearer ${this.options.controlToken}`);
    return received.length === expected.length && timingSafeEqual(received, expected);
  }
  private json(res: ServerResponse, status: number, body: unknown): void {
    if (res.destroyed) return;
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(JSON.stringify(body));
  }
  private async body(req: IncomingMessage): Promise<Params> {
    const chunks: Buffer[] = [];
    let size = 0;
    for await (const chunk of req) {
      size += Buffer.byteLength(chunk);
      if (size > 16_384) throw new ProtocolError("Request body is too large", 413);
      chunks.push(Buffer.from(chunk));
    }
    try {
      const value: unknown = JSON.parse(Buffer.concat(chunks).toString());
      if (!record(value)) throw new Error();
      return value;
    } catch { throw new ProtocolError("Expected a JSON object", 400); }
  }
  private scope(value: Params): Scope {
    if (typeof value.namespace !== "string" || typeof value.session !== "string" || !value.session ||
        value.namespace.length > 200 || value.session.length > 200 || /[\u0000-\u001f\u007f]/.test(value.namespace + value.session)) {
      throw new ProtocolError("Provide a valid namespace and session", 400);
    }
    return { namespace: value.namespace, session: value.session };
  }

  private async http(req: IncomingMessage, res: ServerResponse): Promise<void> {
    if (!this.hostAllowed(req) || req.headers.origin !== undefined) return this.json(res, 403, { error: "Local control requests only" });
    if (!this.authenticated(req)) return this.json(res, 401, { error: "Control authentication required" });
    try {
      const url = new URL(req.url ?? "/", "http://127.0.0.1");
      if (req.method === "GET" && url.pathname === "/health") return this.json(res, 200, { ok: true, protocolVersion: 1 });
      if (req.method === "POST" && url.pathname === "/setup") {
        const scope = this.scope(await this.body(req));
        const previous = this.scopes.get(scopeKey(scope));
        if (previous?.lease) throw new ProtocolError("Release the current lease before pairing again", 409);
        if (previous) this.forget(previous, "Pairing replaced");
        const pairingToken = token();
        const expires = Date.now() + PAIRING_MS;
        const connection: Connection = { scope, requestId: token(), pairingToken, expires,
          timer: setTimeout(() => this.forget(connection, "Pairing expired"), PAIRING_MS) };
        connection.timer.unref();
        this.scopes.set(scopeKey(scope), connection);
        this.pairings.set(pairingToken, connection);
        const result: SetupResult = { requestId: connection.requestId, pairingCode: `${this.port}:${pairingToken}`, expiresAt: new Date(expires).toISOString() };
        return this.json(res, 200, result);
      }
      if (req.method === "GET" && url.pathname === "/status") {
        const scope = this.scope({ namespace: url.searchParams.get("namespace"), session: url.searchParams.get("session") });
        const connection = this.scopes.get(scopeKey(scope));
        const result: ScopeStatus = { scope, state: !connection?.profileId ? "not-paired" : connection.lease?.client ? "connected" :
          connection.lease ? "reserved" : connection.tab ? "authorized" : "awaiting-approval", ...(connection?.tab ? { tab: connection.tab } : {}) };
        return this.json(res, 200, result);
      }
      if (req.method === "POST" && url.pathname === "/lease") {
        const scope = this.scope(await this.body(req));
        const connection = this.scopes.get(scopeKey(scope));
        if (!connection?.tab || connection.extension?.readyState !== WebSocket.OPEN) throw new ProtocolError("Authorize a tab in the extension first", 409);
        if (connection.lease) throw new ProtocolError("This session already has a lease", 409);
        const lease: Lease = { id: token(), token: token(), scope: connection, rootSession: `page-session:${token()}`,
          targetId: `page:${token()}`, attached: false, discovery: false, children: new Map(), autoAttach: new Map(), requests: new Set(),
          timer: setTimeout(() => this.release(lease, "Unused lease expired"), RESERVATION_MS) };
        lease.timer.unref();
        connection.lease = lease;
        this.leases.set(lease.id, lease);
        this.leaseTokens.set(lease.token, lease);
        const result: LeaseResult = { leaseId: lease.id, cdpUrl: `ws://127.0.0.1:${this.port}/cdp/${lease.token}` };
        return this.json(res, 200, result);
      }
      if (req.method === "DELETE" && /^\/lease\/[A-Za-z0-9_-]+$/.test(url.pathname)) {
        const lease = this.leases.get(url.pathname.slice("/lease/".length));
        if (lease) this.release(lease, "Control released");
        return this.json(res, 200, { released: true });
      }
      this.json(res, 404, { error: "Unknown control route" });
    } catch (error) {
      this.json(res, error instanceof ProtocolError && error.code > 0 ? error.code : 400,
        { error: error instanceof ProtocolError ? error.message : "Invalid control request" });
    }
  }

  private upgrade(req: IncomingMessage, socket: Duplex, head: Buffer): void {
    const reject = (status: number) => socket.end(`HTTP/1.1 ${status} Rejected\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    if (!this.hostAllowed(req)) { reject(403); return; }
    let url: URL;
    try { url = new URL(req.url ?? "/", "http://127.0.0.1"); }
    catch { reject(400); return; }
    if (url.pathname === "/extension") {
      const scope = this.pairings.get(url.searchParams.get("token") ?? "");
      const match = /^chrome-extension:\/\/([a-p]{32})$/.exec(req.headers.origin ?? "");
      if (!scope || !match || scope.expires <= Date.now() || scope.extension) { reject(403); return; }
      this.pairings.delete(scope.pairingToken);
      this.wss.handleUpgrade(req, socket, head, ws => {
        scope.extension = ws;
        scope.originId = match[1];
        this.watch(ws);
        const helloTimeout = setTimeout(() => ws.terminate(), 5000);
        helloTimeout.unref();
        ws.on("message", raw => {
          try {
            const value: unknown = JSON.parse(raw.toString());
            if (!record(value) || value.v !== 1) throw new Error();
            this.extensionMessage(scope, value);
            if (scope.profileId) clearTimeout(helloTimeout);
          } catch {
            this.send(ws, { v: 1, kind: "error", message: "Invalid extension message" });
            this.forget(scope, "Extension protocol error");
          }
        });
        ws.on("close", () => { clearTimeout(helloTimeout); this.forget(scope, "Extension disconnected"); });
      });
      return;
    }
    const lease = this.leaseTokens.get(url.pathname.startsWith("/cdp/") ? url.pathname.slice(5) : "");
    if (!lease || req.headers.origin !== undefined || lease.client || !lease.scope.tab || lease.scope.extension?.readyState !== WebSocket.OPEN) {
      reject(403); return;
    }
    this.leaseTokens.delete(lease.token);
    this.wss.handleUpgrade(req, socket, head, ws => {
      clearTimeout(lease.timer);
      lease.client = ws;
      this.watch(ws);
      lease.ready = this.command(lease, "Bridge.attach", {}).then(() => { lease.attached = true; });
      void lease.ready.catch(() => this.release(lease, "Debugger attachment failed"));
      ws.on("message", raw => {
        let request: CdpRequest;
        try { request = parseRequest(raw.toString()); }
        catch { this.send(ws, { error: { code: -32600, message: "Invalid CDP request" } }); return; }
        if (lease.requests.has(request.id) || lease.requests.size >= 256) {
          this.reply(lease, request, undefined, new ProtocolError("Duplicate request ID or too many pending requests"));
          return;
        }
        lease.requests.add(request.id);
        void this.cdp(lease, request).then(result => this.reply(lease, request, result), error => this.reply(lease, request, undefined, error))
          .finally(() => lease.requests.delete(request.id));
      });
      ws.on("close", () => this.release(lease, "CDP client disconnected"));
    });
  }

  private watch(ws: WebSocket): void {
    this.alive.set(ws, true);
    ws.on("pong", () => this.alive.set(ws, true));
    ws.on("error", () => ws.terminate());
    ws.on("close", () => this.alive.delete(ws));
  }
  private send(ws: WebSocket | undefined, value: unknown): void {
    if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(value));
  }
  private claimKey(scope: Connection, tab: BridgeTab): string { return JSON.stringify([scope.profileId, tab.tabId]); }
  private extensionMessage(scope: Connection, message: Params): void {
    if (this.scopes.get(scopeKey(scope.scope)) !== scope) return;
    if (message.kind === "hello") {
      if (scope.profileId || message.extensionId !== scope.originId || typeof message.profileId !== "string" ||
          !/^[A-Za-z0-9_-]{16,128}$/.test(message.profileId) || typeof message.chromeVersion !== "string" || message.chromeVersion.length > 100) throw new Error();
      scope.profileId = message.profileId;
      scope.chromeVersion = message.chromeVersion;
      this.send(scope.extension, { v: 1, kind: "paired", requestId: scope.requestId, scope: scope.scope });
      return;
    }
    if (!scope.profileId) throw new Error();
    if (message.kind === "heartbeat") return;
    if (message.kind === "grant") {
      if (!validTab(message.tab) || scope.lease) throw new Error();
      const owner = this.claims.get(this.claimKey(scope, message.tab));
      if (owner && owner !== scope) {
        this.send(scope.extension, { v: 1, kind: "error", message: "This tab is already authorized to another session" });
        return;
      }
      if (scope.tab) this.claims.delete(this.claimKey(scope, scope.tab));
      scope.tab = message.tab;
      this.claims.set(this.claimKey(scope, scope.tab), scope);
      clearTimeout(scope.timer);
      this.send(scope.extension, { v: 1, kind: "granted" });
      return;
    }
    if (message.kind === "revoked") { this.revoke(scope, "Authorization revoked"); return; }
    if (message.kind === "tab-updated") {
      if (!validTab(message.tab) || message.tab.tabId !== scope.tab?.tabId) { this.revoke(scope, "Authorized tab is unavailable"); return; }
      scope.tab = message.tab;
      if (scope.lease?.discovery) this.event(scope.lease, "Target.targetInfoChanged", { targetInfo: this.pageInfo(scope.lease) });
      return;
    }
    if (message.kind === "cdp-result") {
      if (typeof message.reqId !== "string") throw new Error();
      const pending = this.pending.get(message.reqId);
      if (!pending || pending.scope !== scope) return;
      if (message.error !== undefined && (!record(message.error) || !Number.isInteger(message.error.code) || typeof message.error.message !== "string")) throw new Error();
      this.pending.delete(message.reqId);
      clearTimeout(pending.timer);
      if (message.error !== undefined) {
        const error = message.error as Params;
        pending.reject(new ProtocolError(String(error.message), Number(error.code)));
      } else pending.resolve(message.result ?? {});
      return;
    }
    if (message.kind === "cdp-event") {
      if (scope.lease && message.tabId === scope.tab?.tabId && typeof message.method === "string" && record(message.params) &&
          (message.sessionId === undefined || typeof message.sessionId === "string")) {
        this.forwardEvent(scope.lease, message.method, message.params, message.sessionId as string | undefined);
      }
      return;
    }
    throw new Error();
  }

  private command(lease: Lease, method: string, params: Params, sessionId?: string): Promise<unknown> {
    const scope = lease.scope;
    if (scope.lease !== lease || !scope.tab || scope.extension?.readyState !== WebSocket.OPEN) return Promise.reject(new ProtocolError("Lease is no longer authorized"));
    if (this.pending.size >= 1024) return Promise.reject(new ProtocolError("Too many pending commands"));
    const reqId = token();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(reqId);
        reject(new ProtocolError("Chrome command timed out"));
        this.release(lease, "Chrome command timed out");
      }, COMMAND_MS);
      timer.unref();
      this.pending.set(reqId, { scope, lease, resolve, reject, timer, sessionId });
      this.send(scope.extension, { v: 1, kind: "cdp-command", reqId, tabId: scope.tab!.tabId, method, params, ...(sessionId ? { sessionId } : {}) });
    });
  }
  private pageInfo(lease: Lease): Params { return targetInfoFor(lease.targetId, lease.scope.tab!, lease.attached); }
  private session(lease: Lease, id?: string): Child | undefined {
    if (!id || id === lease.rootSession) return undefined;
    const child = lease.children.get(id);
    if (!child) throw new ProtocolError("Session is outside the authorized page");
    return child;
  }
  private target(lease: Lease, id: unknown): Params {
    if (id === lease.targetId) return this.pageInfo(lease);
    const child = [...lease.children.values()].find(child => child.info.targetId === id);
    if (!child) throw new ProtocolError("Target is outside the authorized page");
    return child.info;
  }
  private async cdp(lease: Lease, request: CdpRequest): Promise<unknown> {
    await lease.ready;
    if (lease.scope.lease !== lease) throw new ProtocolError("Lease is no longer authorized");
    const child = this.session(lease, request.sessionId);
    const params = request.params ?? {};
    const method = request.method;
    if (params.browserContextId !== undefined) throw new ProtocolError("Browser contexts are outside the authorized page");
    if (method === "Browser.getVersion" && !request.sessionId) {
      return { protocolVersion: "1.3", product: `Chrome/${lease.scope.chromeVersion}`, revision: "", userAgent: "agent-browser Chrome extension", jsVersion: "" };
    }
    if (method === "Target.getTargets") return { targetInfos: [this.pageInfo(lease), ...[...lease.children.values()].map(child => child.info)] };
    if (method === "Target.getTargetInfo") return { targetInfo: this.target(lease, params.targetId ?? child?.info.targetId ?? lease.targetId) };
    if (method === "Target.setDiscoverTargets") {
      if (typeof params.discover !== "boolean") throw new ProtocolError("discover must be boolean", -32602);
      lease.discovery = params.discover;
      if (lease.discovery) for (const targetInfo of [this.pageInfo(lease), ...[...lease.children.values()].map(child => child.info)]) this.event(lease, "Target.targetCreated", { targetInfo });
      return {};
    }
    if (method === "Target.attachToTarget") {
      this.target(lease, params.targetId);
      if (params.flatten !== true) throw new ProtocolError("Only flat target sessions are supported");
      return { sessionId: params.targetId === lease.targetId ? lease.rootSession : [...lease.children.values()].find(child => child.info.targetId === params.targetId)!.sessionId };
    }
    if (method === "Target.activateTarget") {
      this.target(lease, params.targetId);
      if (params.targetId !== lease.targetId) throw new ProtocolError("Only the authorized page can be activated");
      return this.command(lease, "Bridge.activate", {});
    }
    if (method === "Target.detachFromTarget") {
      if (params.targetId !== undefined || typeof params.sessionId !== "string") throw new ProtocolError("Provide a scoped sessionId", -32602);
      const detached = this.session(lease, params.sessionId);
      if (!detached) throw new ProtocolError("Release the lease to detach the approved page");
      const parent = detached.parent ? this.session(lease, detached.parent) : undefined;
      const result = await this.command(lease, method, { sessionId: detached.nativeSessionId }, parent?.nativeSessionId);
      this.removeChild(lease, detached.sessionId);
      return result;
    }
    if (method === "Target.setAutoAttach") {
      const filtered = autoAttachParams(params);
      lease.autoAttach.set(request.sessionId ?? lease.rootSession, filtered);
      return this.command(lease, method, filtered, child?.nativeSessionId);
    }
    if (method.startsWith("Browser.") || method.startsWith("Target.")) {
      throw new ProtocolError("This browser operation is not supported for an authorized existing tab", -32601);
    }
    if (!request.sessionId) throw new ProtocolError("Attach to the authorized target before sending page commands");
    const topUrl = lease.scope.tab!.url;
    const scoped = scopedPageParams(method, params, String(child?.info.url ?? topUrl), topUrl);
    const result = await this.command(lease, method, scoped, child?.nativeSessionId);
    if (method === "Page.getFrameTree" && record(result) && record(result.frameTree) && record(result.frameTree.frame) && typeof result.frameTree.frame.id === "string") {
      (child ?? lease).frameId = result.frameTree.frame.id;
    }
    return method === "Network.getCookies" ? scopedCookiesResult(result, scoped, topUrl) : result;
  }
  private reply(lease: Lease, request: CdpRequest, result?: unknown, error?: unknown): void {
    const failure = error instanceof ProtocolError ? error.toJSON() : { code: -32000, message: "Chrome command failed" };
    this.send(lease.client, { id: request.id, ...(request.sessionId ? { sessionId: request.sessionId } : {}), ...(error ? { error: failure } : { result: result ?? {} }) });
  }
  private event(lease: Lease, method: string, params: Params, sessionId?: string): void {
    this.send(lease.client, { method, params, ...(sessionId ? { sessionId } : {}) });
  }
  private removeChild(lease: Lease, id: string): void {
    const child = lease.children.get(id);
    if (!child) return;
    for (const descendant of [...lease.children.values()]) if (descendant.parent === id) this.removeChild(lease, descendant.sessionId);
    lease.children.delete(id);
    lease.autoAttach.delete(id);
    for (const [reqId, pending] of this.pending) if (pending.lease === lease && pending.sessionId === child.nativeSessionId) {
      clearTimeout(pending.timer);
      this.pending.delete(reqId);
      pending.reject(new ProtocolError("Child target detached"));
    }
    this.event(lease, "Target.detachedFromTarget", { sessionId: id, targetId: child.info.targetId }, child.parent ?? lease.rootSession);
    if (lease.discovery) this.event(lease, "Target.targetDestroyed", { targetId: child.info.targetId });
  }
  private forwardEvent(lease: Lease, method: string, params: Params, nativeSessionId?: string): void {
    const parent = nativeSessionId ? [...lease.children.values()].find(child => child.nativeSessionId === nativeSessionId) : undefined;
    if (nativeSessionId && !parent) return;
    const sessionId = parent?.sessionId ?? lease.rootSession;
    if (method === "Target.attachedToTarget") {
      if (!record(params.targetInfo) || typeof params.sessionId !== "string" || typeof params.targetInfo.targetId !== "string") return;
      const info = params.targetInfo;
      if (!["iframe", "worker"].includes(String(info.type)) || typeof info.url !== "string" ||
          info.targetId === lease.targetId || [...lease.children.values()].some(child => child.nativeSessionId === params.sessionId || child.info.targetId === info.targetId)) {
        void this.command(lease, "Target.detachFromTarget", { sessionId: params.sessionId }, nativeSessionId).catch(() => {});
        return;
      }
      const id = `child-session:${token()}`;
      const child: Child = { sessionId: id, nativeSessionId: params.sessionId, parent: parent?.sessionId,
        info: { targetId: info.targetId, type: info.type, title: typeof info.title === "string" ? info.title : "", url: info.url, attached: true, canAccessOpener: false } };
      lease.children.set(id, child);
      this.event(lease, method, { sessionId: id, targetInfo: child.info, waitingForDebugger: params.waitingForDebugger === true }, sessionId);
      if (lease.discovery) this.event(lease, "Target.targetCreated", { targetInfo: child.info });
      const autoAttach = lease.autoAttach.get(sessionId);
      if (autoAttach?.autoAttach === true) {
        lease.autoAttach.set(id, autoAttach);
        void this.command(lease, "Target.setAutoAttach", autoAttach, child.nativeSessionId).catch(() => this.release(lease, "Child auto-attach failed"));
      }
      return;
    }
    if (method === "Target.detachedFromTarget") {
      const child = [...lease.children.values()].find(child => child.nativeSessionId === params.sessionId && child.parent === parent?.sessionId);
      if (child) this.removeChild(lease, child.sessionId);
      return;
    }
    if (method.startsWith("Target.")) {
      if (method === "Target.targetInfoChanged" && record(params.targetInfo)) {
        const child = [...lease.children.values()].find(child => child.info.targetId === (params.targetInfo as Params).targetId);
        if (child && typeof params.targetInfo.url === "string") {
          child.info = { ...child.info, url: params.targetInfo.url, title: typeof params.targetInfo.title === "string" ? params.targetInfo.title : "" };
          if (lease.discovery) this.event(lease, method, { targetInfo: child.info });
        }
      }
      return;
    }
    if (method === "Inspector.detached") { this.release(lease, "Debugger detached"); return; }
    // Commit navigation scope before forwarding the event, rather than waiting
    // for the extension's separate tabs.onUpdated notification.
    if (method === "Page.frameNavigated" && record(params.frame) && !params.frame.parentId && typeof params.frame.id === "string") {
      (parent ?? lease).frameId = params.frame.id;
    }
    const navigatedUrl = method === "Page.frameNavigated" && record(params.frame) && !params.frame.parentId
      ? params.frame.url : method === "Page.navigatedWithinDocument" && params.frameId === (parent ?? lease).frameId ? params.url : undefined;
    if (typeof navigatedUrl === "string") {
      if (parent) parent.info = { ...parent.info, url: navigatedUrl };
      else if (lease.scope.tab) lease.scope.tab = { ...lease.scope.tab, url: navigatedUrl };
    }
    this.event(lease, method, params, sessionId);
  }

  private release(lease: Lease, reason: string): void {
    if (lease.scope.lease !== lease) return;
    clearTimeout(lease.timer);
    this.leases.delete(lease.id);
    this.leaseTokens.delete(lease.token);
    for (const [reqId, pending] of this.pending) if (pending.lease === lease) {
      clearTimeout(pending.timer);
      this.pending.delete(reqId);
      pending.reject(new ProtocolError(reason));
    }
    const scope = lease.scope;
    // Release is authoritative even while Bridge.attach is still awaiting Chrome.
    if (scope.tab) this.send(scope.extension, { v: 1, kind: "cdp-command", reqId: token(), method: "Bridge.detach", params: {}, tabId: scope.tab.tabId });
    scope.lease = undefined;
    lease.children.clear();
    this.revoke(scope, reason);
    lease.client?.close(1000, "Control released");
    const closeTimer = setTimeout(() => lease.client?.terminate(), 1000);
    closeTimer.unref();
  }
  private revoke(scope: Connection, reason: string): void {
    if (scope.lease) { this.release(scope.lease, reason); return; }
    if (scope.tab) this.claims.delete(this.claimKey(scope, scope.tab));
    scope.tab = undefined;
    this.send(scope.extension, { v: 1, kind: "released", reason });
  }
  private forget(scope: Connection, reason: string): void {
    if (this.scopes.get(scopeKey(scope.scope)) !== scope) return;
    this.revoke(scope, reason);
    clearTimeout(scope.timer);
    this.pairings.delete(scope.pairingToken);
    this.scopes.delete(scopeKey(scope.scope));
    scope.extension?.close(1000, "Pairing ended");
    const closeTimer = setTimeout(() => scope.extension?.terminate(), 1000);
    closeTimer.unref();
  }
}
