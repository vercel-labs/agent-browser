import assert from "node:assert/strict";
import test from "node:test";

import { AgentBrowserMcpError, connectAgentBrowserMcp } from "../dist/index.js";
import { startAgentBrowserMcp } from "../dist/vercel.js";

/** An in-memory MCP server; `respond` maps a request to a reply, or `undefined` to never answer. */
function fakeServer(respond = (message) => ({ result: { echo: message.params } })) {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const received = [];
  let stdoutController;
  let buffer = "";
  let exit;
  const exited = new Promise((resolve) => {
    exit = resolve;
  });
  const reply = (message) => {
    const response = respond(message);
    if (response === undefined) return;
    // Split each reply across two chunks to exercise line buffering.
    const line = `${JSON.stringify({ jsonrpc: "2.0", id: message.id, ...response })}\n`;
    stdoutController.enqueue(encoder.encode(line.slice(0, 5)));
    stdoutController.enqueue(encoder.encode(line.slice(5)));
  };
  return {
    received,
    process: {
      stdin: new WritableStream({
        write(chunk) {
          buffer += decoder.decode(chunk);
          let newline;
          while ((newline = buffer.indexOf("\n")) !== -1) {
            const message = JSON.parse(buffer.slice(0, newline));
            buffer = buffer.slice(newline + 1);
            received.push(message);
            if (message.method === "initialize") {
              reply({ ...message, initialize: true });
            } else if (message.id !== undefined) {
              reply(message);
            }
          }
        },
        close() {
          stdoutController.close();
          exit({ exitCode: 0 });
        },
      }),
      stdout: new ReadableStream({
        start(controller) {
          stdoutController = controller;
        },
      }),
      wait: () => exited,
    },
    kill() {
      stdoutController.close();
      exit({ exitCode: 137 });
    },
  };
}

function initializeOr(respond) {
  return (message) =>
    message.initialize
      ? { result: { serverInfo: { name: "agent-browser", version: "test" } } }
      : respond(message);
}

test("initializes, calls tools, and closes with EOF", async () => {
  const server = fakeServer(initializeOr((message) => ({ result: { content: [], echo: message.params } })));
  const client = await connectAgentBrowserMcp(server.process);

  assert.equal(client.serverInfo.version, "test");
  const [open, url] = await Promise.all([
    client.callTool("agent_browser_open", { url: "https://example.com" }),
    client.callTool("agent_browser_get_url"),
  ]);
  assert.deepEqual(open.echo, { arguments: { url: "https://example.com" }, name: "agent_browser_open" });
  assert.deepEqual(url.echo, { arguments: {}, name: "agent_browser_get_url" });
  assert.deepEqual(
    server.received.map((message) => message.method),
    ["initialize", "notifications/initialized", "tools/call", "tools/call"],
  );
  assert.deepEqual(await client.close(), { exitCode: 0 });
});

test("rejects JSON-RPC errors with the server's code", async () => {
  const server = fakeServer(initializeOr(() => ({ error: { code: -32601, message: "nope" } })));
  const client = await connectAgentBrowserMcp(server.process);

  await assert.rejects(client.request("resources/list"), (error) => {
    assert.ok(error instanceof AgentBrowserMcpError);
    assert.equal(error.code, -32601);
    return true;
  });
  await client.close();
});

test("times out a request the server never answers", async () => {
  const server = fakeServer(initializeOr(() => undefined));
  const client = await connectAgentBrowserMcp(server.process, { timeout: 20 });

  await assert.rejects(client.callTool("agent_browser_click"), /tools\/call timed out after 20ms/);
  await client.close();
});

test("aborts a pending request and fails pending requests when the server exits", async () => {
  const server = fakeServer(initializeOr(() => undefined));
  const client = await connectAgentBrowserMcp(server.process);
  const controller = new AbortController();

  const aborted = client.callTool("agent_browser_click", {}, { signal: controller.signal });
  controller.abort(new Error("stop"));
  await assert.rejects(aborted, /stop/);

  const orphan = client.callTool("agent_browser_click");
  server.kill();
  await assert.rejects(orphan, /closed its output/);
});

test("starts agent-browser mcp over Vercel Sandbox stdin", async () => {
  const server = fakeServer(initializeOr((message) => ({ result: { echo: message.params } })));
  const writer = server.process.stdin.getWriter();
  const calls = [];
  const sandbox = {
    async runCommand(options) {
      calls.push(options);
      return {
        closeStdin: () => writer.close(),
        kill: async () => server.kill(),
        async *logs() {
          yield { data: "starting\n", stream: "stderr" };
          for await (const chunk of server.process.stdout) {
            yield { data: new TextDecoder().decode(chunk), stream: "stdout" };
          }
        },
        wait: () => server.process.wait(),
        writeStdin: (data) => writer.write(data),
      };
    },
  };

  const client = await startAgentBrowserMcp(sandbox, { args: ["--tools", "core"] });
  const result = await client.callTool("agent_browser_snapshot");

  assert.deepEqual(calls, [
    { args: ["mcp", "--tools", "core"], cmd: "agent-browser", detached: true, stdin: true },
  ]);
  assert.deepEqual(result.echo, { arguments: {}, name: "agent_browser_snapshot" });
  assert.deepEqual(await client.close(), { exitCode: 0 });
});

test("never sends an already-aborted or unserializable request", async () => {
  const server = fakeServer(initializeOr((message) => ({ result: message.params })));
  const client = await connectAgentBrowserMcp(server.process);
  const controller = new AbortController();
  controller.abort(new Error("stop"));

  await assert.rejects(client.callTool("agent_browser_click", {}, { signal: controller.signal }), /stop/);
  await assert.rejects(client.callTool("agent_browser_click", { n: 1n }), TypeError);
  assert.deepEqual(
    server.received.map((message) => message.method),
    ["initialize", "notifications/initialized"],
  );
  await client.close();
});

test("releases both streams when initialization fails", async () => {
  const server = fakeServer(() => undefined);

  await assert.rejects(connectAgentBrowserMcp(server.process, { timeout: 20 }), /initialize timed out/);
  assert.equal(server.process.stdin.locked, false);
  assert.equal(server.process.stdout.locked, false);
});

test("rejects a failed initialization even when the first write stalls", async () => {
  const process = {
    stdin: new WritableStream({ write: () => new Promise(() => {}) }),
    stdout: new ReadableStream(),
    wait: () => new Promise(() => {}),
  };

  await assert.rejects(connectAgentBrowserMcp(process, { timeout: 20 }), /initialize timed out/);
  assert.equal(process.stdin.locked, false);
  assert.equal(process.stdout.locked, false);
});

test("close rejects within the timeout when a write stalls", async () => {
  const encoder = new TextEncoder();
  let stdout;
  let writes = 0;
  const process = {
    stdin: new WritableStream({
      write() {
        if (++writes > 1) return new Promise(() => {});
        stdout.enqueue(
          encoder.encode(`${JSON.stringify({ id: 1, jsonrpc: "2.0", result: { serverInfo: {} } })}\n`),
        );
      },
    }),
    stdout: new ReadableStream({
      start(controller) {
        stdout = controller;
      },
    }),
    wait: () => new Promise(() => {}),
  };
  const client = await connectAgentBrowserMcp(process, { timeout: 20 });

  await assert.rejects(client.close(), /close timed out after 20ms/);
});
