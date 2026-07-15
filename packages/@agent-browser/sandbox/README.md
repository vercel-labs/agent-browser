# @agent-browser/sandbox

Helpers for installing and running `agent-browser` inside sandbox runtimes.

This package does not define model tools. Use it from framework-specific tools, agents, route handlers, or jobs that already decide what the browser should do.

## Eve

For Eve agents, use [`@agent-browser/eve`](../eve/) — it mounts the full browser tool set and exposes the sandbox bootstrap helpers from `@agent-browser/eve/sandbox`, with this package as an internal dependency.

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

## Version Pinning

By default, this package installs the matching `agent-browser` version:

```ts
agent-browser@0.30.1
```

Pass `installSpec: "latest"` or another npm spec to override that default.
