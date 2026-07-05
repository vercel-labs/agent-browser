# Agent Browser Eve Sandbox Example

This is a scaffold-style Eve app based on `eve init --channel-web-nextjs`. It adds an `agent-browser` sandbox bootstrap and a browser snapshot tool.

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

The sandbox bootstrap installs Chromium system dependencies, `agent-browser`, and Chrome into the Vercel Sandbox template. The tool runs `agent-browser open` and `agent-browser snapshot` inside that sandbox, not in the Next.js runtime.
