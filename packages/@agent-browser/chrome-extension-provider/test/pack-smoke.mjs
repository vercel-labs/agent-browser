import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const root = await mkdtemp(join(tmpdir(), "provider packed install "));
const state = join(root, "private-state");
const pnpmScript = process.env.npm_execpath;
assert.ok(pnpmScript, "Run this check with pnpm test:pack");
function execute(command, args, options = {}, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...options, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stdout.on("data", chunk => stdout += chunk);
    child.stderr.on("data", chunk => stderr += chunk);
    child.once("error", reject);
    child.once("exit", code => code === 0 ? resolve(stdout) : reject(new Error(`${command} exited ${code}: ${stderr.slice(-4000)}`)));
    child.stdin.end(input);
  });
}
try {
  const archive = join(root, "provider.tgz");
  await execute(process.execPath, [pnpmScript, "pack", "--out", archive], { cwd: packageRoot });
  await writeFile(join(root, "package.json"), JSON.stringify({ private: true }));
  await execute(process.execPath, [pnpmScript, "add", archive, "--ignore-scripts"], { cwd: root });
  const entry = join(root, "node_modules", "@agent-browser", "chrome-extension-provider", "dist", "plugin.js");
  const env = Object.fromEntries(["PATH", "SystemRoot", "TEMP", "TMP", "TMPDIR"].filter(key => process.env[key]).map(key => [key, process.env[key]]));
  Object.assign(env, { HOME: root, USERPROFILE: root, AGENT_BROWSER_CHROME_EXTENSION_DIR: state });
  const call = async (type, request = {}) => JSON.parse(await execute(process.execPath, [entry], { cwd: root, env }, JSON.stringify({ protocol: "agent-browser.plugin.v1", type, request })));
  assert.equal((await call("plugin.manifest")).success, true);
  const setup = await call("chrome-extension.setup", { session: "packed" });
  assert.equal(setup.success, true, setup.error);
  assert.ok(setup.data.extensionPath.startsWith(await realpath(root)));
  const manifest = JSON.parse(await readFile(join(setup.data.extensionPath, "manifest.json"), "utf8"));
  assert.ok(manifest.permissions.includes("debugger"));
  assert.equal((await call("browser.launch", { session: "packed" })).success, false, "A fresh package must require explicit tab approval");
  console.log("Packed artifact works without a repository, workspace dependencies, host environment, or preexisting credentials.");
} finally {
  const record = await readFile(join(state, "daemon.json"), "utf8").then(JSON.parse).catch(() => null);
  if (record) { try { process.kill(record.pid, "SIGTERM"); } catch {} }
  await rm(root, { recursive: true, force: true });
}
