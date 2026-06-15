import { chromium } from "patchright";
import { existsSync } from "node:fs";

function readOption(name) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function readJsonOption(name, fallback) {
  const raw = readOption(name);
  if (!raw) return fallback;
  return JSON.parse(raw);
}

const profile = readOption("profile");
const port = readOption("port");
const headless = readOption("headless") === "true";
const executablePath = readOption("executable-path");
const userAgent = readOption("user-agent");
const args = readJsonOption("args", []);

if (!profile || !port) {
  console.error("patchright host requires --profile and --port");
  process.exit(2);
}

const launchOptions = {
  headless,
  viewport: null,
  args: [
    "--remote-debugging-address=127.0.0.1",
    `--remote-debugging-port=${port}`,
    "--disable-blink-features=AutomationControlled",
    ...args,
  ],
};

if (executablePath && existsSync(executablePath)) {
  launchOptions.executablePath = executablePath;
}

if (userAgent) {
  launchOptions.userAgent = userAgent;
}

let context;
let closed = false;

async function shutdown() {
  if (closed) return;
  closed = true;
  try {
    await context?.close();
  } catch {
  }
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
process.on("disconnect", shutdown);

context = await chromium.launchPersistentContext(profile, launchOptions);
console.log(JSON.stringify({ ready: true, port: Number(port) }));

setInterval(() => {}, 60_000);
