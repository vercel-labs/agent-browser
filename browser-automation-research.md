# Browser Automation for AI Agents: Technical Reference

> Consolidated research on **Playwright MCP** and **agent-browser** — two approaches to giving AI agents browser control.

---

## Part 1: Playwright MCP

**Repo:** [microsoft/playwright-mcp](https://github.com/microsoft/playwright-mcp) (thin wrapper)
**Actual source:** [playwright monorepo](https://github.com/microsoft/playwright/tree/main/packages/playwright/src/mcp)
**npm:** `@playwright/mcp`

### What It Is

An MCP server that exposes Playwright's browser automation as structured tool calls. Instead of screenshot-based interaction (which requires a vision model), it gives the LLM an **accessibility tree snapshot** — the same semantic representation screen readers use.

### Architecture

```
LLM Client (Claude, Copilot, Cursor)
    │  MCP Protocol (STDIO, HTTP/SSE, or WebSocket)
    v
MCP Server Layer ─── registers tools, manages heartbeat
    │
    v
BrowserServerBackend ─── routes tool calls
    │
    v
Context ─── manages BrowserContext lifecycle, tabs, network filtering
    │
    v
Tab ─── wraps Playwright Page, captures snapshots, resolves refs
    │
    v
Playwright Browser Engine (Chromium, Firefox, WebKit)
```

### Transport Options

- **STDIO** (default): Standard for IDE integrations. Client spawns the process.
- **HTTP/SSE** (`--port 3000`): For remote deployments. Exposes `/mcp` (streamable) and `/sse` (legacy).
- **WebSocket**: Used internally by extension bridge mode.

### Browser Session Management

**Lazy launch**: Browser starts on first tool call, not on server start.

**Three profile modes:**

| Mode | Description | Use Case |
|------|-------------|----------|
| **Persistent** (default) | Profile at `~/.cache/ms-playwright/mcp-{channel}-profile`. Cookies/localStorage survive between sessions. | Repeated tasks against same sites |
| **Isolated** (`--isolated`) | In-memory only. Can seed with `--storage-state`. | Clean, reproducible sessions |
| **Extension bridge** (`--extension`) | Connects to already-running Chrome via browser extension + CDP. | Automate pages where user is already logged in |

**Security:**
- `--allowed-origins` / `--blocked-origins` for network filtering
- `file://` blocked by default
- `--secrets` flag redacts sensitive values from all responses

---

### Extension Bridge Mode (Deep Dive)

Extension mode (`--extension`) is architecturally distinct from server mode. Instead of launching a fresh browser, it connects to an **existing Chrome session** via a browser extension and the Chrome DevTools Protocol (CDP).

#### Layer Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     AI Agent (MCP Client)                   │
│              listTools() / callTool(name, args)             │
└─────────────────────┬───────────────────────────────────────┘
                      │ MCP Protocol
┌─────────────────────▼───────────────────────────────────────┐
│                      MCP Server                             │
│         (--extension mode)                                  │
│         Translates MCP tool calls → CDP commands            │
└─────────────────────┬───────────────────────────────────────┘
                      │ WebSocket (JSON protocol)
┌─────────────────────▼───────────────────────────────────────┐
│                      Relay Layer                            │
│         RelayConnection class                               │
│         Bridges WebSocket ↔ Chrome Debugger API             │
└─────────────────────┬───────────────────────────────────────┘
                      │ chrome.debugger API (CDP)
┌─────────────────────▼───────────────────────────────────────┐
│                    Extension Layer                          │
│         TabShareExtension (Chrome Service Worker)           │
│         Manages tab lifecycle, UI feedback                  │
└─────────────────────┬───────────────────────────────────────┘
                      │
              [ Browser Tab ]
```

#### The Browser Extension

The extension (`@playwright/mcp-extension`) is a **Chrome Manifest V3 service worker** with these permissions:
- `debugger` — attach to tabs via CDP
- `activeTab`, `tabs` — tab management
- `storage` — token persistence
- `<all_urls>` — broad page access

#### Connection Flow

1. **Initiation**: Extension UI parses URL params (`mcpRelayUrl`, `protocolVersion`, `token`)
2. **Validation**: Ensures `mcpRelayUrl` is a loopback address; checks protocol compatibility
3. **Auto-connect**: If `PLAYWRIGHT_MCP_EXTENSION_TOKEN` is set, bypasses the approval dialog
4. **Tab selection**: User picks target tab (or auto-connects with token)
5. **Debugger attachment**: `chrome.debugger.attach()` connects to the selected tab

#### CDP Relay Protocol

The relay uses a custom JSON protocol over WebSocket:

| Direction | Message Type | Purpose |
|-----------|--------------|---------|
| Client → Extension | `attachToTab` | Attach debugger to a tab |
| Client → Extension | `forwardCDPCommand` | Execute a CDP command on the tab |
| Extension → Client | `forwardCDPEvent` | Forward CDP events back to the MCP server |

The `RelayConnection` class:
- Parses incoming `ProtocolCommand` objects
- Calls `chrome.debugger.sendCommand()` for CDP execution
- Listens to `chrome.debugger.onEvent` and forwards events to the MCP server

This is the critical layer: the MCP server doesn't talk to the browser directly. It sends structured CDP commands over a WebSocket to the extension, which proxies them through Chrome's `chrome.debugger` API.

---

### The Snapshot System

**Why snapshots instead of screenshots:**
1. No vision model required — works with text-only LLMs
2. Deterministic — no ambiguity ("the blue button" vs exact ref)
3. Token efficient — YAML tree << base64 image
4. Supports incremental diffs between actions

**How it works:**

The internal API `page._snapshotForAI()` produces a YAML-like accessibility tree where each interactive element gets a **ref**:

```yaml
- navigation "Main":
  - link "Home" [ref=e1]
  - link "Products" [ref=e2]
- main:
  - heading "Welcome" [ref=e3]
  - textbox "Search" [ref=e4]
  - button "Search" [ref=e5]
```

The LLM calls:
```json
{ "tool": "browser_click", "arguments": { "ref": "e5", "element": "Search button" } }
```

Refs resolve via `aria-ref=` pseudo-selector (Playwright internal), mapping back to DOM elements through ARIA role + accessible name.

**Three snapshot modes** (`--snapshot-mode`):
- **`incremental`** (default): First response = full tree, subsequent = diff only. Massive token savings.
- **`full`**: Always returns complete tree.
- **`none`**: No snapshot in responses.

### Tool System

~70 tools across 26 source files, organized by **capability gates**:

| Capability | Always On? | Example Tools |
|---|---|---|
| `core` | Yes | `browser_snapshot`, `browser_click`, `browser_evaluate`, `browser_close` |
| `core-navigation` | Yes | `browser_navigate`, `browser_navigate_back` |
| `core-tabs` | Yes | `browser_tab_list`, `browser_tab_new`, `browser_tab_select` |
| `core-input` | Yes | `browser_type`, `browser_fill`, `browser_press_key` |
| `vision` | Opt-in | `browser_screenshot`, coordinate-based mouse tools |
| `pdf` | Opt-in | `browser_pdf_save` |
| `testing` | Opt-in | Playwright assertion tools |
| `devtools` | Opt-in | Trace recording |
| `storage` | Opt-in | Cookie management |
| `network` | Opt-in | Request listing, route interception |

Enable opt-in capabilities: `--caps vision,pdf,network`

Each tool handler also emits equivalent Playwright code (`response.addCode()`), so every action produces a runnable test script line.

### Response Format

Every tool response is structured markdown:

```markdown
### Result
(tool output)

### Ran Playwright code
```js
await page.goto('https://example.com');
```

### Open tabs
- 0: (current) [Example](https://example.com)

### Page
- Page URL: https://example.com
- Page Title: Example Domain

### Snapshot
```yaml
- heading "Example Domain" [ref=e1]
- link "More information..." [ref=e2]
```
```

### Setup

```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": [
        "@playwright/mcp@latest",
        "--browser", "chrome",
        "--headless",
        "--caps", "vision,pdf"
      ]
    }
  }
}
```

---

## Part 2: agent-browser (Vercel Labs)

**Repo:** [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)
**Language:** TypeScript + Rust CLI

### What It Is

A headless browser automation CLI purpose-built for AI agents. Unlike Playwright MCP (which is an MCP server), agent-browser is a **command-line tool** that agents invoke via shell commands.

### Architecture: Three-Tier Client-Daemon-Browser

```
Rust CLI (fast native binary, sub-ms parsing)
    │  IPC (Unix socket on Linux/macOS, TCP on Windows)
    v
