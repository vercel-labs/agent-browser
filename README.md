# agent-browser

Browser automation CLI for AI agents.

[![npm](https://img.shields.io/npm/v/agent-browser)](https://www.npmjs.com/package/agent-browser)
[![npm downloads](https://img.shields.io/npm/dm/agent-browser)](https://www.npmjs.com/package/agent-browser)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

CSS selectors break when the DOM changes. Full DOM dumps cost thousands of tokens. `agent-browser` gives AI agents a compact accessibility tree with stable refs instead.

```bash
agent-browser snapshot -i
# → heading "Dashboard"   [ref=e1]
# → button "New Project"  [ref=e2]   ← click by ref, not CSS selector
# → textbox "Search"      [ref=e3]   ← refs survive DOM changes

agent-browser click @e2
agent-browser fill @e3 "agent-browser"
```

| | |
|---|---|
| **Ref-based** | Stable `@e1`, `@e2` refs survive DOM changes — no CSS selectors, no XPath |
| **Token-efficient** | ~200–400 tokens per page vs 3,000–5,000 for full DOM |
| **Native Rust** | Sub-millisecond CLI overhead — no Node.js, no Playwright at runtime |
| **50+ commands** | Navigation, forms, tabs, cookies, storage, screenshots, diff |
| **Cloud-ready** | Browserless, Browserbase, Browser Use, Kernel via `-p` |
| **Mobile** | iOS Safari via Appium, same ref-based workflow |

## Install

```bash
# Recommended — native Rust binary via npm
npm install -g agent-browser
agent-browser install   # download Chrome (first time only)

# macOS
brew install agent-browser && agent-browser install

# Rust
cargo install agent-browser && agent-browser install

# Linux (with system dependencies)
agent-browser install --with-deps
```

[Full install guide →](https://agent-browser.dev/installation)

## Quick Start

```bash
agent-browser open https://example.com
agent-browser snapshot -i

# - heading "Example Domain" [ref=e1]
# - link "More information..." [ref=e2]
# - textbox "Search" [ref=e3]

agent-browser click @e2
agent-browser fill @e3 "query"
agent-browser screenshot page.png
agent-browser close
```

[Full guide →](https://agent-browser.dev/quick-start)

## Documentation

| | |
|---|---|
| **Getting started** | [Installation](https://agent-browser.dev/installation) · [Quick Start](https://agent-browser.dev/quick-start) · [Skills](https://agent-browser.dev/skills) |
| **Reference** | [Commands](https://agent-browser.dev/commands) · [Configuration](https://agent-browser.dev/configuration) · [Selectors](https://agent-browser.dev/selectors) · [Snapshots](https://agent-browser.dev/snapshots) |
| **Sessions & Auth** | [Sessions](https://agent-browser.dev/sessions) · [Security](https://agent-browser.dev/security) |
| **Features** | [CDP Mode](https://agent-browser.dev/cdp-mode) · [Streaming](https://agent-browser.dev/streaming) · [Diffing](https://agent-browser.dev/diffing) · [Profiler](https://agent-browser.dev/profiler) |
| **Engines** | [Chrome](https://agent-browser.dev/engines/chrome) · [Lightpanda](https://agent-browser.dev/engines/lightpanda) |
| **Providers** | [Browser Use](https://agent-browser.dev/providers/browser-use) · [Browserbase](https://agent-browser.dev/providers/browserbase) · [Browserless](https://agent-browser.dev/providers/browserless) · [Kernel](https://agent-browser.dev/providers/kernel) |
| **Platform** | [iOS Simulator](https://agent-browser.dev/ios) · [Native Mode](https://agent-browser.dev/native-mode) · [Next.js + Vercel](https://agent-browser.dev/next) |

## Options

| Option | Description |
|--------|-------------|
| `--session <name>` | Use isolated session (or `AGENT_BROWSER_SESSION` env) |
| `--session-name <name>` | Auto-save/restore session state (or `AGENT_BROWSER_SESSION_NAME` env) |
| `--profile <path>` | Persistent browser profile directory (or `AGENT_BROWSER_PROFILE` env) |
| `--state <path>` | Load storage state from JSON file (or `AGENT_BROWSER_STATE` env) |
| `--headers <json>` | Set HTTP headers scoped to the URL's origin |
| `--executable-path <path>` | Custom browser executable (or `AGENT_BROWSER_EXECUTABLE_PATH` env) |
| `--extension <path>` | Load browser extension (repeatable; or `AGENT_BROWSER_EXTENSIONS` env) |
| `--args <args>` | Browser launch args, comma or newline separated (or `AGENT_BROWSER_ARGS` env) |
| `--user-agent <ua>` | Custom User-Agent string (or `AGENT_BROWSER_USER_AGENT` env) |
| `--proxy <url>` | Proxy server URL with optional auth (or `AGENT_BROWSER_PROXY` env) |
| `--proxy-bypass <hosts>` | Hosts to bypass proxy (or `AGENT_BROWSER_PROXY_BYPASS` env) |
| `--ignore-https-errors` | Ignore HTTPS certificate errors (useful for self-signed certs) |
| `--allow-file-access` | Allow file:// URLs to access local files (Chromium only) |
| `-p, --provider <name>` | Cloud browser provider (or `AGENT_BROWSER_PROVIDER` env) |
| `--device <name>` | iOS device name, e.g. "iPhone 15 Pro" (or `AGENT_BROWSER_IOS_DEVICE` env) |
| `--json` | JSON output (for agents) |
| `--full, -f` | Full page screenshot |
| `--annotate` | Annotated screenshot with numbered element labels (or `AGENT_BROWSER_ANNOTATE` env) |
| `--screenshot-dir <path>` | Default screenshot output directory (or `AGENT_BROWSER_SCREENSHOT_DIR` env) |
| `--screenshot-quality <n>` | JPEG quality 0-100 (or `AGENT_BROWSER_SCREENSHOT_QUALITY` env) |
| `--screenshot-format <fmt>` | Screenshot format: `png`, `jpeg` (or `AGENT_BROWSER_SCREENSHOT_FORMAT` env) |
| `--headed` | Show browser window (not headless) (or `AGENT_BROWSER_HEADED` env) |
| `--cdp <port\|url>` | Connect via Chrome DevTools Protocol (port or WebSocket URL) |
| `--auto-connect` | Auto-discover and connect to running Chrome (or `AGENT_BROWSER_AUTO_CONNECT` env) |
| `--color-scheme <scheme>` | Color scheme: `dark`, `light`, `no-preference` (or `AGENT_BROWSER_COLOR_SCHEME` env) |
| `--download-path <path>` | Default download directory (or `AGENT_BROWSER_DOWNLOAD_PATH` env) |
| `--content-boundaries` | Wrap page output in boundary markers for LLM safety (or `AGENT_BROWSER_CONTENT_BOUNDARIES` env) |
| `--max-output <chars>` | Truncate page output to N characters (or `AGENT_BROWSER_MAX_OUTPUT` env) |
| `--allowed-domains <list>` | Comma-separated allowed domain patterns (or `AGENT_BROWSER_ALLOWED_DOMAINS` env) |
| `--action-policy <path>` | Path to action policy JSON file (or `AGENT_BROWSER_ACTION_POLICY` env) |
| `--confirm-actions <list>` | Action categories requiring confirmation (or `AGENT_BROWSER_CONFIRM_ACTIONS` env) |
| `--confirm-interactive` | Interactive confirmation prompts; auto-denies if stdin is not a TTY (or `AGENT_BROWSER_CONFIRM_INTERACTIVE` env) |
| `--engine <name>` | Browser engine: `chrome` (default), `lightpanda` (or `AGENT_BROWSER_ENGINE` env) |
| `--config <path>` | Use a custom config file (or `AGENT_BROWSER_CONFIG` env) |
| `--debug` | Debug output |

## Architecture

```
Rust CLI ──IPC──▶ Rust Daemon ──CDP──▶ Chrome / Lightpanda
                       │               Browserless / Browserbase
                       │               Browser Use / Kernel
                       └──WebDriver──▶ iOS Safari (Appium)
```

The daemon starts automatically on first command and persists between commands. No Node.js required at runtime.

## Platforms

| Platform    | Binary      |
| ----------- | ----------- |
| macOS ARM64 | Native Rust |
| macOS x64   | Native Rust |
| Linux ARM64 | Native Rust |
| Linux x64   | Native Rust |
| Windows x64 | Native Rust |

## License

Apache-2.0
