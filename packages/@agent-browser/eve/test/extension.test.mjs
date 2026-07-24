import assert from "node:assert/strict";
import test from "node:test";

// Since eve 0.25, extension config is bound in a scope-keyed registry instead of
// on the handle itself. The eve runtime sets this ambient scope global while it
// loads an extension's modules so `defineExtension` can capture it; emulate that
// here before importing the built dist so the mount factory calls below
// (`extension({ ... })`) bind config that the tools then read.
globalThis[Symbol.for("eve.ext-config-scope")] = "@agent-browser/eve.test";

const { default: extension } = await import("../dist/index.mjs");
const tools = await import("../dist/tools/index.mjs");

const OK = (data) => JSON.stringify({ success: true, data, error: null });

function fakeSandbox({ id, respond } = {}) {
  const commands = [];
  return {
    commands,
    id: id ?? `sbx-${Math.random().toString(36).slice(2)}`,
    async run({ command }) {
      commands.push(command);
      if (command.startsWith("command -v ")) {
        return { exitCode: 0, stdout: "", stderr: "" };
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

test("exports the full tool set", () => {
  const expected = [
    "click",
    "close",
    "console",
    "drag",
    "evaluate",
    "fill",
    "find",
    "get",
    "hover",
    "navigate",
    "network_requests",
    "press_key",
    "read",
    "screenshot",
    "scroll",
    "select_option",
    "set_checked",
    "snapshot",
    "tabs",
    "upload",
    "wait_for",
  ];
  for (const name of expected) {
    assert.ok(name in tools, `missing tool export: ${name}`);
  }
});

test("navigate builds an open command with a per-sandbox session", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "abc",
    respond: () => ({ stdout: OK({ title: "Example", url: "https://example.com/" }) }),
  });
  const result = await tools.navigate.execute(
    { action: "goto", url: "https://example.com" },
    fakeCtx(sandbox),
  );
  assert.deepEqual(result, { title: "Example", url: "https://example.com/" });
  const command = sandbox.commands.at(-1);
  assert.equal(command, "agent-browser --session eve-abc open https://example.com --json");
});

test("navigate requires a url for goto", async () => {
  resetConfig();
  await assert.rejects(
    () => tools.navigate.execute({ action: "goto" }, fakeCtx(fakeSandbox())),
    /requires a url/,
  );
});

test("navigate includes provider metadata only when opted in", async () => {
  resetConfig({ includeProviderMetadata: true });
  const sandbox = fakeSandbox({
    id: "provider-metadata",
    respond(command) {
      if (command.includes(" session info ")) {
        return {
          stdout: OK({
            provider: "cloud-browser",
            providerMetadata: {
              sessionId: "sess_123",
              dashboard: {
                url: "https://provider.example/sessions/sess_123",
              },
            },
          }),
        };
      }
      return { stdout: OK({ title: "Example", url: "https://example.com/" }) };
    },
  });

  const result = await tools.navigate.execute(
    { action: "goto", url: "https://example.com" },
    fakeCtx(sandbox),
  );

  assert.equal(result.provider, "cloud-browser");
  assert.equal(result.providerMetadata.sessionId, "sess_123");
  assert.equal(
    result.providerMetadata.dashboard.url,
    "https://provider.example/sessions/sess_123",
  );
  assert.equal(sandbox.commands.filter((command) => command.includes(" session info ")).length, 1);
  assert.deepEqual(tools.navigate.toModelOutput(result), {
    type: "json",
    value: { title: "Example", url: "https://example.com/" },
  });
  resetConfig();
});

test("navigate does not request provider metadata unless opted in", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "provider-metadata-opt-out",
    respond(command) {
      if (command.includes(" session info ")) {
        throw new Error("session info should not be called");
      }
      return { stdout: OK({ title: "Example", url: "https://example.com/" }) };
    },
  });

  const result = await tools.navigate.execute(
    { action: "goto", url: "https://example.com" },
    fakeCtx(sandbox),
  );

  assert.deepEqual(result, { title: "Example", url: "https://example.com/" });
  assert.equal(sandbox.commands.filter((command) => command.includes(" session info ")).length, 0);
});