Node.js Daemon (persistent background process)
    │  Playwright / Appium
    v
Browser (Chromium, Firefox, WebKit, iOS Safari)
```

**Why this architecture:**
- **Rust CLI**: Sub-millisecond parsing overhead. Matters when agents issue dozens of commands.
- **Persistent daemon**: Eliminates 2-5s browser cold start on every command. First command boots daemon; subsequent reuse it.
- **Session isolation**: Each `--session <name>` gets its own socket + browser. Multiple agents run in parallel.

### The Provider System

A **provider** determines where the browser runs. All commands work identically regardless of provider — the abstraction is transparent.

**Provider selection** happens in `BrowserManager.launch()` via a priority chain:

```
1. CDP connection (explicit --cdp <port|url>)
2. Auto-connect (--auto-connect, discovers running Chrome)
3. Named cloud provider (-p <name> or AGENT_BROWSER_PROVIDER env var)
4. iOS provider (-p ios, uses Appium instead of Playwright)
5. Default: local Playwright launch
```

#### All Providers

| Provider | Activation | Env Vars | Backend | Use Case |
|---|---|---|---|---|
| **Local** | Default | None | Playwright local | Dev, testing |
| **CDP** | `--cdp <port\|url>` | None | Chrome DevTools Protocol | Existing Chrome/Electron |
| **Auto-connect** | `--auto-connect` | None | Auto-discovered Chrome | Zero-config local |
| **Browserbase** | `-p browserbase` | `BROWSERBASE_API_KEY`, `BROWSERBASE_PROJECT_ID` | Cloud API → CDP | Serverless, cloud |
| **Browser Use** | `-p browseruse` | `BROWSER_USE_API_KEY` | Cloud API → CDP | Cloud automation |
| **Kernel** | `-p kernel` | `KERNEL_API_KEY` | Cloud API → CDP (stealth) | Anti-bot bypass |
| **iOS** | `-p ios` | Optional device/UDID vars | Appium + WebDriverIO | Mobile Safari |

#### How Cloud Providers Work Internally

All three cloud providers (Browserbase, Browser Use, Kernel) follow the same pattern:

1. Create a remote session via the provider's REST API
2. Get a CDP WebSocket URL from the response
3. Connect Playwright via `chromium.connectOverCDP(wsUrl)`
4. Grab existing context and page
5. Store session ID for cleanup on `close()`

Example (Browserbase):
```typescript
// 1. Create session
const response = await fetch('https://api.browserbase.com/v1/sessions', {
  method: 'POST',
  headers: { 'X-BB-API-Key': apiKey },
  body: JSON.stringify({ projectId }),
});
const session = await response.json();

