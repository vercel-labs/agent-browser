import { spawn } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { open, unlink } from "node:fs/promises";
import { join } from "node:path";
import type { Readable, Writable } from "node:stream";
import { fileURLToPath } from "node:url";
import { ensurePrivateDirectory, readPrivateJson, writePrivateJson } from "./config.js";

export type DaemonRecord = { port: number; pid: number; controlToken: string; instanceId: string };
const recordPath = (directory: string) => join(directory, "daemon.json");

export async function relayRequest<T>(record: DaemonRecord, path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`http://127.0.0.1:${record.port}${path}`, {
    ...init,
    signal: AbortSignal.timeout(5000),
    headers: { "content-type": "application/json", authorization: `Bearer ${record.controlToken}`, ...init.headers },
  });
  if (!response.ok) throw new Error(`Relay request failed (${response.status}); check chrome-extension.status and run setup if needed`);
  return await response.json() as T;
}

export async function readLiveDaemon(directory: string): Promise<DaemonRecord | undefined> {
  const value = await readPrivateJson(recordPath(directory));
  if (!value) return undefined;
  if (!Number.isInteger(value.port) || Number(value.port) < 1 || Number(value.port) > 65535 ||
      !Number.isInteger(value.pid) || Number(value.pid) < 1 ||
      typeof value.controlToken !== "string" || !/^[a-f0-9]{64}$/.test(value.controlToken) ||
      typeof value.instanceId !== "string") throw new Error("Invalid relay state; rerun setup after removing the invalid provider state file");
  const record = value as DaemonRecord;
  try { await relayRequest(record, "/health"); return record; } catch {
    if (isRunning(record.pid)) throw new Error("The existing relay is unresponsive; stop that relay process before running setup again");
    return undefined;
  }
}

function isRunning(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** Serialize setup; interrupted startup needs explicit recovery rather than racing to reap a lock. */
export async function ensureDaemon(directory: string): Promise<DaemonRecord> {
  await ensurePrivateDirectory(directory);
  const lockPath = join(directory, "startup.lock");
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const existing = await readLiveDaemon(directory);
    if (existing) return existing;
    let lock;
    try { lock = await open(lockPath, "wx", 0o600); } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      const owner = await readPrivateJson(lockPath).catch(() => undefined);
      if (Number.isInteger(owner?.pid) && !isRunning(Number(owner!.pid))) {
        throw new Error(`An interrupted setup left ${lockPath}; remove that lock file before retrying setup`);
      }
      await new Promise(resolve => setTimeout(resolve, 100));
      continue;
    }
    try {
      await lock.writeFile(JSON.stringify({ pid: process.pid }));
      const existing = await readLiveDaemon(directory);
      if (existing) return existing;
      const record = await startDaemon();
      try { await writePrivateJson(recordPath(directory), record); } catch (error) {
        process.kill(record.pid, "SIGTERM");
        throw error;
      }
      return record;
    } finally {
      await lock.close();
      await unlink(lockPath).catch(() => undefined);
    }
  }
  throw new Error(`Setup is still running or was interrupted; once no setup process is running, remove ${lockPath} and retry`);
}

async function startDaemon(): Promise<DaemonRecord> {
  const controlToken = randomBytes(32).toString("hex");
  const child = spawn(process.execPath, [fileURLToPath(new URL("./daemon/cli.js", import.meta.url))], {
    detached: true, windowsHide: true,
    stdio: ["ignore", "ignore", "ignore", "pipe", "pipe"],
  });
  const input = child.stdio[3] as Writable;
  const output = child.stdio[4] as Readable;
  try {
    const port = await new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("Relay startup timed out")), 8000);
      let result = "";
      child.once("error", reject);
      child.once("exit", () => reject(new Error("Relay exited during startup")));
      output.on("data", chunk => {
        result += String(chunk);
        if (result.length > 1024) reject(new Error("Invalid relay readiness response"));
      });
      output.once("end", () => {
        clearTimeout(timer);
        try {
          const { port } = JSON.parse(result) as { port: number };
          if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid relay port");
          resolve(port);
        } catch { reject(new Error("Invalid relay readiness response")); }
      });
      input.on("error", reject);
      input.end(JSON.stringify({ controlToken }));
    });
    child.unref();
    return { port, pid: child.pid!, controlToken, instanceId: randomUUID() };
  } catch (error) {
    child.kill();
    throw error;
  } finally { input.destroy(); output.destroy(); }
}
