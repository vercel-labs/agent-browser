# agent-browser Demo

A visual demo of agent-browser's core capabilities. Enter a URL, pick a compute environment, and take a screenshot or accessibility snapshot.

## Environments

- **Serverless Function** -- `@sparticuz/chromium` + `puppeteer-core` running directly inside a Vercel serverless function
- **Vercel Sandbox** -- agent-browser + Chrome in an ephemeral Linux microVM

## Getting Started

```bash
cd examples/demo
pnpm install
pnpm dev
```

## Serverless Function

Runs headless Chrome directly in the serverless function. On Vercel, `@sparticuz/chromium` provides the binary automatically. Locally, the app finds your system Chrome or uses `CHROMIUM_PATH`.

## Vercel Sandbox

Spins up a Linux microVM on demand, installs agent-browser + Chrome, runs the commands, and shuts down. No binary size limits.

### Sandbox snapshots

Without optimization, each Sandbox run installs agent-browser + Chromium from scratch (~30s). A **sandbox snapshot** is a saved VM image with everything pre-installed -- the sandbox boots from the image instead of installing, bringing startup down to sub-second. (This is unrelated to agent-browser's *accessibility snapshot* feature, which dumps a page's accessibility tree.)

Create a sandbox snapshot by running the helper script once:

```bash
npx tsx scripts/create-snapshot.ts
# Output: AGENT_BROWSER_SNAPSHOT_ID=snap_xxxxxxxxxxxx
```

Add the ID to your Vercel project environment variables or `.env.local`. Recommended for production.

## Vercel Configuration

The serverless function pattern requires specific Next.js config so the `@sparticuz/chromium` brotli binaries are included in the deployment bundle:

```ts
// next.config.ts
const nextConfig = {
  serverExternalPackages: ["@sparticuz/chromium"],
  outputFileTracingIncludes: {
    "/**": ["./node_modules/@sparticuz/chromium/**"],
  },
};
```

This project uses pnpm. The `.npmrc` includes `public-hoist-pattern[]=@sparticuz/chromium` to ensure Vercel's file tracing resolves the binaries through pnpm's symlinked `node_modules`.

## Environment Variables

| Variable | Environment | Description |
|---|---|---|
| `CHROMIUM_PATH` | Serverless | Path to local Chrome/Chromium binary (auto-detected on Vercel) |
| `AGENT_BROWSER_SNAPSHOT_ID` | Sandbox | Sandbox snapshot ID for sub-second startup (see above) |

## Project Structure

```
examples/demo/
  app/
    page.tsx                  # Demo UI
    actions/browse.ts         # Server actions (all environments)
    api/browse/route.ts       # API route for programmatic access
  lib/
    agent-browser.ts          # Serverless: @sparticuz/chromium + puppeteer-core
    agent-browser-sandbox.ts  # Sandbox: Vercel Sandbox client
  scripts/
    create-snapshot.ts        # Create sandbox snapshot
```
