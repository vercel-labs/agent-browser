# Partial Pre-Rendering (PPR) workflows

How to drive `agent-browser react suspense` against a Next.js app running
with Partial Pre-Rendering enabled, both for the direct-load HTML shell
and the client-nav "instant" shell.

**Related**: [SKILL.md](../SKILL.md), [hydration.md](hydration.md).

## Prerequisites

- `cacheComponents` enabled in `next.config` (otherwise the shell has nothing
  pre-rendered to show).
- Browser launched with `agent-browser open --enable react-devtools <url>`.
- Next.js v16.2.0-canary.37 or later: bundled docs live at
  `node_modules/next/dist/docs/`. Read the relevant doc before doing any
  non-trivial PPR or Cache Components work - training data may be outdated.
  See https://nextjs.org/docs/app/guides/ai-agents for background.

## The lock cookie

PPR lock is a cookie-based protocol implemented in Next.js dev. Setting the
cookie `next-instant-navigation-testing` tells the dev server to respond
with the static shell only (no dynamic content), mimicking a prefetched
instant navigation.

Cookie value is a JSON array `[0, "pRANDOM"]` where the second element is a
random per-call nonce. Any unique value works locally.

```bash
HOST=$(agent-browser get url | awk -F[/:] '{print $4}')
RAND="p$(od -An -N4 -tu4 /dev/urandom | tr -d ' ')"
agent-browser cookies set next-instant-navigation-testing \
  "[0,\"$RAND\"]" --domain "$HOST"
```

To release the lock, clear the cookie:

```bash
agent-browser cookies clear
```

(or more surgically re-set with an expired timestamp).

## Growing the HTML shell (direct page load)

The HTML shell is what a direct page load delivers before any JavaScript
runs. A meaningful shell is the real component tree with small, local
fallbacks where data is genuinely pending.

**Workflow:**

1. Launch with the React hook, navigate to the target URL.
2. Set the lock cookie scoped to the dev host.
3. `agent-browser navigate <target>` - forces a fresh server render under
   the lock, so the response is the static shell.
4. `agent-browser screenshot "HTML shell - locked"` - visual verification.
5. `agent-browser react suspense` - reads the boundary report.
6. Clear the cookie, reload unlocked, check `bash templates/next-mcp.sh
   <origin> get_errors` for any issues the lock masked.
7. Fix the top-most blocker in the report, let HMR pick it up, re-lock,
   re-navigate, compare.

Work the report top-down. For the component named as the top hole's
primary blocker: can the dynamic access move into a child? If yes, move it
- this component becomes sync and rejoins the shell. Follow the access
down and ask again.

When you reach a component where the blocker can't move lower, there are
two exits - wrap in a Suspense boundary, or cache it for prerender. Both
are human calls. Escalate boundary placement and caching decisions to the
user.

## Optimizing instant navigations (push case)

The instant shell is what the user sees the moment they click a link before
any dynamic data arrives. Same PPR shell concept as the HTML shell, but
delivered as an RSC payload during client navigation.

In dev there is no real prefetching. The lock cookie + `pushstate`
simulates the instant navigation by telling the dev server to respond as if
to a prefetch.

**Workflow:**

1. Launch with the React hook, navigate to a page that has a link to the
   target.
2. Set the lock cookie.
3. `agent-browser pushstate /target-route` - client-side navigation.
4. `agent-browser screenshot "Instant shell - pushed"`.
5. `agent-browser react suspense` - the instant shell's boundary analysis.
6. Clear the cookie.

Client-hook blockers (`usePathname`, `useSearchParams`, `useSelectedLayoutSegment`,
etc.) resolve instantly on the client under push but not under goto - they
only matter for direct loads. When the primary blocker is `client-hook`
and you're optimizing the push case specifically, don't spend effort moving
those hooks around; focus on `request-api` / `server-fetch` / `cache`
blockers instead.

## Runtime prefetching for cookie-dependent instant shells

When the instant shell is empty or shows only skeletons for routes that
depend on `cookies()` or other request-scoped data, the static prefetch
can't include that content - it runs without request context. Runtime
prefetching solves this: the server generates prefetch data using real
cookies, and the client caches it for instant navigations.

Three features compose to make this work:

<table>
<tr><th>Feature</th><th>Role</th></tr>
<tr>
<td><code>unstable_instant</code></td>
<td>Declares the route must support instant navigation; validates a static shell exists</td>
</tr>
<tr>
<td><code>unstable_prefetch = 'runtime'</code></td>
<td>Tells the server to produce a runtime prefetch stream with request context</td>
</tr>
<tr>
<td><code>"use cache: private"</code></td>
<td>Caches per-request data (cookies) in the request-scoped Resume Data Cache so the runtime prefetch rerender reuses it without re-fetching</td>
</tr>
</table>

Without `unstable_prefetch = 'runtime'`, the prefetch only includes the
static shell. Without `"use cache: private"`, the runtime prefetch
re-executes every data call. All three are needed for instant navigations
that show real personalized content.

Read `node_modules/next/dist/docs/` for the full technical breakdown
before starting - your training data may be outdated on these APIs.

**Diagnosis:**

1. Audit instant shells across the target routes: lock, `pushstate` each
   route, screenshot, `react suspense`. Identify routes with empty /
   skeleton shells.
2. For each empty route, temporarily export `unstable_instant = true` and
   navigate - `get_errors` will surface validation failures that name the
   blocking API (`cookies()`, `connection()`, etc.) and the component
   calling it. Diagnostic, not the fix.
3. Read the source of the blocking components. Pattern to find: a data
   function reads `cookies()` -> component becomes dynamic -> hole in the
   static shell -> instant shell has nothing to show there.

**Fix pattern (per route):**

1. In the page's route segment config, export both:
   ```ts
   export const unstable_instant = true
   export const unstable_prefetch = 'runtime'
   ```
2. In data-fetching functions that read `cookies()`, add
   `"use cache: private"` so the result is cached per-request and reused
   by the runtime prefetch rerender. If `"use cache: private"` can't be
   applied directly (e.g. file has `"use server"` directive), extract the
   function to a separate file.
3. If a shared layout or utility calls `connection()` to prevent sync I/O
   during prefetch, investigate whether it also blocks runtime prefetching.
   `connection()` opts into dynamic rendering, which prevents the runtime
   prefetch stream from being generated. A `setTimeout(resolve, 0)` macro
   task boundary provides the same sync I/O protection without blocking
   runtime prefetch - judgment call for the user.

**Verification:**

Runtime prefetch data is generated during the initial page load and
streamed alongside the page content. The client's segment cache fills
asynchronously, not instantly.

1. Navigate to the *origin* page (the page the user navigates *from*).
2. Wait 10-15 seconds for the runtime prefetch stream to complete.
3. Set the lock cookie.
4. `pushstate` to the target route - the instant shell should now show
   real content, not just skeletons.
5. Screenshot to confirm.
6. `react suspense` for the analysis.

## Debugging tips

- `get_errors` doesn't report while locked. If the shell looks wrong
  (empty, bailed to CSR), unlock, navigate normally, then run `get_errors`.
- When PPR bails out completely, `react suspense` returns no boundaries.
  In this case unlock, navigate, and use `get_errors` + `get_logs` to find
  the root cause.
- `network` interception can distinguish a real RSC stream from an HTML
  response: `agent-browser network requests --filter /_next/data` shows
  the RSC payload for each client-side navigation.

## Test your hypothesis before proposing a fix

If you suspect a component is the cause, find evidence - `get_errors`,
`react inspect <id>`, or compare a working route to a broken one. Don't
commit to a root cause or propose changes from a single observation.