test("navigate skips provider metadata lookup for history actions", async () => {
  resetConfig({ includeProviderMetadata: true });
  const sandbox = fakeSandbox({
    id: "provider-metadata-history",
    respond(command) {
      if (command.includes(" session info ")) {
        return {
          stdout: OK({
            provider: "browserbase",
            providerMetadata: { sessionId: "sess_should_not_fetch" },
          }),
        };
      }
      return { stdout: OK({ title: "Example", url: "https://example.com/" }) };
    },
  });

  const result = await tools.navigate.execute({ action: "back" }, fakeCtx(sandbox));
  assert.deepEqual(result, { title: "Example", url: "https://example.com/" });
  assert.equal(sandbox.commands.filter((command) => command.includes(" session info ")).length, 0);
  resetConfig();
});

test("navigate keeps working when opted-in provider metadata is unavailable", async () => {
  resetConfig({ includeProviderMetadata: true });
  const sandbox = fakeSandbox({
    id: "provider-metadata-unavailable",
    respond(command) {
      if (command.includes(" session info ")) {
        return {
          exitCode: 1,
          stdout: JSON.stringify({ success: false, data: null, error: "unsupported" }),
        };
      }
      return { stdout: OK({ title: "Example", url: "https://example.com/" }) };
    },
  });

  const result = await tools.navigate.execute(
    { action: "goto", url: "https://example.com" },
    fakeCtx(sandbox),
  );
  assert.deepEqual(result, { title: "Example", url: "https://example.com/" });
  resetConfig();
});

test("probes for the binary once per sandbox and skips install when present", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "probe-once" });
  const ctx = fakeCtx(sandbox);
  await tools.close.execute({}, ctx);
  await tools.close.execute({}, ctx);
  const probes = sandbox.commands.filter((command) => command.startsWith("command -v "));
  assert.equal(probes.length, 1);
  assert.ok(!sandbox.commands.some((command) => command.includes("npm install")));
});

test("auto-installs when the binary is missing", async () => {
  resetConfig({ installSystemDependencies: false });
  const commands = [];
  const sandbox = {
    id: "needs-install",
    async run({ command }) {
      commands.push(command);
      if (command.startsWith("command -v ")) {
        return { exitCode: 1, stdout: "", stderr: "" };
      }
      return { exitCode: 0, stdout: OK({}), stderr: "" };
    },
  };
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(commands.some((command) => command.includes("npm install -g agent-browser@")));
  resetConfig();
});

test("skips the install probe when autoInstall is false", async () => {
  resetConfig({ autoInstall: false });
  const sandbox = fakeSandbox({ id: "no-auto" });
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(!sandbox.commands.some((command) => command.startsWith("command -v ")));
  resetConfig();
});

test("applies config-level safety flags to every command", async () => {
  resetConfig({ allowedDomains: ["example.com", "*.example.com"], maxOutputChars: 5000 });
  const sandbox = fakeSandbox({ id: "flags" });
  await tools.close.execute({}, fakeCtx(sandbox));
  const command = sandbox.commands.at(-1);
  assert.ok(command.includes("--allowed-domains 'example.com,*.example.com'"), command);
  assert.ok(command.includes("--max-output 5000"), command);
  resetConfig();
});

test("uses a fixed session name when configured", async () => {
  resetConfig({ session: "shared" });
  const sandbox = fakeSandbox({ id: "fixed-session" });
  await tools.close.execute({}, fakeCtx(sandbox));
  assert.ok(sandbox.commands.at(-1).startsWith("agent-browser --session shared "));
  resetConfig();
});

test("surfaces envelope errors even when the exit code is zero", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "envelope-error",
    respond: () => ({
      exitCode: 0,
      stdout: JSON.stringify({ success: false, data: null, error: "Element not found" }),
    }),
  });
  await assert.rejects(
    () => tools.click.execute({ selector: "#nope", doubleClick: false, newTab: false }, fakeCtx(sandbox)),
    /Element not found/,
  );
});

