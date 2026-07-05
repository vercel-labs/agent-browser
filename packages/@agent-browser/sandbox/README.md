# @agent-browser/sandbox

Helpers for installing and running `agent-browser` inside sandbox runtimes.

This package does not define model tools. Use it from framework-specific tools, agents, route handlers, or jobs that already decide what the browser should do.

## Eve

```ts
import { defineSandbox } from "eve/sandbox";
import { vercel } from "eve/sandbox/vercel";
import { agentBrowserRevalidationKey, installAgentBrowser } from "@agent-browser/sandbox/eve";

export default defineSandbox({
  backend: vercel({ runtime: "node24", resources: { vcpus: 2 } }),
  revalidationKey: () => agentBrowserRevalidationKey(),
  async bootstrap({ use }) {
    const sandbox = await use();
    await installAgentBrowser(sandbox);
  },
});
```

Then call `agent-browser` from an Eve tool:

```ts
import { runAgentBrowser } from "@agent-browser/sandbox/eve";

const result = await runAgentBrowser(ctx, ["open", "https://example.com"]);
```

The Eve helper derives a short, stable `agent-browser` session name from the Eve sandbox id. Pass `session` to `runAgentBrowser` when multiple independent browser sessions should share one sandbox.

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

The Eve and Vercel helpers install browser system dependencies by default. Pass `installSystemDependencies: false` only when the sandbox image already provides Chromium's required libraries.

Set `AGENT_BROWSER_SNAPSHOT_ID` to boot from a prebuilt Vercel Sandbox snapshot. Without a snapshot, the helper installs system dependencies, `agent-browser`, and Chrome on first boot.

## Version Pinning

By default, this package installs the matching `agent-browser` version:

```ts
agent-browser@0.30.1
```

Pass `installSpec: "latest"` or another npm spec to override that default.
