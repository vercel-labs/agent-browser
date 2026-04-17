# Debugging re-renders with `agent-browser react renders`

When the user reports "slow after load", "janky", "too many re-renders",
or "laggy interactions" - that's update-phase rendering, not initial load.
Use `react renders` to profile it. For initial load use
[hydration.md](hydration.md).

## Workflow

```bash
agent-browser open --enable react-devtools http://localhost:3000
agent-browser react renders start
# reproduce the slow interaction: click, fill, pushstate, or just wait if
# the issue is polling/timers
agent-browser react renders stop
```

The hook survives navigations (`navigate`/`reload`) and captures mount and
hydration renders, so no need to start before or after navigation.

## Reading the report

```
# Render Profile - 3.05s recording
# 426 renders (38 mounts + 388 re-renders) across 38 components
# FPS: avg 120, min 106, max 137, drops (<30fps): 0

## Components by total render time
| Component              | Insts | Mounts | Re-renders | Total    | Self     | DOM   | Top change reason          |
| ---------------------- | ----- | ------ | ---------- | -------- | -------- | ----- | -------------------------- |
| Parent                 |     1 |      1 |          9 |   5.8ms  |   3.4ms  | 10/10 | state (hook #0)            |
| MemoChild              |     3 |      3 |         27 |     2ms  |   1.9ms  | 30/30 | props.data                 |
| Router                 |     1 |      1 |          9 |   6.3ms  |       -  |  0/10 | parent (ErrorBoundaryHandler) |

## Change details (prev -> next)
  Parent
    state (hook #0): 0 -> 1
  MemoChild
    props.data: {value} -> {value}
```

**Columns:**

- `Insts` - unique component instances observed
- `Mounts` - first-render count for an instance
- `Re-renders` - update-phase renders (total minus mounts)
- `Total` - inclusive render time (component + children)
- `Self` - exclusive render time (component only)
- `DOM` - how many renders actually mutated the DOM vs total renders. A
  component with 100 renders and 0 DOM mutations is doing purely wasted work.
- `Top change reason` - most frequent trigger

**Change reasons:**

- `props.<key>` - a prop changed by reference, with prev -> next values
- `state (hook #N)` - a `useState` / `useReducer` hook changed
- `context (<name>)` - a specific context value changed
- `parent (<name>)` - parent re-rendered, names the parent
- `parent (<name> (mount))` - parent is also mounting (load-time cascading,
  not a leak)
- `mount` - first render

**Timing data** (`Total`, `Self`) requires a React profiling build (`next dev`).
In production builds these columns show `-` but render counts, DOM
mutations, and change reasons are still reported.

## Forming hypotheses

Use the raw columns to reason:

- `Mounts` vs `Re-renders` - is the component re-rendering after load, or
  is the count just from mount-time cascading?
- `Insts` - is a high render count from many instances, or one instance
  rendering excessively?
- `Self` - is this component expensive per render, or just called often?
- `DOM` - did renders actually produce visible changes? Wasted renders
  with 0 DOM mutations are a memo / equality issue.
- `Total` vs `Self` - cost in this component or its children?
- Change reasons - what's driving the re-renders? `parent (X (mount))`
  is load-time cascading, not a leak.
- FPS - are the re-renders causing user-visible jank?

Then:

```bash
agent-browser react tree              # find the component ID
agent-browser react inspect <id>      # source file, props, hooks
```

Read the source to understand **why** it re-renders.

## Verify the fix

After editing the code, HMR picks it up. Re-run `react renders start` /
`react renders stop` and compare the raw numbers. The same component
should show fewer renders, fewer DOM mutations, or lower total time.

## Test your hypothesis before proposing a fix

If you suspect a component is the root cause, find evidence: inspect it
with `react inspect`, read its source, check what's changing via the
change-reason column. Don't propose changes from a single observation.

## Limits

- Up to 200 components tracked per recording.
- Up to 50 change entries kept per component (for the details section).
- `--json` for raw structured output when you want programmatic access to
  the full change list.
