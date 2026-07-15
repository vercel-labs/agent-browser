# Agent Browser Eve Example

This is a scaffold-style [eve](https://eve.dev) app based on `eve init --channel-web-nextjs`. Its agent gets a full browser tool set from the [`@agent-browser/eve`](../../packages/@agent-browser/eve/) extension.

- `agent/extensions/browser.ts` mounts the extension, composing ~20 tools into the agent under the `browser` namespace (`browser__navigate`, `browser__snapshot`, `browser__click`, ...).
- `agent/sandbox.ts` pre-installs Chromium system dependencies, `agent-browser`, and Chrome into the sandbox template with the `@agent-browser/eve/sandbox` helpers, so sessions never pay the install cost. Without this bootstrap the extension still works — it auto-installs on first tool use — just slower on fresh sandboxes.

## Run Locally

```bash
pnpm install
vercel link --yes --scope <team-or-user> --project <project>
vercel env pull .env.local --yes
pnpm run dev
```

Open the local Next.js URL and ask the agent to inspect a page, for example:

```text
Inspect https://example.com and summarize what is visible.
```

The browser tools run `agent-browser` inside the Vercel Sandbox, not in the Next.js runtime.

Note: this directory is its own pnpm workspace; run `pnpm install` from here, not from the repository root.