// 2. Connect Playwright
const browser = await chromium.connectOverCDP(session.connectUrl);

// 3. Reuse existing page
const context = browser.contexts()[0];
const page = context.pages()[0] ?? await context.newPage();
```

#### Kernel-Specific Features

Optional env vars for Kernel provider:
- `KERNEL_PROFILE_NAME` — persistent profile
- `KERNEL_HEADLESS` — headless mode
- `KERNEL_STEALTH` — stealth/anti-detection
- `KERNEL_TIMEOUT_SECONDS` — session timeout

#### iOS: A Separate Architecture

iOS uses `IOSManager` instead of `BrowserManager`:
- **Appium + WebDriverIO** instead of Playwright
- Controls real Mobile Safari in Simulator or on USB device
- Own ref system (`IOSRefMap`) mirrors desktop one
- Mobile-specific commands: `swipe`, `tap`, `device list`
- Daemon routes iOS commands to `executeIOSCommand()` instead of `executeCommand()`

#### Adding a Custom Provider

**There is no formal plugin interface.** Providers are hard-coded in `BrowserManager.launch()`. To add one:

1. Add a `connectToMyProvider()` private method in `/src/browser.ts`
2. Add a dispatch case in `launch()`
3. Add cleanup in `close()`
4. Update CLI to accept new provider name

**Practical shortcut**: If a service exposes a CDP endpoint, you can use `--cdp <your-ws-url>` directly — no custom provider needed.

### The Snapshot-Ref System

Same core concept as Playwright MCP — accessibility tree with refs:

```
- heading "Example Domain" [ref=e1] [level=1]
- button "Submit" [ref=e2]
- textbox "Email" [ref=e3]
```

Interaction via refs:
```bash
agent-browser click @e2        # Click "Submit"
agent-browser fill @e3 "test@example.com"
```

**Ref resolution** uses ARIA role + accessible name (more stable than CSS selectors):
```typescript
let locator = page.getByRole(refData.role, { name: refData.name, exact: true });
if (refData.nth !== undefined) locator = locator.nth(refData.nth);
```

The `-C` flag adds detection of elements with `cursor: pointer` / `onclick` that lack proper ARIA roles (common in SPAs).

**Ref lifecycle**: Refs are invalidated when the DOM changes. Always re-snapshot after navigation or dynamic content updates.

**Diff support**: `diff snapshot` / `diff screenshot` detect changes between actions. This is an explicit command, unlike Playwright MCP's built-in incremental snapshot mode.

### AI-Friendly Design

- **`--json` flag**: Machine-readable output with explicit `success`/`error` fields
- **AI-friendly errors**: Rewrites cryptic Playwright errors into actionable guidance
- **Snapshot stats**: Token count estimates so agents can choose compact vs full snapshot
- **Annotated screenshots** (`--annotate`): Numbered labels on interactive elements for multimodal models

### Session Persistence

Three levels:
1. **Ephemeral** (default): Everything lost on close
2. **Session persistence** (`--session-name`): Auto-saves/restores cookies + localStorage to JSON (optional AES-256-GCM encryption)
3. **Persistent profiles** (`--profile`): Full Chromium user data directory

### Command Structure

```bash
agent-browser <command> [args] [options]
```

| Category | Example Commands |
|----------|------------------|
| Navigation | `open <url>`, `back`, `forward`, `reload` |
| Interaction | `click <sel>`, `fill <sel> <text>`, `hover <sel>` |
| Data | `get text <sel>`, `screenshot`, `snapshot` |
| State | `console`, `network`, `cookies` |

Selectors supported: refs (`@e1`), CSS, XPath, text-based, semantic locators.

### The Agent Interaction Loop

```
AI Agent
  │ "agent-browser snapshot -i --json"
  v
