import assert from "node:assert/strict";
import test from "node:test";

import {
  agentBrowserRevalidationKey,
  buildAgentBrowserCommand,
  installAgentBrowser,
  runAgentBrowser,
} from "../dist/eve.js";

test("builds Eve revalidation key from install options", () => {
  assert.equal(
    agentBrowserRevalidationKey({ installSpec: "agent-browser@1.2.3" }),
    "agent-browser:bootstrap-3:agent-browser@1.2.3:browser:system-deps",
  );
  assert.equal(
    agentBrowserRevalidationKey({ installSpec: "agent-browser@1.2.3", installSystemDependencies: false }),
    "agent-browser:bootstrap-3:agent-browser@1.2.3:browser:no-system-deps",
  );
});

test("builds Eve shell command", () => {
  assert.equal(
    buildAgentBrowserCommand(["open", "https://example.com"], { session: "s1" }),
    "agent-browser --session s1 open https://example.com --json",
  );
});

test("installs agent-browser in an Eve sandbox", async () => {
  const commands = [];
  const sandbox = {
    id: "sandbox-1",
    async run({ command }) {
      commands.push(command);
      return { exitCode: 0, stdout: "", stderr: "" };
    },
  };

  await installAgentBrowser(sandbox, { installSpec: "agent-browser@1.2.3" });

  assert.equal(commands.length, 3);
  assert.match(commands[0], /^if command -v apt-get/);
  assert.match(commands[0], /libglib2\.0-0t64/);
  assert.match(commands[0], /libasound2t64/);
  assert.match(commands[0], /sudo ldconfig; elif command -v dnf/);
  assert.match(commands[0], /sudo ldconfig; else echo/);
  assert.match(commands[0], /sudo dnf install -y --skip-broken -- glib2 nss/);
  assert.equal(commands[1], "npm install -g agent-browser@1.2.3");
  assert.equal(commands[2], "agent-browser install");
});

test("skips Eve system dependencies when explicitly disabled", async () => {
  const commands = [];
  const sandbox = {
    id: "sandbox-1",
    async run({ command }) {
      commands.push(command);
      return { exitCode: 0, stdout: "", stderr: "" };
    },
  };

  await installAgentBrowser(sandbox, {
    installSpec: "agent-browser@1.2.3",
    installSystemDependencies: false,
  });

  assert.deepEqual(commands, ["npm install -g agent-browser@1.2.3", "agent-browser install"]);
});

test("runs agent-browser through ctx.getSandbox", async () => {
  const commands = [];
  const ctx = {
    async getSandbox() {
      return {
        id: "sandbox/id 1",
        async run({ command }) {
          commands.push(command);
          return { exitCode: 0, stdout: '{"ok":true}', stderr: "" };
        },
      };
    },
  };

  const result = await runAgentBrowser(ctx, ["open", "https://example.com"]);

  assert.deepEqual(result.json, { ok: true });
  assert.equal(commands[0], "agent-browser --session eve-sandbox-id-1 open https://example.com --json");
});

test("uses a short generated session for long Eve sandbox ids", async () => {
  const commands = [];
  const ctx = {
    async getSandbox() {
      return {
        id: "eve-sbx-ses-vercel-1d940340bdba4563-wrun_01KVKDK1Z3GC3XEC86DGWRWRMH-__root__",
        async run({ command }) {
          commands.push(command);
          return { exitCode: 0, stdout: '{"ok":true}', stderr: "" };
        },
      };
    },
  };

  await runAgentBrowser(ctx, ["open", "https://example.com"]);

  const session = commands[0].match(/--session ([^ ]+)/)?.[1];
  assert.equal(session.length <= 48, true);
  assert.match(commands[0], /^agent-browser --session eve-eve-sbx-ses-vercel-.+-[a-f0-9]{8} open/);
});

test("accepts Eve promise-like sandbox methods", async () => {
  const thenable = (value) => ({
    then(resolve) {
      resolve(value);
    },
  });
  const ctx = {
    getSandbox() {
      return thenable({
        id: "sandbox-1",
        run() {
          return thenable({ exitCode: 0, stdout: '{"ok":true}', stderr: "" });
        },
      });
    },
  };

  const result = await runAgentBrowser(ctx, ["snapshot"]);

  assert.deepEqual(result.json, { ok: true });
});

test("throws when Eve sandbox command fails", async () => {
  const ctx = {
    async getSandbox() {
      return {
        id: "sandbox-1",
        async run() {
          return { exitCode: 2, stdout: "", stderr: "no chrome" };
        },
      };
    },
  };

  await assert.rejects(() => runAgentBrowser(ctx, ["snapshot"]), /no chrome/);
});
