# Initial load performance with `agent-browser vitals`

Profile a Next.js page load end-to-end: Core Web Vitals (LCP, CLS, TTFB,
FCP, INP) from standard browser APIs plus React hydration timing from the
profiling build that `next dev` ships.

For update-phase performance (slow after load, jank) use
[renders.md](renders.md) instead.

## Quick start

```bash
agent-browser open --enable react-devtools http://localhost:3000
agent-browser vitals http://localhost:3000/dashboard
```

Without a URL, `vitals` reloads the current page. With a URL, it navigates
there first. Observers are installed via `addInitScript` so they capture
the very first paint.

## Reading the report

```
# Page Load Profile - http://localhost:3000/dashboard

## Core Web Vitals
  TTFB                   42ms
  LCP               1205.3ms (img: /_next/image?url=...)
  CLS                    0.03
  FCP                  890ms
  INP                   58ms

## React Hydration - 65.5ms (466.2ms -> 531.7ms)
  Hydrated                         65.5ms  (466.2 -> 531.7)
  Commit                            2.0ms  (531.7 -> 533.7)
  Waiting for Paint                 3.0ms  (533.7 -> 536.7)
  Remaining Effects                 4.1ms  (536.7 -> 540.8)

## Hydrated components (42 total, sorted by duration)
  DeploymentsProvider                       8.3ms
  NavigationProvider                        5.1ms
  ...
```

**TTFB** - server response time (Navigation Timing API).
**LCP** - when the largest visible element painted, plus what it was.
**CLS** - cumulative layout shift score (lower is better).
**FCP** - first contentful paint.
**INP** - largest interaction latency observed (requires user interaction
during the recording window).

**React Hydration** - React reconciler phases and per-component hydration
cost. Requires React's profiling build:

- `next dev` emits the profiling build by default.
- Production builds strip `console.timeStamp`, so the hydration section
  is empty. If "no data (requires profiling build)" appears, you are
  probably running against a prod build.

## Common signals

- **Slow LCP, blank viewport until late**: large JS bundle or render-blocking
  resources. Inspect with `agent-browser network requests --type script`
  and look for giant bundles.
- **High CLS**: elements repositioning after initial paint. Inspect
  `clsEntries` in the JSON output (`vitals --json`) to see the timestamps
  of the worst shifts, then screenshot just before/after.
- **Slow hydration with one huge component**: the top entry in "Hydrated
  components" is the bottleneck. `agent-browser react tree` and
  `react inspect` it to understand what's happening.
- **Hydration mismatch**: browser console shows React's hydration error.
  Use `agent-browser console` to read it, and
  `bash templates/next-mcp.sh <origin> get_errors` for the build errors.

## When the hydration section is empty

- Running a production build (prod strips `console.timeStamp`). Run
  `next dev` instead to profile locally.
- Page didn't actually hydrate (e.g. PPR lock was still on, or the page
  bailed to SSR-only). Check `agent-browser console` and `get_errors`.

## `--json` output

```bash
agent-browser vitals http://localhost:3000/dashboard --json
```

Emits the raw structured data with full CLS entry list and component
timings. Useful for CI regression checks or piping into a summarizer.