agent-browser CLI (Rust) → daemon (Node.js) → browser
  │ Returns: JSON accessibility tree + refs
  v
AI Agent (parses, decides)
  │ "agent-browser click @e3"
  v
agent-browser CLI (Rust) → daemon (Node.js) → browser
  │ Returns: { success: true }
  v
AI Agent (re-snapshots to verify)
```

---

## Part 3: Structural Comparison

### Interface Philosophy

| | Playwright MCP | agent-browser |
|---|---|---|
| **Protocol** | MCP (standardized JSON-RPC) | CLI (shell exec) |
| **Integration** | Native in MCP-aware clients | Any system that runs shell commands |
| **Tool discovery** | Client auto-discovers tools via MCP | Agent must know commands upfront |
| **Statefulness** | MCP server lifetime = session | Persistent daemon across commands |

### Snapshot Approach

Both use accessibility tree snapshots with refs. The core idea is identical. Differences:

| | Playwright MCP | agent-browser |
|---|---|---|
| **Incremental updates** | Built into protocol (`--snapshot-mode incremental`) | Explicit `diff snapshot` command |
| **Snapshot API** | Internal `_snapshotForAI()` (tightly coupled) | Public Playwright ARIA snapshot API |
| **Ref format** | `ref=e1` | `@e1` |
| **Clickable detection** | Standard ARIA roles only | `-C` flag adds `cursor:pointer` / `onclick` detection |

### Provider / Session Models

| | Playwright MCP | agent-browser |
|---|---|---|
| **Cloud providers** | None built-in | Browserbase, Browser Use, Kernel |
| **Mobile** | No | iOS via Appium |
| **Existing browser** | Extension bridge (CDP via Chrome extension) | `--cdp` flag or `--auto-connect` |
| **Session isolation** | One session per MCP server | Named sessions via `--session` |
| **Profile persistence** | `~/.cache/ms-playwright/mcp-{channel}-profile` | `--profile` (Chromium user data dir) |

### The CDP Connection Point

Both tools ultimately interact with browsers via CDP (Chrome DevTools Protocol), but through different paths:

- **Playwright MCP extension mode**: MCP Server → WebSocket → RelayConnection → `chrome.debugger` API → tab
- **agent-browser CDP provider**: CLI → daemon → `chromium.connectOverCDP(wsUrl)` → remote browser
- **agent-browser cloud providers**: CLI → daemon → REST API → get CDP URL → `chromium.connectOverCDP(wsUrl)`

---

## References

- [playwright-mcp on DeepWiki](https://deepwiki.com/microsoft/playwright-mcp)
- [agent-browser on DeepWiki](https://deepwiki.com/vercel-labs/agent-browser)
- [GitHub: microsoft/playwright-mcp](https://github.com/microsoft/playwright-mcp)
- [GitHub: vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)
