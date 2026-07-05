---
name: vercel-sandbox
description: Run agent-browser + Chrome inside Vercel Sandbox microVMs for browser automation from any Vercel-deployed app. Use when the user needs browser automation in a Vercel app (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.), wants to run headless Chrome without binary size limits, needs persistent browser sessions across commands, or wants ephemeral isolated browser environments. Triggers include "Vercel Sandbox browser", "microVM Chrome", "agent-browser in sandbox", "browser automation on Vercel", or any task requiring Chrome in a Vercel Sandbox.
---

# Browser Automation with Vercel Sandbox

Run agent-browser + headless Chrome inside ephemeral Vercel Sandbox microVMs. A Linux VM spins up on demand, executes browser commands, and shuts down. Works with any Vercel-deployed framework (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.).

## Dependencies

```bash
pnpm add @agent-browser/sandbox @vercel/sandbox
```

The sandbox VM needs system dependencies for Chromium plus agent-browser itself. The `@agent-browser/sandbox` helpers install them by default for fresh sandboxes and use sandbox snapshots (below) for sub-second startup. Pass `installSystemDependencies: false` only when the sandbox image already provides Chromium's required libraries.

## Core Pattern

```ts
import {
  createAgentBrowserSnapshot,
  runAgentBrowserCommand,
  withAgentBrowserSandbox,
  type VercelSandboxSession,
} from "@agent-browser/sandbox/vercel";

async function withBrowser<T>(
  fn: (sandbox: VercelSandboxSession) => Promise<T>,
): Promise<T> {
  return withAgentBrowserSandbox(fn);
}
```

## Screenshot

The `screenshot --json` command saves to a file and returns the path. Read the file back as base64:

```ts
export async function screenshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await runAgentBrowserCommand(sandbox, ["open", url]);

    const titleResult = await runAgentBrowserCommand<{ data?: { title?: string } }>(sandbox, [
      "get", "title",
    ]);
    const title = titleResult.json?.data?.title || url;

    const ssResult = await runAgentBrowserCommand<{ data?: { path?: string } }>(sandbox, [
      "screenshot",
    ]);
    const ssPath = ssResult.json?.data?.path;
    if (!ssPath) throw new Error("Screenshot did not return a file path.");
    const b64Result = await sandbox.runCommand("base64", ["-w", "0", ssPath]);
    const screenshot = (await b64Result.stdout()).trim();

    await runAgentBrowserCommand(sandbox, ["close"], { json: false });

    return { title, screenshot };
  });
}
```

## Accessibility Snapshot

```ts
export async function snapshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await runAgentBrowserCommand(sandbox, ["open", url]);

    const titleResult = await runAgentBrowserCommand<{ data?: { title?: string } }>(sandbox, [
      "get", "title",
    ]);
    const title = titleResult.json?.data?.title || url;

    const snapResult = await runAgentBrowserCommand(sandbox, ["snapshot", "-i", "-c"], {
      json: false,
    });

    await runAgentBrowserCommand(sandbox, ["close"], { json: false });

    return { title, snapshot: snapResult.stdout };
  });
}
```

## Multi-Step Workflows

The sandbox persists between commands, so you can run full automation sequences:

```ts
export async function fillAndSubmitForm(url: string, data: Record<string, string>) {
  return withBrowser(async (sandbox) => {
    await runAgentBrowserCommand(sandbox, ["open", url]);

    const snapResult = await runAgentBrowserCommand(sandbox, ["snapshot", "-i"], {
      json: false,
    });
    const snapshot = snapResult.stdout;
    // Parse snapshot to find element refs...

    for (const [ref, value] of Object.entries(data)) {
      await runAgentBrowserCommand(sandbox, ["fill", ref, value]);
    }

    await runAgentBrowserCommand(sandbox, ["click", "@e5"]);
    await runAgentBrowserCommand(sandbox, ["wait", "--load", "networkidle"]);

    const ssResult = await runAgentBrowserCommand<{ data?: { path?: string } }>(sandbox, [
      "screenshot",
    ]);
    const ssPath = ssResult.json?.data?.path;
    if (!ssPath) throw new Error("Screenshot did not return a file path.");
    const b64Result = await sandbox.runCommand("base64", ["-w", "0", ssPath]);
    const screenshot = (await b64Result.stdout()).trim();

    await runAgentBrowserCommand(sandbox, ["close"], { json: false });

    return { screenshot };
  });
}
```

