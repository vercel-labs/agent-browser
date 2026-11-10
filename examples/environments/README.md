# agent-browser environments

A demo of agent-browser running in a Vercel Sandbox. Pick a URL, take a screenshot or accessibility snapshot, and watch each command execute in real time.

## How it works

The app runs agent-browser and Chrome inside an ephemeral Vercel Sandbox microVM. A Linux VM starts on demand, runs the agent-browser commands, and shuts down, so the app's own serverless functions never bundle Chromium.

The UI streams progress via Server-Sent Events so you can see each step as it runs (sandbox creation, browser startup, navigation, screenshot/snapshot, cleanup).

## Getting started

```bash
cd examples/environments
pnpm install
pnpm dev
```

For local development, set `VERCEL_TOKEN`, `VERCEL_TEAM_ID`, and `VERCEL_PROJECT_ID` in `.env.local` so the Sandbox SDK can authenticate.

## Sandbox snapshots

Without a snapshot, each sandbox run installs system dependencies, agent-browser, and Chromium from scratch. A sandbox snapshot is a saved VM image with everything preinstalled, so the sandbox boots from the image instead of installing. Sandbox snapshots are unrelated to agent-browser's `snapshot` command, which returns a page's accessibility tree.

Create a sandbox snapshot by running the helper script once:

```bash
npx tsx scripts/create-snapshot.ts
# Output: AGENT_BROWSER_SNAPSHOT_ID=snap_xxxxxxxxxxxx
```

Add the ID to your Vercel project environment variables or `.env.local`. Recommended for production.

## Environment variables

| Variable | Description |
|---|---|
| `AGENT_BROWSER_SNAPSHOT_ID` | Sandbox snapshot ID to boot from instead of installing (see above) |
| `VERCEL_TOKEN` | Vercel personal access token (for local dev; OIDC is automatic on Vercel) |
| `VERCEL_TEAM_ID` | Vercel team ID (for local dev) |
| `VERCEL_PROJECT_ID` | Vercel project ID (for local dev) |
| `KV_REST_API_URL` | Upstash Redis URL for rate limiting (optional) |
| `KV_REST_API_TOKEN` | Upstash Redis token for rate limiting (optional) |
| `RATE_LIMIT_PER_MINUTE` | Max requests per minute per IP (default: 10) |
| `RATE_LIMIT_PER_DAY` | Max requests per day per IP (default: 100) |

## Project structure

```
examples/environments/
  app/
    page.tsx                  # Demo UI with streaming progress
    actions/browse.ts         # Server action (env status check)
    api/browse/route.ts       # Streaming SSE endpoint
  lib/
    agent-browser-sandbox.ts  # Vercel Sandbox client with progress callbacks
    constants.ts              # Allowed URLs
    rate-limit.ts             # Upstash rate limiting
  scripts/
    create-snapshot.ts        # Create sandbox snapshot
```
