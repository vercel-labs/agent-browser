#!/usr/bin/env node
import { closeSync, readFileSync, writeSync } from "node:fs";
import { BridgeDaemon } from "./server.js";

// Private inherited pipes carry startup credentials and one readiness response.
// Neither stdout nor a command-line argument carries relay credentials.
async function main() {
  const seed = JSON.parse(readFileSync(3, "utf8")) as { controlToken?: unknown };
  closeSync(3);
  if (typeof seed.controlToken !== "string" || !/^[a-f0-9]{64}$/.test(seed.controlToken)) {
    throw new Error("Invalid private relay bootstrap");
  }
  const daemon = new BridgeDaemon({ port: 0, controlToken: seed.controlToken });
  const port = await daemon.start();
  writeSync(4, JSON.stringify({ port }));
  closeSync(4);
  let closing = false;
  const shutdown = () => {
    if (closing) return;
    closing = true;
    void daemon.stop().finally(() => process.exit(0));
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}
main().catch(() => process.exit(1));
