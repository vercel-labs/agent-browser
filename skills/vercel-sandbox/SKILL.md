---
name: vercel-sandbox
description: Run agent-browser + Chrome inside Vercel Sandbox microVMs for browser automation from any Vercel-deployed app. Use when the user needs browser automation in a Vercel app (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.), wants to run headless Chrome without binary size limits, needs persistent browser sessions across commands, or wants ephemeral isolated browser environments. Triggers include "Vercel Sandbox browser", "microVM Chrome", "agent-browser in sandbox", "browser automation on Vercel", or any task requiring Chrome in a Vercel Sandbox.
---

# Browser Automation with Vercel Sandbox

Run agent-browser + headless Chrome inside ephemeral Vercel Sandbox microVMs. A Linux VM spins up on demand, executes browser commands, and shuts down. Works with any Vercel-deployed framework (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.).

## When to Use Sandbox vs Serverless

| | Vercel Sandbox | Serverless (`@sparticuz/chromium`) |
|---|---|---|
| Binary size limit | None | 50MB compressed |
| Session persistence | Yes, within a sandbox lifetime | No, fresh browser per request |
| Multi-step workflows | Yes, run sequences of commands | Single request only |
| Startup time | ~30s cold, sub-second with sandbox snapshot | ~2-3s |
| Framework support | Any (Next.js, SvelteKit, Nuxt, etc.) | Next.js (or any Node.js serverless) |

Use Sandbox when you need full Chrome, multi-step workflows, or longer execution times. Use serverless when you need fast single-request screenshots/snapshots.

## Dependencies

```bash
pnpm add @vercel/sandbox
```

The sandbox VM installs agent-browser and Chrome on first run. Use sandbox snapshots (below) to skip this step.

## Core Pattern

```ts
import { Sandbox } from "@vercel/sandbox";

async function withBrowser<T>(
  fn: (sandbox: InstanceType<typeof Sandbox>) => Promise<T>,
): Promise<T> {
  const snapshotId = process.env.AGENT_BROWSER_SNAPSHOT_ID;

  const sandbox = snapshotId
    ? await Sandbox.create({
        source: { type: "snapshot", snapshotId },
        timeout: 120_000,
      })
    : await Sandbox.create({ runtime: "node24", timeout: 120_000 });

  if (!snapshotId) {
    await sandbox.runCommand("npm", ["install", "-g", "agent-browser"]);
    await sandbox.runCommand("npx", ["agent-browser", "install"]);
  }

  try {
    return await fn(sandbox);
  } finally {
    await sandbox.stop();
  }
}
```

## Screenshot

```ts
export async function screenshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("agent-browser", ["open", url]);

    const titleResult = await sandbox.runCommand("agent-browser", [
      "get", "title", "--json",
    ]);
    const title = JSON.parse(await titleResult.stdout())?.data?.title || url;

    const ssResult = await sandbox.runCommand("agent-browser", [
      "screenshot", "--json",
    ]);
    const screenshot = JSON.parse(await ssResult.stdout())?.data?.base64 || "";

    await sandbox.runCommand("agent-browser", ["close"]);

    return { title, screenshot };
  });
}
```

## Accessibility Snapshot

```ts
export async function snapshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("agent-browser", ["open", url]);

    const titleResult = await sandbox.runCommand("agent-browser", [
      "get", "title", "--json",
    ]);
    const title = JSON.parse(await titleResult.stdout())?.data?.title || url;

    const snapResult = await sandbox.runCommand("agent-browser", [
      "snapshot", "-i", "-c",
    ]);
    const snapshot = await snapResult.stdout();

    await sandbox.runCommand("agent-browser", ["close"]);

    return { title, snapshot };
  });
}
```

## Multi-Step Workflows

The sandbox persists between commands, so you can run full automation sequences:

```ts
export async function fillAndSubmitForm(url: string, data: Record<string, string>) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("agent-browser", ["open", url]);

    const snapResult = await sandbox.runCommand("agent-browser", [
      "snapshot", "-i",
    ]);
    const snapshot = await snapResult.stdout();
    // Parse snapshot to find element refs...

    for (const [ref, value] of Object.entries(data)) {
      await sandbox.runCommand("agent-browser", ["fill", ref, value]);
    }

    await sandbox.runCommand("agent-browser", ["click", "@e5"]);
    await sandbox.runCommand("agent-browser", ["wait", "--load", "networkidle"]);

    const ssResult = await sandbox.runCommand("agent-browser", [
      "screenshot", "--json",
    ]);
    const screenshot = JSON.parse(await ssResult.stdout())?.data?.base64 || "";

    await sandbox.runCommand("agent-browser", ["close"]);

    return { screenshot };
  });
}
```

## Sandbox Snapshots (Fast Startup)

A **sandbox snapshot** is a saved VM image of a Vercel Sandbox with agent-browser + Chromium already installed. Think of it like a Docker image -- instead of installing dependencies from scratch every time, the sandbox boots from the pre-built image.

This is unrelated to agent-browser's *accessibility snapshot* feature (`agent-browser snapshot`), which dumps a page's accessibility tree. A sandbox snapshot is a Vercel infrastructure concept for fast VM startup.

Without a sandbox snapshot, each run installs agent-browser + Chromium (~30s). With one, startup is sub-second.

### Creating a sandbox snapshot

```ts
import { Sandbox } from "@vercel/sandbox";

async function createSnapshot(): Promise<string> {
  const sandbox = await Sandbox.create({
    runtime: "node24",
    timeout: 300_000,
  });

  await sandbox.runCommand("npm", ["install", "-g", "agent-browser"]);
  await sandbox.runCommand("npx", ["agent-browser", "install"]);

  const snapshot = await sandbox.snapshot();
  return snapshot.snapshotId;
}
```

Run this once, then set the environment variable:

```bash
AGENT_BROWSER_SNAPSHOT_ID=snap_xxxxxxxxxxxx
```

A helper script is available in the demo app:

```bash
npx tsx examples/demo/scripts/create-snapshot.ts
```

Recommended for any production deployment using the Sandbox pattern.

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

The Vercel Sandbox SDK handles OIDC authentication automatically when deployed on Vercel. For local development, run `vercel link` and `vercel env pull` to get the required tokens.

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

See `examples/demo/` in the agent-browser repo for a working app with the Vercel Sandbox pattern, including a sandbox snapshot creation script and demo UI.
