# Profiling

Capture Chrome DevTools performance profiles during browser automation for performance analysis.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Basic Profiling](#basic-profiling)
- [Profiler Commands](#profiler-commands)
- [Categories](#categories)
- [Use Cases](#use-cases)
- [Output Format](#output-format)
- [Viewing Profiles](#viewing-profiles)
- [Limitations](#limitations)

## Basic Profiling

```bash
# Start profiling
agent-browser profiler start

# Perform actions
agent-browser navigate https://example.com
agent-browser click "#button"
agent-browser wait 1000

# Stop and save
agent-browser profiler stop ./trace.json
```

## Profiler Commands

```bash
# Start profiling with default categories
agent-browser profiler start

# Start with custom trace categories
agent-browser profiler start --categories "devtools.timeline,v8.execute,blink.user_timing"

# Stop profiling and save to file
agent-browser profiler stop ./trace.json
```

## Categories

The `--categories` flag accepts a comma-separated list of Chrome trace categories. Default categories include:

- `devtools.timeline` -- standard DevTools performance traces
- `v8.execute` -- time spent running JavaScript
- `blink` -- renderer events
- `blink.user_timing` -- `performance.mark()` / `performance.measure()` calls
- `latencyInfo` -- input-to-latency tracking
- `renderer.scheduler` -- task scheduling and execution
- `toplevel` -- broad-spectrum basic events

Several `disabled-by-default-*` categories are also included for detailed timeline, call stack, and V8 CPU profiling data.

## Use Cases

### Diagnosing Slow Page Loads

```bash
agent-browser profiler start
agent-browser navigate https://app.example.com
agent-browser wait --load networkidle
agent-browser profiler stop ./page-load-profile.json
```

### Profiling User Interactions

```bash
agent-browser navigate https://app.example.com
agent-browser profiler start
agent-browser click "#submit"
agent-browser wait 2000
agent-browser profiler stop ./interaction-profile.json
```

### CI Performance Regression Checks

```bash
#!/bin/bash
agent-browser profiler start
agent-browser navigate https://app.example.com
agent-browser wait --load networkidle
agent-browser profiler stop "./profiles/build-${BUILD_ID}.json"
```

## Output Format

The output is a JSON file in Chrome Trace Event format:

```json
{
  "traceEvents": [
    { "cat": "devtools.timeline", "name": "RunTask", "ph": "X", "ts": 12345, "dur": 100, ... },
    ...
  ],
  "metadata": {
    "clock-domain": "LINUX_CLOCK_MONOTONIC"
  }
}
```

The `metadata.clock-domain` field is set based on the host platform (Linux or macOS). On Windows it is omitted.

## Viewing Profiles

Load the output JSON file in any of these tools:

- **Chrome DevTools**: Performance panel > Load profile (Ctrl+Shift+I > Performance)
- **Perfetto UI**: https://ui.perfetto.dev/ -- drag and drop the JSON file
- **Trace Viewer**: `chrome://tracing` in any Chromium browser

## React Profiling

Capture React-specific component render data without the React DevTools extension. Works fully headless by injecting a lightweight hook into `__REACT_DEVTOOLS_GLOBAL_HOOK__`.

**Requirement**: The target React app must use a development build or a profiling build (`react-dom/profiling`). Standard production builds strip the fiber timing data that makes this work.

### React Profiler Commands

```bash
# Start React profiling (injects hook into page)
agent-browser react_profile start

# Perform interactions that trigger React renders
agent-browser click "#add-item"
agent-browser fill "#search" "query"
agent-browser wait 2000

# Stop and get results inline
agent-browser react_profile stop

# Stop and save to file
agent-browser react_profile stop ./react-profile.json
```

### Combining with CDP Profiling

Both profilers can run simultaneously. The CDP profiler captures browser-level trace events (JS execution, layout, paint), while the React profiler captures component-level data (render durations, mount/update phases).

```bash
agent-browser profiler start
agent-browser react_profile start
agent-browser click "#heavy-component"
agent-browser wait 2000
agent-browser react_profile stop ./react-profile.json
agent-browser profiler stop ./chrome-trace.json
```

### React Profile Output Format

```json
{
  "reactDetected": true,
  "reactVersion": "18.2.0",
  "renders": [
    {
      "id": "1",
      "phase": "update",
      "componentName": "TodoList",
      "actualDuration": 12.5,
      "baseDuration": 8.3,
      "startTime": 1500.2,
      "commitTime": 1512.7
    }
  ],
  "components": [
    {
      "name": "TodoList",
      "renderCount": 5,
      "totalActualDuration": 62.5,
      "averageActualDuration": 12.5,
      "reasons": ["mount", "update"]
    }
  ],
  "summary": {
    "totalRenders": 42,
    "totalComponents": 8,
    "slowestComponents": [
      { "name": "TodoList", "avgDuration": 12.5 },
      { "name": "SearchResults", "avgDuration": 8.1 }
    ],
    "totalDuration": 156.3
  }
}
```

### Key Fields

- **`actualDuration`** -- time spent rendering the component in this commit (ms)
- **`baseDuration`** -- estimated time for a full re-render of the subtree (ms)
- **`phase`** -- `mount` (first render) or `update` (re-render)
- **`slowestComponents`** -- top 10 components by average render duration
- **`reactDetected`** -- `false` if no React app was found on the page (graceful degradation, no error)

## Limitations

- Only works with Chromium-based browsers (Chrome, Edge). Not supported on Firefox or WebKit.
- Trace data accumulates in memory while profiling is active (capped at 5 million events). Stop profiling promptly after the area of interest.
- Data collection on stop has a 30-second timeout. If the browser is unresponsive, the stop command may fail.
- React profiling requires a React development or profiling build. Production builds zero out `actualDuration` and related fields.
- React render data is capped at 50,000 entries per session to prevent unbounded memory growth.
