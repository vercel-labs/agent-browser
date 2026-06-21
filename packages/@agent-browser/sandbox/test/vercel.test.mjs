import assert from "node:assert/strict";
import test from "node:test";

import {
  createAgentBrowserSandbox,
  createAgentBrowserSnapshot,
  getSandboxCredentials,
  installAgentBrowserInVercelSandbox,
  runAgentBrowserCommand,
  withAgentBrowserSandbox,
} from "../dist/vercel.js";

function commandResult(stdout = "{}", stderr = "", exitCode = 0) {
  return {
    exitCode,
    async stderr() {
      return stderr;
    },
    async stdout() {
      return stdout;
    },
  };
}

test("imports vercel entry without loading a real @vercel/sandbox module", async () => {
  assert.equal(typeof getSandboxCredentials, "function");
});

test("reads explicit Vercel credentials", () => {
  assert.deepEqual(
    getSandboxCredentials({
      VERCEL_PROJECT_ID: "project",
      VERCEL_TEAM_ID: "team",
      VERCEL_TOKEN: "token",
    }),
    { projectId: "project", teamId: "team", token: "token" },
  );
  assert.deepEqual(getSandboxCredentials({}), {});
});

test("runs agent-browser command in a Vercel sandbox", async () => {
  const calls = [];
  const sandbox = {
    async runCommand(command, args) {
      calls.push([command, args]);
      return commandResult('{"ok":true}');
    },
  };

  const result = await runAgentBrowserCommand(sandbox, ["open", "https://example.com"], {
    session: "s1",
  });

  assert.deepEqual(result.json, { ok: true });
  assert.deepEqual(calls, [
    ["agent-browser", ["--session", "s1", "open", "https://example.com", "--json"]],
  ]);
});

test("creates and bootstraps a fresh Vercel sandbox", async () => {
  const calls = [];
  const sandbox = {
    async runCommand(command, args) {
      calls.push([command, args]);
      return commandResult();
    },
    async snapshot() {
      return { snapshotId: "snap" };
    },
    async stop() {},
  };
  const Sandbox = {
    async create(options) {
      calls.push(["create", options]);
      return sandbox;
    },
  };

  await createAgentBrowserSandbox({
    Sandbox,
    env: {},
    install: { installSpec: "agent-browser@1.2.3", systemDependencies: ["nss"] },
  });

  assert.equal(calls[0][0], "create");
  assert.deepEqual(calls[1], [
    "sh",
    [
      "-c",
      "sudo dnf clean all 2>&1 && sudo dnf install -y --skip-broken -- nss 2>&1 && sudo ldconfig 2>&1",
    ],
  ]);
  assert.deepEqual(calls[2], ["npm", ["install", "-g", "agent-browser@1.2.3"]]);
  assert.deepEqual(calls[3], ["agent-browser", ["install"]]);
});

test("skips Vercel system dependencies when explicitly disabled", async () => {
  const calls = [];
  const sandbox = {
    async runCommand(command, args) {
      calls.push([command, args]);
      return commandResult();
    },
  };

  await installAgentBrowserInVercelSandbox(sandbox, {
    installSpec: "agent-browser@1.2.3",
    installSystemDependencies: false,
  });

  assert.deepEqual(calls, [
    ["npm", ["install", "-g", "agent-browser@1.2.3"]],
    ["agent-browser", ["install"]],
  ]);
});

test("rejects invalid Vercel system dependency names", async () => {
  const calls = [];
  const sandbox = {
    async runCommand(command, args) {
      calls.push([command, args]);
      return commandResult();
    },
  };

  await assert.rejects(
    () =>
      installAgentBrowserInVercelSandbox(sandbox, {
        systemDependencies: ["nss; touch /tmp/pwned"],
      }),
    /Invalid system dependency name/,
  );
  assert.deepEqual(calls, []);
});

test("stops a fresh Vercel sandbox when bootstrap fails", async () => {
  let stopped = false;
  const sandbox = {
    async runCommand() {
      return commandResult("", "install failed", 1);
    },
    async snapshot() {
      return { snapshotId: "snap" };
    },
    async stop() {
      stopped = true;
    },
  };

  await assert.rejects(
    () =>
      createAgentBrowserSandbox({
        Sandbox: { async create() { return sandbox; } },
        env: {},
        install: { systemDependencies: [] },
      }),
    /install failed/,
  );
  assert.equal(stopped, true);
});

test("withAgentBrowserSandbox stops the sandbox", async () => {
  let stopped = false;
  const sandbox = {
    async runCommand() {
      return commandResult();
    },
    async snapshot() {
      return { snapshotId: "snap" };
    },
    async stop() {
      stopped = true;
    },
  };

  const value = await withAgentBrowserSandbox(async () => 42, {
    Sandbox: { async create() { return sandbox; } },
    bootstrap: false,
    env: {},
  });

  assert.equal(value, 42);
  assert.equal(stopped, true);
});

test("withAgentBrowserSandbox preserves callback failure when stop fails", async () => {
  const sandbox = {
    async runCommand() {
      return commandResult();
    },
    async snapshot() {
      return { snapshotId: "snap" };
    },
    async stop() {
      throw new Error("stop failed");
    },
  };

  await assert.rejects(
    () =>
      withAgentBrowserSandbox(
        async () => {
          throw new Error("work failed");
        },
        {
          Sandbox: { async create() { return sandbox; } },
          bootstrap: false,
          env: {},
        },
      ),
    /work failed/,
  );
});

test("withAgentBrowserSandbox surfaces stop failure after success", async () => {
  const sandbox = {
    async runCommand() {
      return commandResult();
    },
    async snapshot() {
      return { snapshotId: "snap" };
    },
    async stop() {
      throw new Error("stop failed");
    },
  };

  await assert.rejects(
    () =>
      withAgentBrowserSandbox(async () => 42, {
        Sandbox: { async create() { return sandbox; } },
        bootstrap: false,
        env: {},
      }),
    /stop failed/,
  );
});

test("creates a Vercel sandbox snapshot", async () => {
  const sandbox = {
    async runCommand() {
      return commandResult();
    },
    async snapshot() {
      return { snapshotId: "snap_123" };
    },
    async stop() {},
  };

  const snapshotId = await createAgentBrowserSnapshot({
    Sandbox: { async create() { return sandbox; } },
    env: {},
    install: { systemDependencies: [] },
  });

  assert.equal(snapshotId, "snap_123");
});

test("createAgentBrowserSnapshot preserves snapshot failure when stop fails", async () => {
  const sandbox = {
    async runCommand() {
      return commandResult();
    },
    async snapshot() {
      throw new Error("snapshot failed");
    },
    async stop() {
      throw new Error("stop failed");
    },
  };

  await assert.rejects(
    () =>
      createAgentBrowserSnapshot({
        Sandbox: { async create() { return sandbox; } },
        env: {},
        install: { systemDependencies: [] },
      }),
    /snapshot failed/,
  );
});
