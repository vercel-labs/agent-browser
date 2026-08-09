# agent-browser sandbox helpers

This example uses `@agent-browser/sandbox` with `@vercel/sandbox` directly from a Node script. For an eve agent with the full browser tool set, see the [eve example](../eve/), which mounts the `@agent-browser/eve` extension.

The package installs `agent-browser` in the sandbox and runs commands there, not in the serverless function or app runtime.

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