## Sandbox Snapshots (Fast Startup)

A **sandbox snapshot** is a saved VM image of a Vercel Sandbox with system dependencies + agent-browser + Chromium already installed. Think of it like a Docker image: instead of installing dependencies from scratch every time, the sandbox boots from the pre-built image.

This is unrelated to agent-browser's *accessibility snapshot* feature (`agent-browser snapshot`), which dumps a page's accessibility tree. A sandbox snapshot is a Vercel infrastructure concept for fast VM startup.

Without a sandbox snapshot, each run installs system deps + agent-browser + Chromium (~30s). With one, startup is sub-second.

### Creating a sandbox snapshot

The snapshot must include system dependencies (via `dnf`), agent-browser, and Chromium:

```ts
const snapshotId = await createAgentBrowserSnapshot();
```

Run this once, then set the environment variable:

```bash
AGENT_BROWSER_SNAPSHOT_ID=snap_xxxxxxxxxxxx
```

A helper script is available in the demo app:

```bash
npx tsx examples/environments/scripts/create-snapshot.ts
```

Recommended for any production deployment using the Sandbox pattern.

## Authentication

On Vercel deployments, the Sandbox SDK authenticates automatically via OIDC. For local development or explicit control, set:

```bash
VERCEL_TOKEN=<personal-access-token>
VERCEL_TEAM_ID=<team-id>
VERCEL_PROJECT_ID=<project-id>
```

These are spread into `Sandbox.create()` calls. When absent, the SDK falls back to `VERCEL_OIDC_TOKEN` (automatic on Vercel).

## Scheduled Workflows (Cron)

Combine with Vercel Cron Jobs for recurring browser tasks:

```ts
// app/api/cron/route.ts  (or equivalent in your framework)
export async function GET() {
  const result = await withBrowser(async (sandbox) => {
    await sandbox.runCommand("agent-browser", ["open", "https://example.com/pricing"]);
    const snap = await sandbox.runCommand("agent-browser", ["snapshot", "-i", "-c"]);
    await sandbox.runCommand("agent-browser", ["close"]);
    return await snap.stdout();
  });

  // Process results, send alerts, store data...
  return Response.json({ ok: true, snapshot: result });
}
```

```json
// vercel.json
{ "crons": [{ "path": "/api/cron", "schedule": "0 9 * * *" }] }
```

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `AGENT_BROWSER_SNAPSHOT_ID` | No (but recommended) | Pre-built sandbox snapshot ID for sub-second startup (see above) |
| `VERCEL_TOKEN` | No | Vercel personal access token (for local dev; OIDC is automatic on Vercel) |
| `VERCEL_TEAM_ID` | No | Vercel team ID (for local dev) |
| `VERCEL_PROJECT_ID` | No | Vercel project ID (for local dev) |

## Framework Examples

The pattern works identically across frameworks. The only difference is where you put the server-side code:

| Framework | Server code location |
|---|---|
| Next.js | Server actions, API routes, route handlers |
| SvelteKit | `+page.server.ts`, `+server.ts` |
| Nuxt | `server/api/`, `server/routes/` |
| Remix | `loader`, `action` functions |
| Astro | `.astro` frontmatter, API routes |

## Example

See `examples/environments/` in the agent-browser repo for a working app with the Vercel Sandbox pattern, including a sandbox snapshot creation script, streaming progress UI, and rate limiting.