test("throws a helpful error when no sandbox is available", async () => {
  resetConfig();
  await assert.rejects(
    () => tools.close.execute({}, { getSandbox: async () => null }),
    /require an eve sandbox/,
  );
});

test("wait_for requires at least one condition", async () => {
  resetConfig();
  const ctx = fakeCtx(fakeSandbox());
  await assert.rejects(() => tools.wait_for.execute({}, ctx), /at least one/);
});

test("wait_for runs combined conditions sequentially, load state first", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "combined-waits" });
  await tools.wait_for.execute(
    { loadState: "networkidle", selector: "#app", timeMs: 100 },
    fakeCtx(sandbox),
  );
  const waits = sandbox.commands.filter((command) => command.includes(" wait "));
  assert.equal(waits.length, 3);
  assert.ok(waits[0].includes("wait --load networkidle"), waits[0]);
  assert.ok(waits[1].includes("wait '#app'"), waits[1]);
  assert.ok(waits[2].includes("wait 100"), waits[2]);
});

test("wait_for builds each condition variant", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "waits" });
  const ctx = fakeCtx(sandbox);
  await tools.wait_for.execute({ selector: "#spinner" }, ctx);
  await tools.wait_for.execute({ text: "Welcome" }, ctx);
  await tools.wait_for.execute({ urlPattern: "**/dash" }, ctx);
  await tools.wait_for.execute({ loadState: "networkidle" }, ctx);
  await tools.wait_for.execute({ timeMs: 250 }, ctx);
  const waits = sandbox.commands.filter((command) => command.includes(" wait "));
  assert.ok(waits[0].includes("wait '#spinner'"), waits[0]);
  assert.ok(waits[1].includes("wait --text Welcome"), waits[1]);
  assert.ok(waits[2].includes("wait --url '**/dash'"), waits[2]);
  assert.ok(waits[3].includes("wait --load networkidle"), waits[3]);
  assert.ok(waits[4].includes("wait 250"), waits[4]);
});

test("wait_for caps every condition with a timeout and reports outcomes", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "capped-waits" });
  const outcome = await tools.wait_for.execute({ selector: "#app" }, fakeCtx(sandbox));
  assert.ok(sandbox.commands.at(-1).includes("wait '#app' --timeout 10000"), sandbox.commands.at(-1));
  assert.equal(outcome.satisfied, true);
  assert.equal(outcome.condition, "element #app");
});

test("wait_for treats a timeout as an outcome, not an error", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "timeout-outcome",
    respond: () => ({
      exitCode: 1,
      stdout: JSON.stringify({
        success: false,
        data: null,
        error: "Wait timed out after 10000ms",
      }),
    }),
  });
  const outcome = await tools.wait_for.execute(
    { loadState: "networkidle", timeoutMs: 10_000 },
    fakeCtx(sandbox),
  );
  assert.deepEqual(outcome, {
    condition: "load state networkidle",
    satisfied: false,
    timedOut: true,
  });
});

test("get validates selector and attribute requirements", async () => {
  resetConfig();
  const ctx = fakeCtx(fakeSandbox());
  await assert.rejects(() => tools.get.execute({ property: "text" }, ctx), /requires a selector/);
  await assert.rejects(
    () => tools.get.execute({ property: "attr", selector: "#x" }, ctx),
    /requires an attribute/,
  );
});

test("find requires a value for fill and type", async () => {
  resetConfig();
  const ctx = fakeCtx(fakeSandbox());
  await assert.rejects(
    () => tools.find.execute({ action: "fill", by: "label", exact: false, query: "Email" }, ctx),
    /requires a value/,
  );
});

test("tabs builds each action variant", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "tabs" });
  const ctx = fakeCtx(sandbox);
  await tools.tabs.execute({ action: "list" }, ctx);
  await tools.tabs.execute({ action: "new", label: "docs", url: "https://docs.example.com" }, ctx);
  await tools.tabs.execute({ action: "switch", target: "docs" }, ctx);
  await tools.tabs.execute({ action: "close" }, ctx);
  const commands = sandbox.commands.filter((command) => command.includes(" tab"));
  assert.ok(commands[0].includes(" tab --json"), commands[0]);
  assert.ok(commands[1].includes(" tab new --label docs https://docs.example.com"), commands[1]);
  assert.ok(commands[2].includes(" tab docs"), commands[2]);
  assert.ok(commands[3].includes(" tab close"), commands[3]);
});

