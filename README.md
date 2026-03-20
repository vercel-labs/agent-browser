# agent-browser

Headless browser automation CLI for AI agents. Fast native Rust CLI.

## Installation

### Global Installation (recommended)

Installs the native Rust binary:

```bash
npm install -g agent-browser
agent-browser install  # Download Chrome from Chrome for Testing (first time only)
```

### Project Installation (local dependency)

For projects that want to pin the version in `package.json`:

```bash
npm install agent-browser
agent-browser install
```

Then use via `package.json` scripts or by invoking `agent-browser` directly.

### Homebrew (macOS)

```bash
brew install agent-browser
agent-browser install  # Download Chrome from Chrome for Testing (first time only)
```

### Cargo (Rust)

```bash
cargo install agent-browser
agent-browser install  # Download Chrome from Chrome for Testing (first time only)
```

### From Source

```bash
git clone https://github.com/vercel-labs/agent-browser
cd agent-browser
pnpm install
pnpm build
pnpm build:native   # Requires Rust (https://rustup.rs)
pnpm link --global  # Makes agent-browser available globally
agent-browser install
```

### Linux Dependencies

On Linux, install system dependencies:

```bash
agent-browser install --with-deps
```

### Updating

Upgrade to the latest version:

```bash
agent-browser upgrade
```

Detects your installation method (npm, Homebrew, or Cargo) and runs the appropriate update command automatically.

### Requirements

