/**
 * E2e test for the Next.js integration HTTP proxy server.
 *
 * Validates the full pipeline:
 *   HTTP request -> proxy server -> Unix socket -> agent-browser daemon -> Chrome
 *
 * Requires Chrome to be installed. Run with:
 *   pnpm test -- test/e2e/next-proxy.test.ts
 */

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import * as http from "node:http";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";

const SESSION = `test-next-proxy-${Date.now()}`;
const PROXY_PORT = 49_300 + Math.floor(Math.random() * 100);
const PROJECT_ROOT = path.resolve(import.meta.dirname, "../..");

function getSocketDir(): string {
  if (process.env.AGENT_BROWSER_SOCKET_DIR)
    return process.env.AGENT_BROWSER_SOCKET_DIR;
  if (process.env.XDG_RUNTIME_DIR)
    return path.join(process.env.XDG_RUNTIME_DIR, "agent-browser");
  const home = os.homedir();
  return home
    ? path.join(home, ".agent-browser")
    : path.join(os.tmpdir(), "agent-browser");
}

function getSocketPath(): string {
  return path.join(getSocketDir(), `${SESSION}.sock`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForSocket(timeoutMs = 15_000): Promise<void> {
  const start = Date.now();
  const socketPath = getSocketPath();
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(socketPath)) {
      try {
        await new Promise<void>((resolve, reject) => {
          const c = net.createConnection({ path: socketPath }, () => {
            c.destroy();
            resolve();
          });
          c.on("error", reject);
          c.setTimeout(1000);
          c.on("timeout", () => {
            c.destroy();
            reject(new Error("timeout"));
          });
        });
        return;
      } catch {
        // not ready yet
      }
    }
    await sleep(100);
  }
  throw new Error(`Daemon socket not ready within ${timeoutMs}ms`);
}

async function waitForHttp(
  port: number,
  timeoutMs = 10_000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      await fetch(`http://localhost:${port}/health`);
      return;
    } catch {
      await sleep(200);
    }
  }
  throw new Error(`HTTP server not ready on port ${port} within ${timeoutMs}ms`);
}

function sendSocketCommand(
  cmd: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const client = net.createConnection({ path: getSocketPath() }, () => {
      client.write(JSON.stringify(cmd) + "\n");
    });
    let data = "";
    client.on("data", (chunk) => {
      data += chunk.toString();
      const idx = data.indexOf("\n");
      if (idx !== -1) {
        client.destroy();
        try {
          resolve(JSON.parse(data.slice(0, idx)));
        } catch {
          reject(new Error(`Invalid JSON: ${data}`));
        }
      }
    });
    client.on("error", reject);
    client.setTimeout(30_000);
    client.on("timeout", () => {
      client.destroy();
      reject(new Error("timeout"));
    });
  });
}

async function postJson(
  port: number,
  path: string,
  body: unknown,
): Promise<{ status: number; data: Record<string, unknown> }> {
  const res = await fetch(`http://localhost:${port}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = (await res.json()) as Record<string, unknown>;
  return { status: res.status, data };
}

let daemonProcess: ChildProcess | undefined;
let proxyProcess: ChildProcess | undefined;

describe("Next.js proxy -> daemon pipeline", () => {
  beforeAll(async () => {
    const daemonPath = path.join(PROJECT_ROOT, "dist/daemon.js");
    if (!fs.existsSync(daemonPath)) {
      throw new Error(
        `Node daemon not found at ${daemonPath}. Run "pnpm build" first.`,
      );
    }

    daemonProcess = spawn("node", [daemonPath], {
      env: {
        ...process.env,
        AGENT_BROWSER_DAEMON: "1",
        AGENT_BROWSER_SESSION: SESSION,
      },
      stdio: ["ignore", "ignore", "pipe"],
    });

    await waitForSocket();

    const proxyPath = path.join(
      PROJECT_ROOT,
      "examples/next/server/index.ts",
    );
    proxyProcess = spawn("npx", ["tsx", proxyPath], {
      env: {
        ...process.env,
        PORT: String(PROXY_PORT),
        AGENT_BROWSER_SESSION: SESSION,
      },
      stdio: ["ignore", "ignore", "pipe"],
    });

    await waitForHttp(PROXY_PORT);
  }, 30_000);

  afterAll(async () => {
    try {
      await sendSocketCommand({ id: "close", action: "close" });
    } catch {
      // daemon may be gone
    }

    await sleep(200);

    if (proxyProcess) {
      proxyProcess.kill("SIGTERM");
      proxyProcess = undefined;
    }
    if (daemonProcess) {
      daemonProcess.kill("SIGTERM");
      daemonProcess = undefined;
    }

    const sockPath = getSocketPath();
    try {
      fs.unlinkSync(sockPath);
    } catch {
      // ok
    }
    try {
      fs.unlinkSync(sockPath.replace(/\.sock$/, ".pid"));
    } catch {
      // ok
    }
  }, 15_000);

  it("GET /health returns daemon status", async () => {
    const res = await fetch(`http://localhost:${PROXY_PORT}/health`);
    const data = (await res.json()) as Record<string, unknown>;

    expect(res.status).toBe(200);
    expect(data.ok).toBe(true);
    expect(data.daemon).toBe(true);
    expect(data.session).toBe(SESSION);
  });

  it("POST /api/command sends a single command", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/command", {
      action: "launch",
      headless: true,
    });

    expect(status).toBe(200);
    expect(data.success).toBe(true);
  });

  it("POST /api/command navigates to a URL", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/command", {
      action: "navigate",
      url: "data:text/html,<h1>Hello from proxy test</h1>",
    });

    expect(status).toBe(200);
    expect(data.success).toBe(true);
  });

  it("POST /api/run executes a command sequence", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/run", {
      commands: [
        {
          action: "navigate",
          url: "data:text/html,<h1>Sequence test</h1>",
        },
        { action: "title" },
      ],
    });

    expect(status).toBe(200);
    const results = data.results as Array<Record<string, unknown>>;
    expect(results).toHaveLength(2);
    expect(results[0].success).toBe(true);
    expect(results[1].success).toBe(true);
  });

  it("POST /api/command takes a screenshot", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/command", {
      action: "screenshot",
    });

    expect(status).toBe(200);
    expect(data.success).toBe(true);
    const inner = data.data as Record<string, unknown>;
    expect(inner.path).toBeTruthy();
  });

  it("POST /api/command returns snapshot text", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/command", {
      action: "snapshot",
    });

    expect(status).toBe(200);
    expect(data.success).toBe(true);
    const inner = data.data as Record<string, unknown>;
    expect(typeof inner.snapshot).toBe("string");
  });

  it("returns 400 for missing action", async () => {
    const { status, data } = await postJson(PROXY_PORT, "/api/command", {
      url: "https://example.com",
    });

    expect(status).toBe(400);
    expect(data.error).toBeTruthy();
  });

  it("returns 404 for unknown routes", async () => {
    const res = await fetch(`http://localhost:${PROXY_PORT}/unknown`, {
      method: "POST",
    });
    expect(res.status).toBe(404);
  });
});
