import assert from "node:assert/strict";
import { readdirSync } from "node:fs";
import test from "node:test";

// Since eve 0.25, extension config is bound in a scope-keyed registry instead of
// on the handle itself. The eve runtime sets this ambient scope global while it
// loads an extension's modules so `defineExtension` can capture it; emulate that
// here before importing the built dist so the mount factory calls below
// (`extension({ ... })`) bind config that the tools then read.
globalThis[Symbol.for("eve.ext-config-scope")] = "@agent-browser/eve.test";

const { default: extension } = await import("../dist/index.mjs");
const tools = await import("../dist/tools/index.mjs");
const { AGENT_BROWSER_SANDBOX_VERSION } = await import("@agent-browser/sandbox");
const { default: catalog } = await import("@agent-browser/sandbox/mcp-tools.json", {
  with: { type: "json" },
});

/** A `mcp call` result line carrying a CLI response whose `data` is `data`. */
const OK = (data, version = AGENT_BROWSER_SANDBOX_VERSION) =>
  JSON.stringify({
    _meta: { agentBrowserVersion: version },
    content: [{ type: "text", text: "ok" }],
    isError: false,
    structuredContent: { exitCode: 0, response: { success: true, data, error: null } },
  });

const FAILED = (text) =>
  JSON.stringify({
    _meta: { agentBrowserVersion: AGENT_BROWSER_SANDBOX_VERSION },
    content: [{ type: "text", text }],
    isError: true,
    structuredContent: { exitCode: 1, response: null },
  });

function fakeSandbox({ respond } = {}) {
  const commands = [];
  return {
    commands,
    async run({ command }) {
      commands.push(command);
      if (command.endsWith(" --version 2>/dev/null")) {
        return { exitCode: 0, stdout: `agent-browser ${AGENT_BROWSER_SANDBOX_VERSION}\n`, stderr: "" };
      }
      const result = respond?.(command);
      return { exitCode: 0, stdout: OK({}), stderr: "", ...result };
    },
  };
}

function fakeCtx(sandbox) {
  return { getSandbox: async () => sandbox };
}

function resetConfig(overrides = {}) {
  extension(overrides);
}

/** Splits `[ENV=...] agent-browser mcp call <tool> '<json>'` into its tool and arguments. */
function parseCall(command) {
  const match = /mcp call (\S+) '(.*)'$/.exec(command);
  assert.ok(match, command);
  return { tool: match[1], args: JSON.parse(match[2].replaceAll("'\\''", "'")) };
}

test("ships one tool file per catalog tool it exposes", () => {
  const files = readdirSync(new URL("../extension/tools", import.meta.url)).map((file) =>
    file.replace(/\.ts$/, ""),
  );
  const names = new Set(catalog.tools.map((tool) => tool.name.replace(/^agent_browser_/, "")));
  assert.deepEqual(Object.keys(tools).sort(), files.sort());
  for (const name of files) assert.ok(names.has(name), `${name} is not in the MCP catalog`);
  for (const name of ["open", "snapshot", "click", "fill", "screenshot", "drag", "upload", "find"]) {
    assert.ok(name in tools, `missing tool: ${name}`);
  }
});

test("close cannot reach other sessions", async () => {
  assert.ok(!("all" in tools.close.inputSchema.properties));
  resetConfig();
  const sandbox = fakeSandbox();
  await tools.close.execute({ all: true }, fakeCtx(sandbox));
  assert.equal(parseCall(sandbox.commands.at(-1)).args.all, undefined);
});

test("instructions name only tools the extension ships", async () => {
  const { readFileSync } = await import("node:fs");
  const text = readFileSync(new URL("../extension/instructions.md", import.meta.url), "utf8");
  for (const [, name] of text.matchAll(/`([a-z_]+)`/g)) {
    if (name === "networkidle" || name === "eve__search" || name.endsWith("_")) continue;
    assert.ok(name in tools, `instructions mention missing tool ${name}`);
  }
});

