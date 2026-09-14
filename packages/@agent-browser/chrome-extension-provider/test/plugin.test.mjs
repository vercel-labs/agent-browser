import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { ensurePrivateDirectory } from "../dist/config.js";

const plugin = fileURLToPath(new URL("../dist/plugin.js", import.meta.url));
const protocol = "agent-browser.plugin.v1";

async function sandbox(t) {
  const root = await mkdtemp(join(tmpdir(), "browser provider standalone "));
  const directory = join(root, "state");
  // Deliberately allowlist only platform runtime variables. No host integration
  // service, repository path, bridge credentials or helper command is inherited.
  const env = Object.fromEntries(["PATH", "SystemRoot", "TEMP", "TMP", "TMPDIR"].filter(key => process.env[key]).map(key => [key, process.env[key]]));
  Object.assign(env, { HOME: root, USERPROFILE: root, AGENT_BROWSER_CHROME_EXTENSION_DIR: directory });
  t.after(async () => {
    const record = await readFile(join(directory, "daemon.json"), "utf8").then(JSON.parse).catch(() => null);
    if (record) { try { process.kill(record.pid, "SIGTERM"); } catch {} }
    await rm(root, { recursive: true, force: true });
  });
  return { root, directory, env };
}

async function invoke(env, type, request = {}) {
  const child = execFile(process.execPath, [plugin], { env, timeout: 12_000, maxBuffer: 65_536 });
  child.stdin.end(JSON.stringify({ protocol, type, capability: "command.run", request }));
  const result = await new Promise((resolve, reject) => {
    let stdout = "", stderr = "";
    child.stdout.on("data", chunk => stdout += chunk);
    child.stderr.on("data", chunk => stderr += chunk);
    child.on("error", reject);
    child.on("close", code => code === 0 ? resolve({ stdout, stderr }) : reject(new Error(`exit ${code}: ${stderr}`)));
  });
  assert.equal(result.stderr, "");
  return JSON.parse(result.stdout);
}

test("manifest works in an empty home and process environment without starting a relay", async t => {
  const { env, directory } = await sandbox(t);
  const result = await invoke(env, "plugin.manifest");
  assert.equal(result.success, true);
  assert.deepEqual(result.manifest.capabilities, ["browser.provider", "command.run"]);
  assert.equal(await stat(directory).then(() => true).catch(() => false), false);
});

test("launch without setup fails without selecting a browser or requiring host credentials", async t => {
  const { env } = await sandbox(t);
  const result = await invoke(env, "browser.launch", { session: "work" });
  assert.equal(result.success, false);
  assert.match(result.error, /setup/);
  const status = await invoke(env, "chrome-extension.status", { session: "work" });
  assert.equal(status.data.state, "offline");
  assert.deepEqual(status.data.scope, { namespace: "", session: "work" });
});

test("standalone setup exits promptly, starts one relay under contention, and keeps secrets private", async t => {
  const { env, directory } = await sandbox(t);
  env.AGENT_BROWSER_NAMESPACE = "project one";
  const start = Date.now();
  const results = await Promise.all([
    invoke(env, "chrome-extension.setup", { session: "alpha" }),
    invoke(env, "chrome-extension.setup", { session: "beta" }),
  ]);
  assert.ok(Date.now() - start < 10_000, "setup waited for a human or inherited relay pipe");
  for (const result of results) {
    assert.equal(result.success, true, result.error);
    assert.equal(result.data.scope.namespace, "project one");
    assert.ok(await stat(join(result.data.extensionPath, "manifest.json")));
  }
  assert.equal(results[0].data.pairingCode.split(":")[0], results[1].data.pairingCode.split(":")[0]);
  const record = JSON.parse(await readFile(join(directory, "daemon.json"), "utf8"));
  assert.ok(record.pid > 0);
  const status = await invoke(env, "chrome-extension.status", { session: "alpha" });
  assert.equal(status.success, true);
  assert.equal(JSON.stringify(status).includes(record.controlToken), false);
  assert.equal(JSON.stringify(status).includes(results[0].data.pairingCode), false);
  if (process.platform !== "win32") {
    assert.equal((await stat(directory)).mode & 0o777, 0o700);
    assert.equal((await stat(join(directory, "daemon.json"))).mode & 0o777, 0o600);
  }
});

test("session labels cannot change the private state path", async t => {
  const { env, directory } = await sandbox(t);
  const result = await invoke(env, "chrome-extension.status", { session: "../../other-state" });
  assert.equal(result.success, true);
  assert.equal(result.data.scope.session, "../../other-state");
  assert.equal(await stat(directory).then(() => true).catch(() => false), false);
});

test("competing recovery attempts never delete another startup lock", async t => {
  const { env, directory } = await sandbox(t);
  await ensurePrivateDirectory(directory);
  const lock = join(directory, "startup.lock");
  const content = JSON.stringify({ pid: 2147483647 });
  await writeFile(lock, content, { mode: 0o600 });
  const results = await Promise.all([
    invoke(env, "chrome-extension.setup", { session: "a" }),
    invoke(env, "chrome-extension.setup", { session: "b" }),
  ]);
  for (const result of results) {
    assert.equal(result.success, false);
    assert.match(result.error, /interrupted setup/);
  }
  assert.equal(await readFile(lock, "utf8"), content);
  assert.equal(await stat(join(directory, "daemon.json")).then(() => true).catch(() => false), false);
});

test("an unresponsive live relay is not replaced", { skip: process.platform === "win32" }, async t => {
  const { env, directory } = await sandbox(t);
  assert.equal((await invoke(env, "chrome-extension.setup", { session: "paused" })).success, true);
  const before = await readFile(join(directory, "daemon.json"), "utf8");
  const record = JSON.parse(before);
  process.kill(record.pid, "SIGSTOP");
  try {
    const result = await invoke(env, "chrome-extension.setup", { session: "second" });
    assert.equal(result.success, false);
    assert.match(result.error, /existing relay is unresponsive/);
    assert.equal(await readFile(join(directory, "daemon.json"), "utf8"), before);
  } finally { process.kill(record.pid, "SIGCONT"); }
});

test("Windows refuses preexisting state with explicit public access", { skip: process.platform !== "win32" }, async t => {
  const { env, directory } = await sandbox(t);
  await mkdir(directory);
  await new Promise((resolve, reject) => execFile("icacls", [directory, "/grant", "*S-1-1-0:(OI)(CI)R"], { windowsHide: true }, error => error ? reject(error) : resolve()));
  const result = await invoke(env, "chrome-extension.setup", { session: "public" });
  assert.equal(result.success, false);
  assert.match(result.error, /another account/);
  assert.equal(await stat(join(directory, "daemon.json")).then(() => true).catch(() => false), false);
});