test("scroll picks scrollintoview when only a selector is given", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "scrolls" });
  const ctx = fakeCtx(sandbox);
  await tools.scroll.execute({ selector: "#footer" }, ctx);
  await tools.scroll.execute({ direction: "down", pixels: 300 }, ctx);
  const commands = sandbox.commands.filter((command) => command.includes("scroll"));
  assert.ok(commands[0].includes("scrollintoview '#footer'"), commands[0]);
  assert.ok(commands[1].includes("scroll down 300"), commands[1]);
});

test("snapshot retries once on a transient CDP error", async () => {
  resetConfig();
  let attempts = 0;
  const sandbox = fakeSandbox({
    id: "cdp-flake",
    respond: () => {
      attempts += 1;
      if (attempts === 1) {
        return {
          stdout: JSON.stringify({
            success: false,
            data: null,
            error: "CDP error (DOM.describeNode): Object id doesn't reference a Node",
          }),
        };
      }
      return { stdout: OK({ origin: "https://example.com/", snapshot: "- heading" }) };
    },
  });
  const result = await tools.snapshot.execute(
    { compact: true, includeUrls: false, interactiveOnly: false },
    fakeCtx(sandbox),
  );
  assert.equal(attempts, 2);
  assert.deepEqual(result, { origin: "https://example.com/", snapshot: "- heading" });
});

test("snapshot drops ref selectors instead of failing", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "ref-selector",
    respond: () => ({ stdout: OK({ origin: "https://example.com/", snapshot: "- page" }) }),
  });
  const ctx = fakeCtx(sandbox);
  await tools.snapshot.execute(
    { compact: false, includeUrls: false, interactiveOnly: false, selector: "@e1" },
    ctx,
  );
  await tools.snapshot.execute(
    { compact: false, includeUrls: false, interactiveOnly: false, selector: "#main" },
    ctx,
  );
  const snapshots = sandbox.commands.filter((command) => command.includes("snapshot"));
  assert.ok(!snapshots[0].includes("--selector"), snapshots[0]);
  assert.ok(snapshots[1].includes("--selector '#main'"), snapshots[1]);
});

test("snapshot falls back to a whole-page snapshot when the scoped one fails", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "selector-fallback",
    respond: (command) =>
      command.includes("--selector")
        ? {
            exitCode: 1,
            stdout: JSON.stringify({
              success: false,
              data: null,
              error: "Invalid selector 'text=Contact': not a valid selector",
            }),
          }
        : { stdout: OK({ origin: "https://example.com/", snapshot: "- page" }) },
  });
  const result = await tools.snapshot.execute(
    { compact: false, includeUrls: false, interactiveOnly: false, selector: "text=Contact" },
    fakeCtx(sandbox),
  );
  assert.deepEqual(result, { origin: "https://example.com/", snapshot: "- page" });
  const snapshots = sandbox.commands.filter((command) => command.includes("snapshot"));
  assert.ok(snapshots.at(0).includes("--selector"), snapshots[0]);
  assert.ok(!snapshots.at(-1).includes("--selector"), snapshots.at(-1));
});

test("snapshot does not retry non-CDP errors", async () => {
  resetConfig();
  let attempts = 0;
  const sandbox = fakeSandbox({
    id: "real-error",
    respond: () => {
      attempts += 1;
      return {
        stdout: JSON.stringify({ success: false, data: null, error: "No browser session" }),
      };
    },
  });
  await assert.rejects(
    () =>
      tools.snapshot.execute({ compact: true, includeUrls: false, interactiveOnly: false }, fakeCtx(sandbox)),
    /No browser session/,
  );
  assert.equal(attempts, 1);
});

