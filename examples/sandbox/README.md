# agent-browser sandbox helpers

This example shows how to use `@agent-browser/sandbox` in two places:

- `eve/` is a scaffold-style Eve app with a sandbox-backed browser tool.
- `vercel/` uses `@vercel/sandbox` directly from a Node script.

The package installs `agent-browser` in the sandbox and runs commands there, not in the serverless function or app runtime.

## Eve

Run the Eve app from its own directory:

```bash
cd eve
pnpm install
vercel link --yes --scope <team-or-user> --project <project>
vercel env pull .env.local --yes
pnpm run dev
```

The app follows the project shape created by `eve init --channel-web-nextjs`. Its `agent/sandbox.ts` bootstrap installs Chromium system dependencies, `agent-browser`, and Chrome once into the sandbox template. The `browser_snapshot` tool opens a URL and returns an accessibility snapshot to the agent.

## Vercel Sandbox

Run the direct Vercel example from this directory:

```bash
pnpm install
vercel link --yes --scope <team-or-user> --project <project>
vercel env pull .env.local --yes
node vercel/snapshot-url.mjs https://example.com
```

The script loads `.env.local` when it is present, so local runs can use the OIDC token pulled by the Vercel CLI. On Vercel, the OIDC token is provided by the runtime.

For production, create and reuse a Vercel Sandbox snapshot so fresh requests do not reinstall system dependencies and Chrome.
