# Browserbase + agent-browser eve Example

An [eve](https://eve.dev) browser agent that runs the repository-local [`@agent-browser/eve`](../../../packages/@agent-browser/eve/) extension inside Vercel Sandbox and uses [Browserbase](https://browserbase.com) for the browser. The Next.js UI streams browser activity, renders screenshots inline, and shows the active Browserbase session in an interactive live-view iframe.

## How it works

- The extension is linked from `../../../packages/@agent-browser/eve`, so this example exercises local package changes.
- Vercel Sandbox preinstalls only the `agent-browser` CLI. Browserbase supplies the browser, so the template does not install Chromium or Linux browser libraries.
- `AGENT_BROWSER_PROVIDER=browserbase` makes the CLI create and release remote Browserbase sessions without changing the browser tool interface.
- The Next.js/eve runtime passes `BROWSERBASE_API_KEY` into the sandbox environment. This works on Vercel's Hobby plan, where Sandbox is included but credential-brokering network transformations are unavailable.
- `includeProviderMetadata` lets completed navigation results carry Browserbase debugger URLs to the channel UI while `toModelOutput` removes them from model-visible output.
- The chat renders `debuggerFullscreenUrl` inline in the navigate activity (same place screenshots appear), matching the eve example layout.
- The default `bash`, `web_fetch`, and `web_search` tools are disabled so model-authored commands cannot directly read or reuse the Browserbase credential.

The example does not use `@browserbasehq/sdk` and does not require a Browserbase project ID.

## Requirements

- Node.js 24
- pnpm
- A Vercel account and project with Vercel Sandbox access
- A Browserbase API key from the [Browserbase dashboard](https://browserbase.com/overview)

## Configure and run

Build the linked extension once from the repository root, then install the example dependencies and create the local environment file:

```bash
pnpm -C ../../.. install
pnpm -C ../../../packages/@agent-browser/eve build
pnpm install
cp .env.example .env.local
```

Set `BROWSERBASE_API_KEY` in `.env.local`, then link the Vercel project and pull its development environment:

```bash
vercel link --yes --scope <team-or-user> --project <project>
vercel env pull .env.local --yes
pnpm run dev
```

If `vercel env pull` replaces the file, add `BROWSERBASE_API_KEY` again or set it in the Vercel project's Development environment before pulling. Local development still creates a hosted Vercel Sandbox; there is no local Chromium fallback.

Open the Next.js URL and try:

```text
Open https://example.com, describe the page, and take a screenshot.
```

The recommended interaction loop is:

1. Use `browser__navigate` to open the page; the navigate activity shows the interactive live view when Browserbase returns a debugger URL.
2. Use the iframe directly when a human needs to observe or take control.
3. Call `browser__snapshot` to obtain stable refs such as `[ref=e12]`.
4. Use `@e12` with tools such as `browser__click` or `browser__fill`.
5. Snapshot again after page-changing actions.
6. Call `browser__close` when finished so Browserbase releases the session.

For fixed-purpose agents, uncomment and adapt `allowedDomains` in `agent/extensions/browser.ts` to restrict navigation and subresources.

## Security

The Browserbase debugger URL is a capability-bearing URL. It is intentionally delivered to the authenticated frontend so the iframe can load, but it is hidden from model-visible tool output. Before exposing the app publicly, replace `placeholderAuth()` in `agent/channels/eve.ts` with the application's production authentication policy and ensure unauthorized users cannot read session messages or live-view URLs.

The Hobby-compatible configuration places the real Browserbase API key in the sandbox environment. The browser extension needs it to create remote sessions, but the model must not receive it through environment output, tool results, logs, or committed files. Keep `.env.local` untracked and commit only `.env.example`. For stronger isolation, use a Pro or Enterprise plan and Vercel Sandbox credential brokering so the key never enters the sandbox.

## Deploy

Add `BROWSERBASE_API_KEY` to every Vercel project environment that will run the agent, then deploy from source so eve can prewarm the sandbox template:

```bash
vercel deploy
```

Do not use `vercel deploy --prebuilt`. Eve's hosted build needs to create or reuse the Vercel Sandbox template, and a failed prewarm intentionally fails the build.

## Verify

Run local checks from this standalone workspace:

```bash
pnpm typecheck
pnpm build
```

For a credentialed smoke test, run navigate → live-view interaction → snapshot/ref interaction → screenshot → close. Confirm the Browserbase dashboard records and releases the remote session, the live iframe appears under the navigate activity, screenshots render inline, and the debugger URL never appears in model-authored text.

On a deployment, also verify:

```bash
curl https://<deployment>/eve/v1/health
```

Creating an eve session additionally requires whichever production route authentication policy is configured in `agent/channels/eve.ts`.

## Troubleshooting

- **`BROWSERBASE_API_KEY is required`**: add the key to `.env.local` and the relevant Vercel project environments. The error never prints its value.
- **Vercel Sandbox authentication fails locally**: run `vercel login`, link the project, and pull its environment again.
- **A browser command receives a Browserbase 401/403**: verify the key is active and available as `BROWSERBASE_API_KEY` in the sandbox environment.
- **The browser CLI is missing in a session**: redeploy or change the sandbox revalidation key so eve rebuilds the prewarmed template. Runtime auto-install is intentionally disabled.
- **The live view does not appear**: browsing remains usable when Browserbase's debug endpoint is temporarily unavailable. Check `agent-browser session info --json` in the sandbox and confirm `providerMetadata.debuggerFullscreenUrl` is present. The CLI retries the debug lookup briefly after CDP connect.

This directory is its own pnpm workspace. Run the extension build against the repository workspace first, then run the example's install, development, typecheck, and build commands from this directory.