test("screenshot inlines the image and hides it from the model", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "shots",
    respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }),
  });
  sandbox.readBinaryFile = async ({ path }) => {
    assert.equal(path, "/workspace/shot.png");
    return new Uint8Array([137, 80, 78, 71]);
  };
  const output = await tools.screenshot.execute(
    { annotate: false, fullPage: false },
    fakeCtx(sandbox),
  );
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
  const sandbox = fakeSandbox({
    id: "no-file-io",
    respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }),
  });
  const output = await tools.screenshot.execute(
    { annotate: false, fullPage: false },
    fakeCtx(sandbox),
  );
  assert.equal(output.path, "/workspace/shot.png");
  assert.equal(output.imageDataUrl, undefined);
  // Without an inline image the user has no other way to locate the file,
  // so the model keeps the path.
  const modelOutput = await tools.screenshot.toModelOutput(output);
  assert.equal(modelOutput.value.path, "/workspace/shot.png");
});

test("screenshot skips inlining when disabled in config", async () => {
  resetConfig({ inlineScreenshots: false });
  const sandbox = fakeSandbox({
    id: "inline-off",
    respond: () => ({ stdout: OK({ path: "/workspace/shot.png" }) }),
  });
  sandbox.readBinaryFile = async () => new Uint8Array([1]);
  const output = await tools.screenshot.execute(
    { annotate: false, fullPage: false },
    fakeCtx(sandbox),
  );
  assert.equal(output.imageDataUrl, undefined);
  resetConfig();
});

test("errors instead of hanging when the sandbox stops responding", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  resetConfig({ autoInstall: false });
  const sandbox = { id: "wedged", run: () => new Promise(() => {}) };
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
  const sandbox = { id: "wedged-probe", run: () => new Promise(() => {}) };
  const rejection = assert.rejects(
    () => tools.close.execute({}, fakeCtx(sandbox)),
    /The browser install probe did not respond within 180s/,
  );
  // Let the async chain reach the probe's sandbox.run and arm the deadline timer.
  await new Promise((resolve) => setImmediate(resolve));
  t.mock.timers.tick(180_001);
  await rejection;
});

test("wait_for schema drops empty-string conditions", () => {
  const parsed = tools.wait_for.inputSchema.parse({
    jsCondition: "",
    loadState: "",
    selector: "",
    text: "Welcome",
    urlPattern: " ",
  });
  assert.equal(parsed.jsCondition, undefined);
  assert.equal(parsed.loadState, undefined);
  assert.equal(parsed.selector, undefined);
  assert.equal(parsed.urlPattern, undefined);
  assert.equal(parsed.text, "Welcome");
});

test("wait_for runs only the conditions that are actually set", async () => {
  resetConfig();
  const sandbox = fakeSandbox({ id: "wait-only-set" });
  const input = tools.wait_for.inputSchema.parse({ selector: "", text: "Welcome", urlPattern: "" });
  const result = await tools.wait_for.execute(input, fakeCtx(sandbox));
  assert.deepEqual(result, { condition: 'text "Welcome"', satisfied: true, result: {} });
  const waits = sandbox.commands.filter((command) => command.includes(" wait "));
  assert.equal(waits.length, 1);
  assert.ok(waits[0].includes("--text Welcome"));
});

test("wait_for reports load-state timeout as unsatisfied instead of throwing", async () => {
  resetConfig();
  const sandbox = fakeSandbox({
    id: "wait-load-timeout",
    respond: (command) =>
      command.includes("--load")
        ? {
            exitCode: 1,
            stdout: JSON.stringify({
              success: false,
              data: null,
              error: "Timeout waiting for load state: networkidle",
            }),
          }
        : {},
  });
  const result = await tools.wait_for.execute(
    { loadState: "networkidle", timeoutMs: 1000 },
    fakeCtx(sandbox),
  );
  assert.deepEqual(result, {
    condition: "load state networkidle",
    satisfied: false,
    timedOut: true,
  });
});

test("mount factory validates config", () => {
  assert.throws(() => extension({ maxOutputChars: -1 }));
  resetConfig();
});
