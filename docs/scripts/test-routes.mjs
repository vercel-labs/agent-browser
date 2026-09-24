import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import { createServer } from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const cwd = fileURLToPath(new URL("../", import.meta.url));
const preview = process.argv.includes("--preview");
assert.ok(
  process.argv.slice(2).every((arg) => arg === "--preview"),
  "Usage: node scripts/test-routes.mjs [--preview]",
);
await access(new URL("../.next/BUILD_ID", import.meta.url)).catch(() => {
  throw new Error(
    "Missing production docs build. Run pnpm run build in docs first; this runner never starts next dev.",
  );
});
const socket = createServer();
await new Promise((resolve, reject) => {
  socket.once("error", reject);
  socket.listen(0, "127.0.0.1", resolve);
});
const port = socket.address().port;
await new Promise((resolve, reject) =>
  socket.close((error) => (error ? reject(error) : resolve())),
);
const env = {
  ...process.env,
  NODE_ENV: "production",
  PORT: String(port),
  VERCEL_ENV: preview ? "preview" : "production",
  DOCS_EXPECT_NOINDEX: preview ? "1" : "0",
  DOCS_TEST_URL: `http://127.0.0.1:${port}`,
};
function launch(args) {
  const child = spawn(process.execPath, args, { cwd, stdio: "inherit", env });
  const owned = { child, done: false, exited: undefined };
  owned.exited = new Promise((resolve) => {
    child.once("error", (error) => {
      console.error(error.message);
      owned.done = true;
      resolve(1);
    });
    child.once("exit", (code) => {
      owned.done = true;
      resolve(code ?? 1);
    });
  });
  return owned;
}
const server = launch([
  fileURLToPath(import.meta.resolve("next/dist/bin/next")),
  "start",
  "--hostname",
  "127.0.0.1",
  "--port",
  String(port),
]);
let tests;
let shutdown;
let stopping = false;
function stop() {
  stopping = true;
  shutdown ??= Promise.all(
    [tests, server].map(async (owned) => {
      if (!owned || owned.done) return;
      owned.child.kill("SIGTERM");
      const deadline = setTimeout(() => owned.child.kill("SIGKILL"), 5000);
      try {
        await owned.exited;
      } finally {
        clearTimeout(deadline);
      }
    }),
  );
  return shutdown;
}
for (const [signal, code] of [
  ["SIGINT", 130],
  ["SIGTERM", 143],
]) {
  process.once(signal, () => {
    void stop().finally(() => process.exit(code));
  });
}
try {
  const deadline = Date.now() + 60000;
  let ready = false;
  while (Date.now() < deadline && !stopping) {
    if (server.done)
      throw new Error(
        `Production docs server exited with ${await server.exited}`,
      );
    try {
      const response = await fetch(`${env.DOCS_TEST_URL}/robots.txt`, {
        signal: AbortSignal.timeout(1000),
        redirect: "manual",
      });
      await response.body?.cancel();
      if (response.ok) {
        ready = true;
        break;
      }
    } catch {}
    await sleep(100);
  }
  if (!ready && !stopping)
    throw new Error(
      "Production docs server did not become ready within 60 seconds",
    );
  if (!stopping) {
    console.log(
      `Testing owned production server ${env.DOCS_TEST_URL}; VERCEL_ENV=${env.VERCEL_ENV}`,
    );
    tests = launch([
      "--test",
      "--test-concurrency=1",
      "tests/baseline-oracle.test.mjs",
      "tests/docs-baseline.test.mjs",
      "tests/docs-routes.test.mjs",
    ]);
    const outcome = await Promise.race([
      tests.exited.then((code) => ({ source: "tests", code })),
      server.exited.then((code) => ({ source: "server", code })),
    ]);
    if (outcome.source === "server")
      throw new Error(
        `Production server exited during tests with ${outcome.code}`,
      );
    process.exitCode = outcome.code;
  }
} finally {
  await stop();
}