- **Chrome** - Run `agent-browser install` to download Chrome from [Chrome for Testing](https://developer.chrome.com/blog/chrome-for-testing/) (Google's official automation channel). No Playwright or Node.js required for the daemon.
- **Rust** - Only needed when building from source (see From Source above).

## Quick Start

```bash
agent-browser open example.com
agent-browser snapshot                    # Get accessibility tree with refs
agent-browser click @e2                   # Click by ref from snapshot
agent-browser fill @e3 "test@example.com" # Fill by ref
agent-browser get text @e1                # Get text by ref
agent-browser screenshot page.png
agent-browser close
```

### Traditional Selectors (also supported)

```bash
agent-browser click "#submit"
agent-browser fill "#email" "test@example.com"
agent-browser find role button click --name "Submit"
```

## Commands

### Core Commands

```bash
agent-browser open <url>              # Navigate to URL (aliases: goto, navigate)
agent-browser click <sel>             # Click element (--new-tab to open in new tab)
agent-browser dblclick <sel>          # Double-click element
agent-browser focus <sel>             # Focus element
agent-browser type <sel> <text>       # Type into element
agent-browser fill <sel> <text>       # Clear and fill
agent-browser press <key>             # Press key (Enter, Tab, Control+a) (alias: key)
agent-browser keyboard type <text>    # Type with real keystrokes (no selector, current focus)
agent-browser keyboard inserttext <text>  # Insert text without key events (no selector)
agent-browser keydown <key>           # Hold key down
agent-browser keyup <key>             # Release key
agent-browser hover <sel>             # Hover element
agent-browser select <sel> <val>      # Select dropdown option
agent-browser check <sel>             # Check checkbox
agent-browser uncheck <sel>           # Uncheck checkbox
agent-browser scroll <dir> [px]       # Scroll (up/down/left/right, --selector <sel>)
agent-browser scrollintoview <sel>    # Scroll element into view (alias: scrollinto)
agent-browser drag <src> <tgt>        # Drag and drop
agent-browser upload <sel> <files>    # Upload files
agent-browser screenshot [path]       # Take screenshot (--full for full page, saves to a temporary directory if no path)
agent-browser screenshot --annotate   # Annotated screenshot with numbered element labels
agent-browser screenshot --screenshot-dir ./shots    # Save to custom directory
agent-browser screenshot --screenshot-format jpeg --screenshot-quality 80
agent-browser pdf <path>              # Save as PDF
agent-browser snapshot                # Accessibility tree with refs (best for AI)
agent-browser eval <js>               # Run JavaScript (-b for base64, --stdin for piped input)
agent-browser connect <port>          # Connect to browser via CDP
agent-browser close                   # Close browser (aliases: quit, exit)
```

### Get Info

```bash
agent-browser get text <sel>          # Get text content
agent-browser get html <sel>          # Get innerHTML
agent-browser get value <sel>         # Get input value
agent-browser get attr <sel> <attr>   # Get attribute
agent-browser get title               # Get page title
agent-browser get url                 # Get current URL
agent-browser get cdp-url             # Get CDP WebSocket URL (for DevTools, debugging)
agent-browser get count <sel>         # Count matching elements
agent-browser get box <sel>           # Get bounding box
agent-browser get styles <sel>        # Get computed styles
```

### Check State

```bash
agent-browser is visible <sel>        # Check if visible
agent-browser is enabled <sel>        # Check if enabled
agent-browser is checked <sel>        # Check if checked
```

### Find Elements (Semantic Locators)

```bash
agent-browser find role <role> <action> [value]       # By ARIA role
agent-browser find text <text> <action>               # By text content
agent-browser find label <label> <action> [value]     # By label
agent-browser find placeholder <ph> <action> [value]  # By placeholder
agent-browser find alt <text> <action>                # By alt text
agent-browser find title <text> <action>              # By title attr
agent-browser find testid <id> <action> [value]       # By data-testid
agent-browser find first <sel> <action> [value]       # First match
agent-browser find last <sel> <action> [value]        # Last match
agent-browser find nth <n> <sel> <action> [value]     # Nth match
```

**Actions:** `click`, `fill`, `type`, `hover`, `focus`, `check`, `uncheck`, `text`

**Options:** `--name <name>` (filter role by accessible name), `--exact` (require exact text match)

**Examples:**

```bash
agent-browser find role button click --name "Submit"
agent-browser find text "Sign In" click
agent-browser find label "Email" fill "test@test.com"
agent-browser find first ".item" click
agent-browser find nth 2 "a" text
```

### Wait

```bash
agent-browser wait <selector>         # Wait for element to be visible
agent-browser wait <ms>               # Wait for time (milliseconds)
agent-browser wait --text "Welcome"   # Wait for text to appear (substring match)
agent-browser wait --url "**/dash"    # Wait for URL pattern
agent-browser wait --load networkidle # Wait for load state
agent-browser wait --fn "window.ready === true"  # Wait for JS condition

# Wait for text/element to disappear
agent-browser wait --fn "!document.body.innerText.includes('Loading...')"
agent-browser wait "#spinner" --state hidden
```

**Load states:** `load`, `domcontentloaded`, `networkidle`

### Batch Execution

Execute multiple commands in a single invocation by piping a JSON array of
string arrays to `batch`. This avoids per-command process startup overhead
when running multi-step workflows.

```bash
# Pipe commands as JSON
echo '[
  ["open", "https://example.com"],
  ["snapshot", "-i"],
  ["click", "@e1"],
  ["screenshot", "result.png"]
]' | agent-browser batch --json

# Stop on first error
agent-browser batch --bail < commands.json
```

### Clipboard

```bash
agent-browser clipboard read                      # Read text from clipboard
agent-browser clipboard write "Hello, World!"     # Write text to clipboard
agent-browser clipboard copy                      # Copy current selection (Ctrl+C)
agent-browser clipboard paste                     # Paste from clipboard (Ctrl+V)
```

### Mouse Control

```bash
agent-browser mouse move <x> <y>      # Move mouse
agent-browser mouse down [button]     # Press button (left/right/middle)
agent-browser mouse up [button]       # Release button
agent-browser mouse wheel <dy> [dx]   # Scroll wheel
```

### Browser Settings

```bash
agent-browser set viewport <w> <h> [scale]  # Set viewport size (scale for retina, e.g. 2)
agent-browser set device <name>       # Emulate device ("iPhone 14")
agent-browser set geo <lat> <lng>     # Set geolocation
agent-browser set offline [on|off]    # Toggle offline mode
agent-browser set headers <json>      # Extra HTTP headers
agent-browser set credentials <u> <p> # HTTP basic auth
agent-browser set media [dark|light]  # Emulate color scheme
```

### Cookies & Storage

```bash
agent-browser cookies                 # Get all cookies
agent-browser cookies set <name> <val> # Set cookie
agent-browser cookies clear           # Clear cookies

agent-browser storage local           # Get all localStorage
agent-browser storage local <key>     # Get specific key
agent-browser storage local set <k> <v>  # Set value
agent-browser storage local clear     # Clear all

agent-browser storage session         # Same for sessionStorage
```

### Network

```bash
agent-browser network route <url>              # Intercept requests
agent-browser network route <url> --abort      # Block requests
agent-browser network route <url> --body <json>  # Mock response
agent-browser network unroute [url]            # Remove routes
agent-browser network requests                 # View tracked requests
agent-browser network requests --filter api    # Filter requests
agent-browser network har start                # Start HAR recording
agent-browser network har stop [output.har]    # Stop and save HAR (temp path if omitted)
```

### Tabs & Windows

```bash
agent-browser tab                     # List tabs
agent-browser tab new [url]           # New tab (optionally with URL)
agent-browser tab <n>                 # Switch to tab n
agent-browser tab close [n]           # Close tab
agent-browser window new              # New window
```

### Frames

```bash
agent-browser frame <sel>             # Switch to iframe
agent-browser frame main              # Back to main frame
```

### Dialogs

```bash
agent-browser dialog accept [text]    # Accept (with optional prompt text)
agent-browser dialog dismiss          # Dismiss
```

### Diff

```bash
agent-browser diff snapshot                              # Compare current vs last snapshot
agent-browser diff snapshot --baseline before.txt        # Compare current vs saved snapshot file
agent-browser diff snapshot --selector "#main" --compact # Scoped snapshot diff
agent-browser diff screenshot --baseline before.png      # Visual pixel diff against baseline
agent-browser diff screenshot --baseline b.png -o d.png  # Save diff image to custom path
agent-browser diff screenshot --baseline b.png -t 0.2    # Adjust color threshold (0-1)
agent-browser diff url https://v1.com https://v2.com     # Compare two URLs (snapshot diff)
agent-browser diff url https://v1.com https://v2.com --screenshot  # Also visual diff
agent-browser diff url https://v1.com https://v2.com --wait-until networkidle  # Custom wait strategy
agent-browser diff url https://v1.com https://v2.com --selector "#main"  # Scope to element
```

### Debug

```bash
agent-browser trace start [path]      # Start recording trace
agent-browser trace stop [path]       # Stop and save trace
agent-browser profiler start          # Start Chrome DevTools profiling
agent-browser profiler stop [path]    # Stop and save profile (.json)
agent-browser console                 # View console messages (log, error, warn, info)
agent-browser console --clear         # Clear console
agent-browser errors                  # View page errors (uncaught JavaScript exceptions)
agent-browser errors --clear          # Clear errors
agent-browser highlight <sel>         # Highlight element
agent-browser inspect                 # Open Chrome DevTools for the active page
agent-browser state save <path>       # Save auth state
agent-browser state load <path>       # Load auth state
agent-browser state list              # List saved state files
agent-browser state show <file>       # Show state summary
agent-browser state rename <old> <new> # Rename state file
agent-browser state clear [name]      # Clear states for session
agent-browser state clear --all       # Clear all saved states
agent-browser state clean --older-than <days>  # Delete old states
```

### Navigation

```bash
agent-browser back                    # Go back
agent-browser forward                 # Go forward
agent-browser reload                  # Reload page
```

### Setup

```bash
agent-browser install                 # Download Chrome from Chrome for Testing (Google's official automation channel)
agent-browser install --with-deps     # Also install system deps (Linux)
agent-browser upgrade                 # Upgrade agent-browser to the latest version
```

## Authentication

agent-browser provides multiple ways to persist login sessions so you don't re-authenticate every run.

### Quick summary

| Approach | Best for | Flag / Env |
|----------|----------|------------|
| **Persistent profile** | Full browser state (cookies, IndexedDB, service workers, cache) across restarts | `--profile <path>` / `AGENT_BROWSER_PROFILE` |
| **Session persistence** | Auto-save/restore cookies + localStorage by name | `--session-name <name>` / `AGENT_BROWSER_SESSION_NAME` |
| **Import from your browser** | Grab auth from a Chrome session you already logged into | `--auto-connect` + `state save` |
| **State file** | Load a previously saved state JSON on launch | `--state <path>` / `AGENT_BROWSER_STATE` |
| **Auth vault** | Store credentials locally (encrypted), login by name | `auth save` / `auth login` |

### Import auth from your browser

If you are already logged in to a site in Chrome, you can grab that auth state and reuse it:

```bash
# 1. Launch Chrome with remote debugging enabled
#    macOS:
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --remote-debugging-port=9222
#    Or use --auto-connect to discover an already-running Chrome

# 2. Connect and save the authenticated state
agent-browser --auto-connect state save ./my-auth.json

# 3. Use the saved auth in future sessions
agent-browser --state ./my-auth.json open https://app.example.com/dashboard

# 4. Or use --session-name for automatic persistence
agent-browser --session-name myapp state load ./my-auth.json
# From now on, --session-name myapp auto-saves/restores this state
```

> **Security notes:**
> - `--remote-debugging-port` exposes full browser control on localhost. Any local process can connect. Only use on trusted machines and close Chrome when done.
> - State files contain session tokens in plaintext. Add them to `.gitignore` and delete when no longer needed. For encryption at rest, set `AGENT_BROWSER_ENCRYPTION_KEY` (see [State Encryption](#state-encryption)).

For full details on login flows, OAuth, 2FA, cookie-based auth, and the auth vault, see the [Authentication](docs/src/app/sessions/page.mdx) docs.

## Sessions

Run multiple isolated browser instances:

```bash
# Different sessions
agent-browser --session agent1 open site-a.com
agent-browser --session agent2 open site-b.com

# Or via environment variable
AGENT_BROWSER_SESSION=agent1 agent-browser click "#btn"

# List active sessions
agent-browser session list
# Output:
# Active sessions:
# -> default
#    agent1

# Show current session
agent-browser session
```

Each session has its own:

- Browser instance
- Cookies and storage
- Navigation history
- Authentication state

## Persistent Profiles

By default, browser state (cookies, localStorage, login sessions) is ephemeral and lost when the browser closes. Use `--profile` to persist state across browser restarts:

```bash
# Use a persistent profile directory
agent-browser --profile ~/.myapp-profile open myapp.com

# Login once, then reuse the authenticated session
agent-browser --profile ~/.myapp-profile open myapp.com/dashboard

# Or via environment variable
AGENT_BROWSER_PROFILE=~/.myapp-profile agent-browser open myapp.com
```

The profile directory stores:

- Cookies and localStorage
- IndexedDB data
- Service workers
- Browser cache
- Login sessions

**Tip**: Use different profile paths for different projects to keep their browser state isolated.

## Session Persistence

Alternatively, use `--session-name` to automatically save and restore cookies and localStorage across browser restarts:

```bash
# Auto-save/load state for "twitter" session
agent-browser --session-name twitter open twitter.com

# Login once, then state persists automatically
# State files stored in ~/.agent-browser/sessions/

# Or via environment variable
export AGENT_BROWSER_SESSION_NAME=twitter
agent-browser open twitter.com
```

### State Encryption

Encrypt saved session data at rest with AES-256-GCM:

```bash
# Generate key: openssl rand -hex 32
export AGENT_BROWSER_ENCRYPTION_KEY=<64-char-hex-key>

# State files are now encrypted automatically
agent-browser --session-name secure open example.com
```

| Variable                          | Description                                        |
| --------------------------------- | -------------------------------------------------- |
| `AGENT_BROWSER_SESSION_NAME`      | Auto-save/load state persistence name              |
| `AGENT_BROWSER_ENCRYPTION_KEY`    | 64-char hex key for AES-256-GCM encryption         |
| `AGENT_BROWSER_STATE_EXPIRE_DAYS` | Auto-delete states older than N days (default: 30) |

## Security

agent-browser includes security features for safe AI agent deployments. All features are opt-in -- existing workflows are unaffected until you explicitly enable a feature:

- **Authentication Vault** -- Store credentials locally (always encrypted), reference by name. The LLM never sees passwords. `auth login` navigates with `load` and then waits for login form selectors to appear (SPA-friendly, timeout follows the default action timeout). A key is auto-generated at `~/.agent-browser/.encryption-key` if `AGENT_BROWSER_ENCRYPTION_KEY` is not set: `echo "pass" | agent-browser auth save github --url https://github.com/login --username user --password-stdin` then `agent-browser auth login github`
- **Content Boundary Markers** -- Wrap page output in delimiters so LLMs can distinguish tool output from untrusted content: `--content-boundaries`
- **Domain Allowlist** -- Restrict navigation to trusted domains (wildcards like `*.example.com` also match the bare domain): `--allowed-domains "example.com,*.example.com"`. Sub-resource requests (scripts, images, fetch) and WebSocket/EventSource connections to non-allowed domains are also blocked. Include any CDN domains your target pages depend on (e.g., `*.cdn.example.com`).
- **Action Policy** -- Gate destructive actions with a static policy file: `--action-policy ./policy.json`
- **Action Confirmation** -- Require explicit approval for sensitive action categories: `--confirm-actions eval,download`
- **Output Length Limits** -- Prevent context flooding: `--max-output 50000`

| Variable                            | Description                              |
| ----------------------------------- | ---------------------------------------- |
| `AGENT_BROWSER_CONTENT_BOUNDARIES`  | Wrap page output in boundary markers     |
| `AGENT_BROWSER_MAX_OUTPUT`          | Max characters for page output           |
| `AGENT_BROWSER_ALLOWED_DOMAINS`     | Comma-separated allowed domain patterns  |
| `AGENT_BROWSER_ACTION_POLICY`       | Path to action policy JSON file          |
| `AGENT_BROWSER_CONFIRM_ACTIONS`     | Action categories requiring confirmation |
| `AGENT_BROWSER_CONFIRM_INTERACTIVE` | Enable interactive confirmation prompts  |

See [Security documentation](https://agent-browser.dev/security) for details.

## Snapshot Options

The `snapshot` command supports filtering to reduce output size:

```bash
agent-browser snapshot                    # Full accessibility tree
agent-browser snapshot -i                 # Interactive elements only (buttons, inputs, links)
agent-browser snapshot -i -C              # Include cursor-interactive elements (divs with onclick, etc.)
agent-browser snapshot -c                 # Compact (remove empty structural elements)
agent-browser snapshot -d 3               # Limit depth to 3 levels
agent-browser snapshot -s "#main"         # Scope to CSS selector
agent-browser snapshot -i -c -d 5         # Combine options
```

| Option                 | Description                                                             |
| ---------------------- | ----------------------------------------------------------------------- |
| `-i, --interactive`    | Only show interactive elements (buttons, links, inputs)                 |
| `-C, --cursor`         | Include cursor-interactive elements (cursor:pointer, onclick, tabindex) |
| `-c, --compact`        | Remove empty structural elements                                        |
| `-d, --depth <n>`      | Limit tree depth                                                        |
| `-s, --selector <sel>` | Scope to CSS selector                                                   |

The `-C` flag is useful for modern web apps that use custom clickable elements (divs, spans) instead of standard buttons/links.

## Annotated Screenshots

The `--annotate` flag overlays numbered labels on interactive elements in the screenshot. Each label `[N]` corresponds to ref `@eN`, so the same refs work for both visual and text-based workflows.

Annotated screenshots are supported on the CDP-backed browser path (Chrome/Lightpanda). The Safari/WebDriver backend does not yet support `--annotate`.

```bash
agent-browser screenshot --annotate
# -> Screenshot saved to /tmp/screenshot-2026-02-17T12-00-00-abc123.png
#    [1] @e1 button "Submit"
#    [2] @e2 link "Home"
#    [3] @e3 textbox "Email"
```

After an annotated screenshot, refs are cached so you can immediately interact with elements:

```bash
agent-browser screenshot --annotate ./page.png
agent-browser click @e2     # Click the "Home" link labeled [2]
```

This is useful for multimodal AI models that can reason about visual layout, unlabeled icon buttons, canvas elements, or visual state that the text accessibility tree cannot capture.

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

## Configuration

Create an `agent-browser.json` file to set persistent defaults instead of repeating flags on every command.

**Locations (lowest to highest priority):**

1. `~/.agent-browser/config.json` -- user-level defaults
2. `./agent-browser.json` -- project-level overrides (in working directory)
3. `AGENT_BROWSER_*` environment variables override config file values
4. CLI flags override everything

**Example `agent-browser.json`:**

```json
{
  "headed": true,
  "proxy": "http://localhost:8080",
  "profile": "./browser-data",
  "userAgent": "my-agent/1.0",
  "ignoreHttpsErrors": true
}
```

Use `--config <path>` or `AGENT_BROWSER_CONFIG` to load a specific config file instead of the defaults:

```bash
agent-browser --config ./ci-config.json open example.com
AGENT_BROWSER_CONFIG=./ci-config.json agent-browser open example.com
```

All options from the table above can be set in the config file using camelCase keys (e.g., `--executable-path` becomes `"executablePath"`, `--proxy-bypass` becomes `"proxyBypass"`). Unknown keys are ignored for forward compatibility.

Boolean flags accept an optional `true`/`false` value to override config settings. For example, `--headed false` disables `"headed": true` from config. A bare `--headed` is equivalent to `--headed true`.

Auto-discovered config files that are missing are silently ignored. If `--config <path>` points to a missing or invalid file, agent-browser exits with an error. Extensions from user and project configs are merged (concatenated), not replaced.

> **Tip:** If your project-level `agent-browser.json` contains environment-specific values (paths, proxies), consider adding it to `.gitignore`.

## Default Timeout

The default timeout for standard operations (clicks, waits, fills, etc.) is 25 seconds. This is intentionally below the CLI's 30-second IPC read timeout so that the daemon returns a proper error instead of the CLI timing out with EAGAIN.

Override the default timeout via environment variable:

```bash
# Set a longer timeout for slow pages (in milliseconds)
export AGENT_BROWSER_DEFAULT_TIMEOUT=45000
```

> **Note:** Setting this above 30000 (30s) may cause EAGAIN errors on slow operations because the CLI's read timeout will expire before the daemon responds. The CLI retries transient errors automatically, but response times will increase.

| Variable                        | Description                              |
| ------------------------------- | ---------------------------------------- |
| `AGENT_BROWSER_DEFAULT_TIMEOUT` | Default operation timeout in ms (default: 25000) |

## Selectors

### Refs (Recommended for AI)

Refs provide deterministic element selection from snapshots:

```bash
# 1. Get snapshot with refs
agent-browser snapshot
# Output:
# - heading "Example Domain" [ref=e1] [level=1]
# - button "Submit" [ref=e2]
# - textbox "Email" [ref=e3]
# - link "Learn more" [ref=e4]

# 2. Use refs to interact
agent-browser click @e2                   # Click the button
agent-browser fill @e3 "test@example.com" # Fill the textbox
agent-browser get text @e1                # Get heading text
agent-browser hover @e4                   # Hover the link
```

**Why use refs?**

- **Deterministic**: Ref points to exact element from snapshot
- **Fast**: No DOM re-query needed
- **AI-friendly**: Snapshot + ref workflow is optimal for LLMs

### CSS Selectors

```bash
agent-browser click "#id"
agent-browser click ".class"
agent-browser click "div > button"
```

### Text & XPath

```bash
agent-browser click "text=Submit"
agent-browser click "xpath=//button"
```

### Semantic Locators

```bash
agent-browser find role button click --name "Submit"
agent-browser find label "Email" fill "test@test.com"
```

## Agent Mode

Use `--json` for machine-readable output:

```bash
agent-browser snapshot --json
# Returns: {"success":true,"data":{"snapshot":"...","refs":{"e1":{"role":"heading","name":"Title"},...}}}

agent-browser get text @e1 --json
agent-browser is visible @e2 --json
```

### Optimal AI Workflow

```bash
# 1. Navigate and get snapshot
agent-browser open example.com
agent-browser snapshot -i --json   # AI parses tree and refs

# 2. AI identifies target refs from snapshot
# 3. Execute actions using refs
agent-browser click @e2
agent-browser fill @e3 "input text"

# 4. Get new snapshot if page changed
agent-browser snapshot -i --json
```

### Command Chaining

Commands can be chained with `&&` in a single shell invocation. The browser persists via a background daemon, so chaining is safe and more efficient:

```bash
# Open, wait for load, and snapshot in one call
agent-browser open example.com && agent-browser wait --load networkidle && agent-browser snapshot -i

# Chain multiple interactions
agent-browser fill @e1 "user@example.com" && agent-browser fill @e2 "pass" && agent-browser click @e3

# Navigate and screenshot
agent-browser open example.com && agent-browser wait --load networkidle && agent-browser screenshot page.png
```

Use `&&` when you don't need intermediate output. Run commands separately when you need to parse output first (e.g., snapshot to discover refs before interacting).

## Headed Mode

Show the browser window for debugging:

```bash
agent-browser open example.com --headed
```

This opens a visible browser window instead of running headless.

> **Note:** Browser extensions work in both headed and headless mode (Chrome's `--headless=new`).

## Authenticated Sessions

Use `--headers` to set HTTP headers for a specific origin, enabling authentication without login flows:

```bash
# Headers are scoped to api.example.com only
agent-browser open api.example.com --headers '{"Authorization": "Bearer <token>"}'

# Requests to api.example.com include the auth header
agent-browser snapshot -i --json
agent-browser click @e2

# Navigate to another domain - headers are NOT sent (safe!)
agent-browser open other-site.com
```

This is useful for:

- **Skipping login flows** - Authenticate via headers instead of UI
- **Switching users** - Start new sessions with different auth tokens
- **API testing** - Access protected endpoints directly
- **Security** - Headers are scoped to the origin, not leaked to other domains

To set headers for multiple origins, use `--headers` with each `open` command:

```bash
agent-browser open api.example.com --headers '{"Authorization": "Bearer token1"}'
agent-browser open api.acme.com --headers '{"Authorization": "Bearer token2"}'
```

For global headers (all domains), use `set headers`:

```bash
agent-browser set headers '{"X-Custom-Header": "value"}'
```

## Custom Browser Executable

Use a custom browser executable instead of the bundled Chromium. This is useful for:

- **Serverless deployment**: Use lightweight Chromium builds like `@sparticuz/chromium` (~50MB vs ~684MB)
- **System browsers**: Use an existing Chrome/Chromium installation
- **Custom builds**: Use modified browser builds

### CLI Usage

```bash
# Via flag
agent-browser --executable-path /path/to/chromium open example.com

# Via environment variable
AGENT_BROWSER_EXECUTABLE_PATH=/path/to/chromium agent-browser open example.com
```

### Serverless (Vercel)

Run agent-browser + Chrome in an ephemeral Vercel Sandbox microVM. No external server needed:

```typescript
import { Sandbox } from "@vercel/sandbox";

const sandbox = await Sandbox.create({ runtime: "node24" });
await sandbox.runCommand("agent-browser", ["open", "https://example.com"]);
const result = await sandbox.runCommand("agent-browser", ["screenshot", "--json"]);
await sandbox.stop();
```

See the [environments example](examples/environments/) for a working demo with a UI and deploy-to-Vercel button.

### Serverless (AWS Lambda)

```typescript
import chromium from '@sparticuz/chromium';
import { execSync } from 'child_process';

export async function handler() {
  const executablePath = await chromium.executablePath();
  const result = execSync(
    `AGENT_BROWSER_EXECUTABLE_PATH=${executablePath} agent-browser open https://example.com && agent-browser snapshot -i --json`,
    { encoding: 'utf-8' }
  );
  return JSON.parse(result);
}
```

## Local Files

Open and interact with local files (PDFs, HTML, etc.) using `file://` URLs:

```bash
# Enable file access (required for JavaScript to access local files)
agent-browser --allow-file-access open file:///path/to/document.pdf
agent-browser --allow-file-access open file:///path/to/page.html

# Take screenshot of a local PDF
agent-browser --allow-file-access open file:///Users/me/report.pdf
agent-browser screenshot report.png
```

The `--allow-file-access` flag adds Chromium flags (`--allow-file-access-from-files`, `--allow-file-access`) that allow `file://` URLs to:

- Load and render local files
- Access other local files via JavaScript (XHR, fetch)
- Load local resources (images, scripts, stylesheets)

**Note:** This flag only works with Chromium. For security, it's disabled by default.

## CDP Mode

Connect to an existing browser via Chrome DevTools Protocol:

```bash
# Start Chrome with: google-chrome --remote-debugging-port=9222

# Connect once, then run commands without --cdp
agent-browser connect 9222
agent-browser snapshot
agent-browser tab
agent-browser close

# Or pass --cdp on each command
agent-browser --cdp 9222 snapshot

# Connect to remote browser via WebSocket URL
agent-browser --cdp "wss://your-browser-service.com/cdp?token=..." snapshot
```

The `--cdp` flag accepts either:

- A port number (e.g., `9222`) for local connections via `http://localhost:{port}`
- A full WebSocket URL (e.g., `wss://...` or `ws://...`) for remote browser services

This enables control of:

- Electron apps
- Chrome/Chromium instances with remote debugging
- WebView2 applications
- Any browser exposing a CDP endpoint

### Auto-Connect

Use `--auto-connect` to automatically discover and connect to a running Chrome instance without specifying a port:

```bash
# Auto-discover running Chrome with remote debugging
agent-browser --auto-connect open example.com
agent-browser --auto-connect snapshot

# Or via environment variable
AGENT_BROWSER_AUTO_CONNECT=1 agent-browser snapshot
```

Auto-connect discovers Chrome by:

1. Reading Chrome's `DevToolsActivePort` file from the default user data directory
2. Falling back to probing common debugging ports (9222, 9229)
3. If HTTP-based discovery (`/json/version`, `/json/list`) fails, falling back to a direct WebSocket connection

This is useful when:

- Chrome 144+ has remote debugging enabled via `chrome://inspect/#remote-debugging` (which uses a dynamic port)
- You want a zero-configuration connection to your existing browser
- You don't want to track which port Chrome is using

## Streaming (Browser Preview)

Stream the browser viewport via WebSocket for live preview or "pair browsing" where a human can watch and interact alongside an AI agent.

### Enable Streaming

Set the `AGENT_BROWSER_STREAM_PORT` environment variable:

```bash
AGENT_BROWSER_STREAM_PORT=9223 agent-browser open example.com
```

This starts a WebSocket server on the specified port that streams the browser viewport and accepts input events.

### WebSocket Protocol

Connect to `ws://localhost:9223` to receive frames and send input:

**Receive frames:**

```json
{
  "type": "frame",
  "data": "<base64-encoded-jpeg>",
  "metadata": {
    "deviceWidth": 1280,
    "deviceHeight": 720,
    "pageScaleFactor": 1,
    "offsetTop": 0,
    "scrollOffsetX": 0,
    "scrollOffsetY": 0
  }
}
```

**Send mouse events:**

```json
{
  "type": "input_mouse",
  "eventType": "mousePressed",
  "x": 100,
  "y": 200,
  "button": "left",
  "clickCount": 1
}
```

**Send keyboard events:**

```json
{
  "type": "input_keyboard",
  "eventType": "keyDown",
  "key": "Enter",
  "code": "Enter"
}
```

**Send touch events:**

```json
{
  "type": "input_touch",
  "eventType": "touchStart",
  "touchPoints": [{ "x": 100, "y": 200 }]
}
```

## Architecture

agent-browser uses a client-daemon architecture:

1. **Rust CLI** - Parses commands, communicates with daemon
2. **Rust Daemon** - Pure Rust daemon using direct CDP, no Node.js required

The daemon starts automatically on first command and persists between commands for fast subsequent operations. To auto-shutdown the daemon after a period of inactivity, set `AGENT_BROWSER_IDLE_TIMEOUT_MS` (value in milliseconds). When set, the daemon closes the browser and exits after receiving no commands for the specified duration.

**Browser Engine:** Uses Chrome (from Chrome for Testing) by default. The `--engine` flag selects between `chrome` and `lightpanda`. Supported browsers: Chromium/Chrome (via CDP) and Safari (via WebDriver for iOS).

## Platforms

| Platform    | Binary      |
| ----------- | ----------- |
| macOS ARM64 | Native Rust |
| macOS x64   | Native Rust |
| Linux ARM64 | Native Rust |
| Linux x64   | Native Rust |
| Windows x64 | Native Rust |

## Usage with AI Agents

### Just ask the agent

The simplest approach -- just tell your agent to use it:

```
Use agent-browser to test the login flow. Run agent-browser --help to see available commands.
```

The `--help` output is comprehensive and most agents can figure it out from there.

### AI Coding Assistants (recommended)

Add the skill to your AI coding assistant for richer context:

```bash
npx skills add vercel-labs/agent-browser
```

This works with Claude Code, Codex, Cursor, Gemini CLI, GitHub Copilot, Goose, OpenCode, and Windsurf. The skill is fetched from the repository, so it stays up to date automatically -- do not copy `SKILL.md` from `node_modules` as it will become stale.

### Claude Code

Install as a Claude Code skill:

```bash
npx skills add vercel-labs/agent-browser
```

This adds the skill to `.claude/skills/agent-browser/SKILL.md` in your project. The skill teaches Claude Code the full agent-browser workflow, including the snapshot-ref interaction pattern, session management, and timeout handling.

### AGENTS.md / CLAUDE.md

For more consistent results, add to your project or global instructions file:

```markdown
## Browser Automation

Use `agent-browser` for web automation. Run `agent-browser --help` for all commands.

Core workflow:

1. `agent-browser open <url>` - Navigate to page
2. `agent-browser snapshot -i` - Get interactive elements with refs (@e1, @e2)
3. `agent-browser click @e1` / `fill @e2 "text"` - Interact using refs
4. Re-snapshot after page changes
```

## Integrations

### iOS Simulator

Control real Mobile Safari in the iOS Simulator for authentic mobile web testing. Requires macOS with Xcode.

**Setup:**

```bash
# Install Appium and XCUITest driver
npm install -g appium
appium driver install xcuitest
```

**Usage:**

```bash
# List available iOS simulators
agent-browser device list

# Launch Safari on a specific device
agent-browser -p ios --device "iPhone 16 Pro" open https://example.com

# Same commands as desktop
agent-browser -p ios snapshot -i
agent-browser -p ios tap @e1
agent-browser -p ios fill @e2 "text"
agent-browser -p ios screenshot mobile.png

# Mobile-specific commands
agent-browser -p ios swipe up
agent-browser -p ios swipe down 500

# Close session
agent-browser -p ios close
```

Or use environment variables:

```bash
export AGENT_BROWSER_PROVIDER=ios
export AGENT_BROWSER_IOS_DEVICE="iPhone 16 Pro"
agent-browser open https://example.com
```

| Variable                   | Description                                     |
| -------------------------- | ----------------------------------------------- |
| `AGENT_BROWSER_PROVIDER`   | Set to `ios` to enable iOS mode                 |
| `AGENT_BROWSER_IOS_DEVICE` | Device name (e.g., "iPhone 16 Pro", "iPad Pro") |
| `AGENT_BROWSER_IOS_UDID`   | Device UDID (alternative to device name)        |

**Supported devices:** All iOS Simulators available in Xcode (iPhones, iPads), plus real iOS devices.

**Note:** The iOS provider boots the simulator, starts Appium, and controls Safari. First launch takes ~30-60 seconds; subsequent commands are fast.

#### Real Device Support

Appium also supports real iOS devices connected via USB. This requires additional one-time setup:

**1. Get your device UDID:**

```bash
xcrun xctrace list devices
# or
system_profiler SPUSBDataType | grep -A 5 "iPhone\|iPad"
```

**2. Sign WebDriverAgent (one-time):**

```bash
# Open the WebDriverAgent Xcode project
cd ~/.appium/node_modules/appium-xcuitest-driver/node_modules/appium-webdriveragent
open WebDriverAgent.xcodeproj
```

In Xcode:

- Select the `WebDriverAgentRunner` target
- Go to Signing & Capabilities
- Select your Team (requires Apple Developer account, free tier works)
- Let Xcode manage signing automatically

**3. Use with agent-browser:**

```bash
# Connect device via USB, then:
agent-browser -p ios --device "<DEVICE_UDID>" open https://example.com

# Or use the device name if unique
agent-browser -p ios --device "John's iPhone" open https://example.com
```

**Real device notes:**

- First run installs WebDriverAgent to the device (may require Trust prompt)
- Device must be unlocked and connected via USB
- Slightly slower initial connection than simulator
- Tests against real Safari performance and behavior

### Browserless

[Browserless](https://browserless.io) provides cloud browser infrastructure with a Sessions API. Use it when running agent-browser in environments where a local browser isn't available.

To enable Browserless, use the `-p` flag:

```bash
export BROWSERLESS_API_KEY="your-api-token"
agent-browser -p browserless open https://example.com
```

Or use environment variables for CI/scripts:

```bash
export AGENT_BROWSER_PROVIDER=browserless
export BROWSERLESS_API_KEY="your-api-token"
agent-browser open https://example.com
```

Optional configuration via environment variables:

| Variable                   | Description                                      | Default                                 |
| -------------------------- | ------------------------------------------------ | --------------------------------------- |
| `BROWSERLESS_API_URL`      | Base API URL (for custom regions or self-hosted) | `https://production-sfo.browserless.io` |
| `BROWSERLESS_BROWSER_TYPE` | Type of browser to use (chromium or chrome)      | chromium                                |
| `BROWSERLESS_TTL`          | Session TTL in milliseconds                      | `300000`                                |
| `BROWSERLESS_STEALTH`      | Enable stealth mode (`true`/`false`)             | `true`                                  |

When enabled, agent-browser connects to a Browserless cloud session instead of launching a local browser. All commands work identically.

Get your API token from the [Browserless Dashboard](https://browserless.io).

### Browserbase

[Browserbase](https://browserbase.com) provides remote browser infrastructure to make deployment of agentic browsing agents easy. Use it when running the agent-browser CLI in an environment where a local browser isn't feasible.

To enable Browserbase, use the `-p` flag:

```bash
export BROWSERBASE_API_KEY="your-api-key"
agent-browser -p browserbase open https://example.com
```

Or use environment variables for CI/scripts:

```bash
export AGENT_BROWSER_PROVIDER=browserbase
export BROWSERBASE_API_KEY="your-api-key"
agent-browser open https://example.com
```

When enabled, agent-browser connects to a Browserbase session instead of launching a local browser. All commands work identically.

Get your API key from the [Browserbase Dashboard](https://browserbase.com/overview).

### Browser Use

[Browser Use](https://browser-use.com) provides cloud browser infrastructure for AI agents. Use it when running agent-browser in environments where a local browser isn't available (serverless, CI/CD, etc.).

To enable Browser Use, use the `-p` flag:

```bash
export BROWSER_USE_API_KEY="your-api-key"
agent-browser -p browseruse open https://example.com
```

Or use environment variables for CI/scripts:

```bash
export AGENT_BROWSER_PROVIDER=browseruse
export BROWSER_USE_API_KEY="your-api-key"
agent-browser open https://example.com
```

When enabled, agent-browser connects to a Browser Use cloud session instead of launching a local browser. All commands work identically.

Get your API key from the [Browser Use Cloud Dashboard](https://cloud.browser-use.com/settings?tab=api-keys). Free credits are available to get started, with pay-as-you-go pricing after.

### Kernel

[Kernel](https://www.kernel.sh) provides cloud browser infrastructure for AI agents with features like stealth mode and persistent profiles.

To enable Kernel, use the `-p` flag:

```bash
export KERNEL_API_KEY="your-api-key"
agent-browser -p kernel open https://example.com
```

Or use environment variables for CI/scripts:

```bash
export AGENT_BROWSER_PROVIDER=kernel
export KERNEL_API_KEY="your-api-key"
agent-browser open https://example.com
```

Optional configuration via environment variables:

| Variable                 | Description                                                                      | Default |
| ------------------------ | -------------------------------------------------------------------------------- | ------- |
| `KERNEL_HEADLESS`        | Run browser in headless mode (`true`/`false`)                                    | `false` |
| `KERNEL_STEALTH`         | Enable stealth mode to avoid bot detection (`true`/`false`)                      | `true`  |
| `KERNEL_TIMEOUT_SECONDS` | Session timeout in seconds                                                       | `300`   |
| `KERNEL_PROFILE_NAME`    | Browser profile name for persistent cookies/logins (created if it doesn't exist) | (none)  |

When enabled, agent-browser connects to a Kernel cloud session instead of launching a local browser. All commands work identically.

**Profile Persistence:** When `KERNEL_PROFILE_NAME` is set, the profile will be created if it doesn't already exist. Cookies, logins, and session data are automatically saved back to the profile when the browser session ends, making them available for future sessions.

Get your API key from the [Kernel Dashboard](https://dashboard.onkernel.com).

## License

Apache-2.0