test("lists core-profile tools and defers the rest", () => {
  for (const tool of catalog.tools) {
    const name = tool.name.replace(/^agent_browser_/, "");
    if (!(name in tools)) continue;
    assert.equal(tools[name].deferred === true, !tool.profiles.includes("core"), name);
  }
});

test("takes descriptions and schemas from the catalog, minus app-owned arguments", () => {
  const source = catalog.tools.find((tool) => tool.name === "agent_browser_click");
  assert.equal(tools.click.description, source.description);
  const properties = Object.keys(tools.click.inputSchema.properties);
  assert.ok(properties.includes("selector"));
  for (const hidden of ["session", "allowedDomains", "caCert", "namespace", "restore", "extraArgs"]) {
    assert.ok(!properties.includes(hidden), `${hidden} reached the model`);
  }
});

test("runs one mcp call per tool call with the default session", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    respond: () => ({ stdout: OK({ title: "Example", url: "https://example.com/" }) }),
  });
  const result = await tools.open.execute({ url: "https://example.com" }, fakeCtx(sandbox));
  assert.deepEqual(result, { title: "Example", url: "https://example.com/" });
  const command = sandbox.commands.at(-1);
  assert.ok(command.startsWith("agent-browser mcp call agent_browser_open "), command);
  assert.deepEqual(parseCall(command).args, { url: "https://example.com", session: "eve-default" });
});

test("auto-installs when the binary is missing", async () => {
  resetConfig({ installSystemDependencies: false });
  const sandbox = versionSandbox([undefined, AGENT_BROWSER_SANDBOX_VERSION]);
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(sandbox.commands.some((command) => command.includes("npm install -g agent-browser@")));
  resetConfig();
});

test("reinstalls when the sandbox has another agent-browser minor", async () => {
  resetConfig({ installSystemDependencies: false });
  const sandbox = versionSandbox(["0.1.0", AGENT_BROWSER_SANDBOX_VERSION]);
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(sandbox.commands.some((command) => command.includes("npm install -g agent-browser@")));
  resetConfig();
});

function versionSandbox(versions) {
  const commands = [];
  return {
    commands,
    async run({ command }) {
      commands.push(command);
      if (command.endsWith(" --version 2>/dev/null")) {
        const version = versions.shift();
        return version === undefined
          ? { exitCode: 127, stdout: "", stderr: "" }
          : { exitCode: 0, stdout: `agent-browser ${version}\n`, stderr: "" };
      }
      return { exitCode: 0, stdout: OK({}), stderr: "" };
    },
  };
}

test("fails with a config error when the install still has another minor", async () => {
  resetConfig({ installSystemDependencies: false });
  const sandbox = versionSandbox(["0.1.0", "0.1.0"]);
  await assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /agent-browser 0\.1\.0 in the sandbox does not match/,
  );
  assert.ok(!sandbox.commands.some((command) => command.includes(" mcp call ")));
  resetConfig();
});

test("does not reinstall a custom installSpec on a version mismatch", async () => {
  resetConfig({ installSpec: "agent-browser@0.1.0", installSystemDependencies: false });
  const sandbox = versionSandbox(["0.1.0"]);
  await assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /agent-browser 0\.1\.0 in the sandbox does not match/,
  );
  assert.ok(!sandbox.commands.some((command) => command.includes("npm install -g")));
  resetConfig();
});

test("skips the install probe when autoInstall is false", async () => {
  resetConfig({ autoInstall: false });
  const sandbox = fakeSandbox();
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(!sandbox.commands.some((command) => command.endsWith(" --version 2>/dev/null")));
  resetConfig();
});

