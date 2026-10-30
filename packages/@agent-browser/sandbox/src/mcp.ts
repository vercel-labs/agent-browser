export const AGENT_BROWSER_MCP_PROTOCOL_VERSION = "2025-11-25";

/** A running `agent-browser mcp` process: newline-delimited JSON-RPC over stdin and stdout. */
export interface AgentBrowserMcpProcess {
  readonly stdin: WritableStream<Uint8Array>;
  readonly stdout: ReadableStream<Uint8Array>;
  wait(): PromiseLike<{ readonly exitCode: number }>;
}

export interface AgentBrowserMcpRequestOptions {
  readonly signal?: AbortSignal;
  /** Milliseconds before the request rejects. Defaults to the client's `timeout`. */
  readonly timeout?: number;
}

export interface ConnectAgentBrowserMcpOptions {
  readonly clientInfo?: { readonly name: string; readonly version: string };
  /** Milliseconds before each request, and `close()`, rejects. No timeout when omitted. */
  readonly timeout?: number;
}

export interface AgentBrowserMcpToolResult {
  readonly content: readonly { readonly type: string; readonly text?: string }[];
  readonly isError?: boolean;
  readonly structuredContent?: Record<string, unknown>;
}

export interface AgentBrowserMcpClient {
  readonly serverInfo: { readonly name: string; readonly version: string };
  request<T = unknown>(
    method: string,
    params?: Record<string, unknown>,
    options?: AgentBrowserMcpRequestOptions,
  ): Promise<T>;
  callTool(
    name: string,
    args?: Record<string, unknown>,
    options?: AgentBrowserMcpRequestOptions,
  ): Promise<AgentBrowserMcpToolResult>;
  /** Sends EOF and waits for the server to exit, within the client's `timeout`. */
  close(): Promise<{ readonly exitCode: number }>;
}

export class AgentBrowserMcpError extends Error {
  readonly code: number;
  readonly data: unknown;

  constructor(error: { readonly code: number; readonly message: string; readonly data?: unknown }) {
    super(`agent-browser mcp error ${error.code}: ${error.message}`);
    this.name = "AgentBrowserMcpError";
    this.code = error.code;
    this.data = error.data;
  }
}

interface Pending {
  readonly reject: (error: unknown) => void;
  readonly resolve: (value: unknown) => void;
}

/**
 * Speaks MCP to an `agent-browser mcp` process the caller started, and runs
 * the initialize handshake. Requests may run concurrently; writes reach stdin
 * in call order.
 */
export async function connectAgentBrowserMcp(
  process: AgentBrowserMcpProcess,
  options: ConnectAgentBrowserMcpOptions = {},
): Promise<AgentBrowserMcpClient> {
  const encoder = new TextEncoder();
  const writer = process.stdin.getWriter();
  const reader = process.stdout.getReader();
  const pending = new Map<number, Pending>();
  let nextId = 0;
  let closed: Error | undefined;

  const failAll = (error: Error) => {
    closed ??= error;
    for (const entry of [...pending.values()]) entry.reject(closed);
  };

  const reading = (async () => {
    for await (const line of readLines(reader)) {
      const message = parseMessage(line);
      if (message === undefined || typeof message.id !== "number") continue;
      const entry = pending.get(message.id);
      if (entry === undefined) continue;
      if (message.error !== undefined) entry.reject(new AgentBrowserMcpError(message.error));
      else entry.resolve(message.result);
    }
    failAll(new Error("agent-browser mcp closed its output."));
  })();
  reading.catch((error: unknown) => failAll(toError(error)));

  const encode = (message: Record<string, unknown>) =>
    encoder.encode(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);

  function request<T>(
    method: string,
    params: Record<string, unknown> = {},
    requestOptions: AgentBrowserMcpRequestOptions = {},
  ): Promise<T> {
    if (closed !== undefined) return Promise.reject(closed);
    if (requestOptions.signal?.aborted) {
      return Promise.reject(requestOptions.signal.reason ?? new Error("Aborted."));
    }
    const id = ++nextId;
    let line: Uint8Array;
    try {
      line = encode({ id, method, params });
    } catch (error) {
      return Promise.reject(error);
    }
    const timeout = requestOptions.timeout ?? options.timeout;
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((onResolve, onReject) => {
      resolve = onResolve;
      reject = onReject;
    });
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onAbort = () => fail(requestOptions.signal?.reason ?? new Error("Aborted."));
    /** Returns false when the request already settled. */
    const finish = () => {
      if (!pending.delete(id)) return false;
      clearTimeout(timer);
      requestOptions.signal?.removeEventListener("abort", onAbort);
      return true;
    };
    const fail = (error: unknown) => {
      if (finish()) reject(error);
    };
    pending.set(id, {
      reject: fail,
      resolve: (value) => {
        if (finish()) resolve(value as T);
      },
    });
    if (timeout !== undefined) {
      timer = setTimeout(
        () => fail(new Error(`agent-browser mcp ${method} timed out after ${timeout}ms.`)),
        timeout,
      );
    }
    requestOptions.signal?.addEventListener("abort", onAbort, { once: true });
    writer.write(line).catch((error: unknown) => fail(toError(error)));
    return promise;
  }

  let initialized: { serverInfo: { name: string; version: string } };
  try {
    initialized = await request("initialize", {
      capabilities: {},
      clientInfo: options.clientInfo ?? { name: "@agent-browser/sandbox", version: "0" },
      protocolVersion: AGENT_BROWSER_MCP_PROTOCOL_VERSION,
    });
  } catch (error) {
    failAll(toError(error));
    // A stalled write would hold abort() open, so release the streams without waiting.
    void writer.abort(error).catch(() => undefined);
    void reader.cancel(error).catch(() => undefined);
    writer.releaseLock();
    reader.releaseLock();
    throw error;
  }
  // Later writes queue behind this one, so it needs no await.
  writer
    .write(encode({ method: "notifications/initialized" }))
    .catch((error: unknown) => failAll(toError(error)));

  return {
    serverInfo: initialized.serverInfo,
    request,
    callTool: (name, args = {}, requestOptions) =>
      request<AgentBrowserMcpToolResult>("tools/call", { arguments: args, name }, requestOptions),
    async close() {
      failAll(new Error("agent-browser mcp client is closed."));
      const closing = (async () => {
        await writer.close();
        const exit = await process.wait();
        await reading.catch(() => undefined);
        return { exitCode: exit.exitCode };
      })();
      if (options.timeout === undefined) return closing;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const timedOut = new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`agent-browser mcp close timed out after ${options.timeout}ms.`)),
          options.timeout,
        );
      });
      closing.catch(() => undefined);
      try {
        return await Promise.race([closing, timedOut]);
      } finally {
        clearTimeout(timer);
      }
    },
  };
}

async function* readLines(reader: ReadableStreamDefaultReader<Uint8Array>): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffer = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let newline = buffer.indexOf("\n");
    while (newline !== -1) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (line.length > 0) yield line;
      newline = buffer.indexOf("\n");
    }
  }
  const rest = (buffer + decoder.decode()).trim();
  if (rest.length > 0) yield rest;
}

interface JsonRpcMessage {
  readonly error?: { readonly code: number; readonly message: string; readonly data?: unknown };
  readonly id?: unknown;
  readonly result?: unknown;
}

function parseMessage(line: string): JsonRpcMessage | undefined {
  try {
    const value: unknown = JSON.parse(line);
    return typeof value === "object" && value !== null ? (value as JsonRpcMessage) : undefined;
  } catch {
    return undefined;
  }
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}
