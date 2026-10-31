# @agent-browser/sandbox

Helpers for installing and running `agent-browser` inside sandbox runtimes.

This package does not define model tools. Use it from framework-specific tools, agents, route handlers, or jobs that already decide what the browser should do.

## eve

For eve agents, use [`@agent-browser/eve`](../eve/) — it mounts the full browser tool set and exposes the sandbox bootstrap helpers from `@agent-browser/eve/sandbox`, with this package as an internal dependency.

## Vercel Sandbox

Install `@vercel/sandbox` in the consuming app:

```bash
pnpm add @vercel/sandbox
```

Then use the Vercel provider entry:

```ts
import { runAgentBrowserCommand, withAgentBrowserSandbox } from "@agent-browser/sandbox/vercel";

const snapshot = await withAgentBrowserSandbox(async (sandbox) => {
  await runAgentBrowserCommand(sandbox, ["open", "https://example.com"]);
  const result = await runAgentBrowserCommand(sandbox, ["snapshot", "-i", "-c"], {
    json: false,
  });
  return result.stdout;
});
```

The Vercel helpers install browser system dependencies by default. Pass `installSystemDependencies: false` only when the sandbox image already provides Chromium's required libraries.

Set `AGENT_BROWSER_SNAPSHOT_ID` to boot from a prebuilt Vercel Sandbox snapshot. Without a snapshot, the helper installs system dependencies, `agent-browser`, and Chrome on first boot.

### One MCP server per sandbox

`startAgentBrowserMcp` keeps one `agent-browser mcp` running over stdin, so tool calls reuse the same process instead of starting a command each time. It needs `@vercel/sandbox` 3.7 or later.

```ts
import { createAgentBrowserSandbox, startAgentBrowserMcp } from "@agent-browser/sandbox/vercel";

const sandbox = await createAgentBrowserSandbox();
const mcp = await startAgentBrowserMcp(sandbox, { timeout: 60_000 });
await mcp.callTool("agent_browser_open", { url: "https://example.com" });
const snapshot = await mcp.callTool("agent_browser_snapshot", { interactive: true });
await mcp.close();
await sandbox.stop();
```

`timeout` applies to each request and has no default. For another runtime, pass a process with `stdin` and `stdout` streams to `connectAgentBrowserMcp` from `@agent-browser/sandbox`.

## MCP tool catalog

`@agent-browser/sandbox/mcp-tools.json` lists every `agent-browser mcp` tool with its title, description, input schema, annotations, and startup profiles, generated from the CLI of the same version. Use it to build tools for runtimes that cannot run `tools/list`, and call them with `agent-browser mcp call <tool> '<json>'`. Run `pnpm mcp-tools:sync` after changing MCP tools; a Rust test fails while the file is out of date.

## Version Pinning

By default, this package installs the `agent-browser` version that matches its own, such as `agent-browser@0.41.0` for `@agent-browser/sandbox@0.41.0`.

Pass `installSpec: "latest"` or another npm spec to override that default.