test("passes access config as arguments and server settings as environment", async () => {
  resetConfig({
    allowedDomains: ["example.com", "*.example.com"],
    caCert: "/etc/ssl/certs/proxy-ca.pem",
    contentBoundaries: true,
    maxOutputChars: 5000,
    proxy: "http://proxy.example.com:8080",
    useSystemCa: true,
  });
  const sandbox = fakeSandbox();
  await tools.close.execute({}, fakeCtx(sandbox));
  const command = sandbox.commands.at(-1);
  assert.ok(command.startsWith("AGENT_BROWSER_PROXY=http://proxy.example.com:8080 "), command);
  assert.ok(command.includes("AGENT_BROWSER_CONTENT_BOUNDARIES=1 "), command);
  assert.ok(command.includes("AGENT_BROWSER_MAX_OUTPUT=5000 "), command);
  assert.deepEqual(parseCall(command).args, {
    session: "eve-default",
    allowedDomains: ["example.com", "*.example.com"],
    caCert: "/etc/ssl/certs/proxy-ca.pem",
    useSystemCa: true,
  });
  resetConfig();
});

test("model input cannot override app-owned arguments", async () => {
  resetConfig({ session: "shared" });
  const sandbox = fakeSandbox();
  await tools.close.execute({ session: "other", allowedDomains: [] }, fakeCtx(sandbox));
  const { args } = parseCall(sandbox.commands.at(-1));
  assert.equal(args.session, "shared");
  assert.equal(args.allowedDomains, undefined);
  resetConfig();
});

test("can explicitly clear retained CA trust", async () => {
  resetConfig({ clearCaCert: true });
  const sandbox = fakeSandbox();
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.equal(parseCall(sandbox.commands.at(-1)).args.clearCaCert, true);
  resetConfig();
});

test("rejects config CA selection with a simultaneous clear", async () => {
  resetConfig({ caCert: "/etc/ssl/certs/proxy-ca.pem", clearCaCert: true });
  await assert.rejects(
    tools.close.execute({}, fakeCtx(fakeSandbox())),
    /Cannot use caCert with clearCaCert/,
  );
  resetConfig();
});

test("quotes arguments for the shell", async () => {
  resetConfig();
  const sandbox = fakeSandbox();
  await tools.fill.execute({ selector: "#q", text: "it's $HOME" }, fakeCtx(sandbox));
  assert.equal(parseCall(sandbox.commands.at(-1)).args.text, "it's $HOME");
});

test("surfaces tool errors from the CLI", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ respond: () => ({ exitCode: 1, stdout: FAILED("Element not found") }) });
  await assert.rejects(
    () => tools.click.execute({ selector: "#nope" }, fakeCtx(sandbox)),
    /agent_browser_click failed: Element not found/,
  );
});

test("surfaces invalid calls from the CLI", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    respond: () => ({ exitCode: 2, stdout: JSON.stringify({ error: { code: -32602, message: "bad" } }) }),
  });
  await assert.rejects(() => tools.close.execute({}, fakeCtx(sandbox)), /failed: bad/);
});

test("fails loudly when the sandbox runs another agent-browser minor", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ respond: () => ({ stdout: OK({}, "9.9.0") }) });
  await assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /agent-browser 9\.9\.0 in the sandbox does not match @agent-browser\/eve/,
  );
});

test("shows the install hint when another minor rejects the call", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    respond: () => ({
      exitCode: 2,
      stdout: JSON.stringify({
        _meta: { agentBrowserVersion: "9.9.0" },
        error: { code: -32602, message: "Unknown tool: agent_browser_close" },
      }),
    }),
  });
  await assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /agent-browser 9\.9\.0 in the sandbox does not match[\s\S]*Install agent-browser@/,
  );
});

test("explains when the sandbox CLI predates mcp call", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    respond: () => ({ exitCode: 1, stdout: "", stderr: "Unknown mcp option: call" }),
  });
  await assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /does not support `mcp call`[\s\S]*Unknown mcp option: call/,
  );
});

