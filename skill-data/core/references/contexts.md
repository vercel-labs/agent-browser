# Browser Contexts

Isolated cookies, localStorage, and cache within a single Chrome process.

**Related**: [session-management.md](session-management.md) for multi-session workflows,
[SKILL.md](../SKILL.md) for the quick-start overview.

## Contents

- [What browser contexts are](#what-browser-contexts-are)
- [Contexts vs sessions vs multiple tabs](#contexts-vs-sessions-vs-multiple-tabs)
- [Commands](#commands)
- [Patterns](#patterns)
- [State persistence](#state-persistence)
- [Caveats](#caveats)

## What browser contexts are

A browser context is Chrome's native isolation primitive (CDP
`Target.createBrowserContext`). Every context has its own:

- Cookie jar
- localStorage and sessionStorage (per origin)
- Cache
- Service workers

Tabs opened inside a context share that context's storage but are isolated
from tabs in other contexts and from the default context. This mirrors the
`BrowserContext` concept in Playwright and Puppeteer.

## Contexts vs sessions vs multiple tabs

| Need | Recommended approach |
|------|---------------------|
| Fully separate Chrome processes | `--session <name>` (one daemon per session) |
| Isolated identity within one process | `context new` (one daemon, multiple contexts) |
| Separate views of the same login | Multiple tabs in the same context |

Use contexts when you need identity isolation but want to save memory: two
contexts share one Chrome process (~250 MB total) versus two separate sessions
(~200 MB each, ~400 MB total).

Use sessions (`--session`) when you need full process isolation or want to run
truly parallel workflows from different shell processes without coordination.

## Commands

### Create a context

```bash
agent-browser context new                  # Returns c1, c2, c3 ...
agent-browser context new --label staging  # Create with a memorable label
```

The response includes `contextId` (the stable ref, e.g. `c1`) and `label`
(if given). Refs are assigned sequentially and never reused within a session.

Labels must start with a letter and contain only letters, digits, `-`, and `_`.
Labels are unique per session — using a duplicate errors immediately.

### List contexts

```bash
agent-browser context list
```

Returns all active contexts with their `contextId`, `label`, and tab count.

### Open a tab inside a context

```bash
agent-browser tab new --context staging https://app.example.com
agent-browser tab new --context c1 https://app.example.com
```

The `--context` flag accepts either the stable ref (`c1`) or the label
(`staging`). Without `--context`, the tab opens in the default context.

### Close a context

```bash
agent-browser context close staging   # by label
agent-browser context close c1        # by ref
```

Closing a context:

1. Closes all tabs belonging to it (best-effort — already-closed tabs are
   silently skipped).
2. Disposes the CDP BrowserContext.
3. Removes the context from the registry.

Closing a context with an unknown ref or label returns a clear error rather
than propagating a CDP exception.

## Patterns

### Multi-identity workflow

Run two accounts in parallel without two daemons:

```bash
agent-browser context new --label alice
agent-browser context new --label bob

agent-browser tab new --context alice https://app.example.com/login
agent-browser tab new --context bob  https://app.example.com/login

# Log into alice
agent-browser tab alice
agent-browser snapshot -i
agent-browser fill @e3 "alice@example.com"
agent-browser fill @e4 "hunter2"
agent-browser click @e5
agent-browser wait --url "**/dashboard"

# Log into bob (separate cookie jar — alice's session is not visible)
agent-browser tab bob
agent-browser snapshot -i
agent-browser fill @e3 "bob@example.com"
agent-browser fill @e4 "s3cr3t"
agent-browser click @e5
agent-browser wait --url "**/dashboard"
```

### A/B comparison (logged-in vs anonymous)

```bash
agent-browser context new --label auth
agent-browser tab new --context auth https://app.example.com/login
# ... log in inside auth context ...

# Default context is always anonymous
agent-browser tab new https://app.example.com

# Compare snapshots
agent-browser tab auth
agent-browser snapshot -i --json > auth-view.json

agent-browser tab t1   # the anonymous tab
agent-browser snapshot -i --json > anon-view.json
```

### Throwaway sandbox

```bash
agent-browser context new --label sandbox
agent-browser tab new --context sandbox https://untrusted.example.com
# ... explore freely ...
agent-browser context close sandbox  # wipes all cookies and storage for that context
```

### Parallel sub-agents

```bash
# Parent creates contexts and passes refs to sub-agents via environment variable
agent-browser context new --label worker1
agent-browser context new --label worker2

# Sub-agent 1 (separate shell, same session)
AGENT_BROWSER_SESSION=default agent-browser tab new --context worker1 https://task-a.example.com

# Sub-agent 2
AGENT_BROWSER_SESSION=default agent-browser tab new --context worker2 https://task-b.example.com
```

## State persistence

`agent-browser state save <path>` captures per-context cookies and localStorage
alongside the default context state. On `state load`, each saved context is
re-created with its original label, and its cookies and storage are restored.

Old state files (saved before context support was added) load without error —
the `contexts` field defaults to an empty list.

Note: the `ref_id` field in a saved context snapshot (`c1`, `c2`, ...) is
informational only. After loading, contexts are re-created in order and
assigned new sequential refs starting from `c1`. The label is the stable
identifier to use across save/load cycles.

## Caveats

**Re-snapshot after switching context tabs.** Element refs (`@e1`, `@e2`, ...)
are scoped to the tab that was active when the snapshot ran. Switching to a tab
in a different context does not invalidate the ref cache automatically, but
using a stale ref against the new tab will produce a "ref not found" error.
Always re-snapshot after switching to a tab from a different context.

**localStorage is per-origin per-context by design.** CDP does not share
localStorage between tabs in the same context when the tabs navigate to the
same origin — they share the same storage namespace (standard browser
behaviour). Cross-tab storage sharing within a context is the same as in a
normal Chrome window.

**Default context is always present.** Tabs opened without `--context` belong
to the default context. The default context cannot be closed.

**Labels are session-scoped, not process-scoped.** If you open a new CLI
session (new daemon), the context registry starts empty and label collisions
reset. Labels only need to be unique within the same running session.

**CDP id vs stable ref.** The JSON response from `context new` includes both
`contextId` (the stable `c<N>` ref) and `browserContextId` (the raw CDP hex
string). Always use `contextId` or `label` for subsequent commands — the raw
CDP id is internal and may change across load cycles.