test("throws a helpful error when no sandbox is available", async () => {
  resetConfig();
  await assert.rejects(
    () => tools.close.execute({}, { getSandbox: async () => null }),
    /require an eve sandbox/,
  );
});

test("snapshot retries once on a transient CDP error", async () => {
  resetConfig();
  let calls = 0;
  const sandbox = fakeSandbox({
    respond: () => {
      calls += 1;
      return calls === 1
        ? { exitCode: 1, stdout: FAILED("CDP error: No node with given id found") }
        : { stdout: OK({ snapshot: "- button [ref=e1]" }) };
    },
  });
  const output = await tools.snapshot.execute({}, fakeCtx(sandbox));
  assert.equal(output.snapshot, "- button [ref=e1]");
  assert.equal(calls, 2);
});

test("snapshot does not retry non-CDP errors", async () => {
  resetConfig();
  let calls = 0;
  const sandbox = fakeSandbox({
    respond: () => {
      calls += 1;
      return { exitCode: 1, stdout: FAILED("Browser not launched") };
    },
  });
  await assert.rejects(() => tools.snapshot.execute({}, fakeCtx(sandbox)), /Browser not launched/);
  assert.equal(calls, 1);
});

test("screenshot inlines the image and hides it from the model", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }) });
  sandbox.readBinaryFile = async ({ path }) => {
    assert.equal(path, "/workspace/shot.png");
    return new Uint8Array([137, 80, 78, 71]);
  };
  const output = await tools.screenshot.execute({}, fakeCtx(sandbox));
  assert.equal(output.path, "/workspace/shot.png");
  assert.ok(output.imageDataUrl.startsWith("data:image/png;base64,"), output.imageDataUrl);
  const modelOutput = await tools.screenshot.toModelOutput(output);
  assert.equal(modelOutput.type, "json");
  assert.equal(modelOutput.value.imageDataUrl, undefined);
  // The image is already shown to the user, so the model gets neither the
  // base64 nor the sandbox path it might otherwise echo into its reply.
  assert.equal(modelOutput.value.path, undefined);
  assert.match(modelOutput.value.screenshot, /displayed to the user/);
});

test("screenshot still works when the sandbox cannot read files back", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }) });
  const output = await tools.screenshot.execute({}, fakeCtx(sandbox));
  assert.equal(output.path, "/workspace/shot.png");
  assert.equal(output.imageDataUrl, undefined);
  // Without an inline image the user has no other way to locate the file,
  // so the model keeps the path.
  const modelOutput = await tools.screenshot.toModelOutput(output);
  assert.equal(modelOutput.value.path, "/workspace/shot.png");
});

test("screenshot skips inlining when disabled in config", async () => {
  resetConfig({ inlineScreenshots: false });
  const sandbox = fakeSandbox({ respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }) });
  sandbox.readBinaryFile = async () => new Uint8Array([1]);
  const output = await tools.screenshot.execute({}, fakeCtx(sandbox));
  assert.equal(output.imageDataUrl, undefined);
  resetConfig();
});

test("errors instead of hanging when the sandbox stops responding", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  resetConfig({ autoInstall: false });
  const sandbox = { run: () => new Promise(() => {}) };
  const rejection = assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /did not respond within 180s/,
  );
  // Let the async chain reach sandbox.run and arm the deadline timer.
  await new Promise((resolve) => setImmediate(resolve));
  t.mock.timers.tick(180_001);
  await rejection;
});

test("errors instead of hanging when the install probe stops responding", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  resetConfig();
  const sandbox = { run: () => new Promise(() => {}) };
  const rejection = assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /The browser install probe did not respond within 180s/,
  );
  // Let the async chain reach the probe's sandbox.run and arm the deadline timer.
  await new Promise((resolve) => setImmediate(resolve));
  t.mock.timers.tick(180_001);
  await rejection;
});

test("mount factory validates config", () => {
  assert.throws(() => extension({ maxOutputChars: -1 }));
  resetConfig();
});
