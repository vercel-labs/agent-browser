# BrowserFleet

Headless browser automation CLI for AI agents. Fast Rust CLI with Node.js fallback.

## Installation

### npm (recommended)

```bash
npm install -g browserfleet
browserfleet install  # Download Chromium
```

### From Source

```bash
git clone https://github.com/nmwcode/browserfleet
cd browserfleet
pnpm install
pnpm build
pnpm build:native   # Requires Rust (https://rustup.rs)
pnpm link --global  # Makes browserfleet available globally
browserfleet install
```

### Linux Dependencies

On Linux, install system dependencies:

```bash
browserfleet install --with-deps
# or manually: npx playwright install-deps chromium
```

## Quick Start

```bash
browserfleet open example.com
browserfleet snapshot                    # Get accessibility tree with refs
browserfleet click @e2                   # Click by ref from snapshot
browserfleet fill @e3 "test@example.com" # Fill by ref
browserfleet get text @e1                # Get text by ref
browserfleet screenshot page.png
browserfleet close
```

### Traditional Selectors (also supported)

```bash
browserfleet click "#submit"
browserfleet fill "#email" "test@example.com"
browserfleet find role button click --name "Submit"
```

## Commands

### Core Commands

```bash
browserfleet open <url>              # Navigate to URL (aliases: goto, navigate)
browserfleet click <sel>             # Click element
browserfleet dblclick <sel>          # Double-click element
browserfleet focus <sel>             # Focus element
browserfleet type <sel> <text>       # Type into element
browserfleet fill <sel> <text>       # Clear and fill
browserfleet press <key>             # Press key (Enter, Tab, Control+a) (alias: key)
browserfleet keydown <key>           # Hold key down
browserfleet keyup <key>             # Release key
browserfleet hover <sel>             # Hover element
browserfleet select <sel> <val>      # Select dropdown option
browserfleet check <sel>             # Check checkbox
browserfleet uncheck <sel>           # Uncheck checkbox
browserfleet scroll <dir> [px]       # Scroll (up/down/left/right)
browserfleet scrollintoview <sel>    # Scroll element into view (alias: scrollinto)
browserfleet drag <src> <tgt>        # Drag and drop
browserfleet upload <sel> <files>    # Upload files
browserfleet screenshot [path]       # Take screenshot (--full for full page)
browserfleet pdf <path>              # Save as PDF
browserfleet snapshot                # Accessibility tree with refs (best for AI)
browserfleet eval <js>               # Run JavaScript
browserfleet close                   # Close browser (aliases: quit, exit)
```

### Get Info

```bash
browserfleet get text <sel>          # Get text content
browserfleet get html <sel>          # Get innerHTML
browserfleet get value <sel>         # Get input value
browserfleet get attr <sel> <attr>   # Get attribute
browserfleet get title               # Get page title
browserfleet get url                 # Get current URL
browserfleet get count <sel>         # Count matching elements
browserfleet get box <sel>           # Get bounding box
```

### Check State

```bash
browserfleet is visible <sel>        # Check if visible
browserfleet is enabled <sel>        # Check if enabled
browserfleet is checked <sel>        # Check if checked
```

### Find Elements (Semantic Locators)

```bash
browserfleet find role <role> <action> [value]       # By ARIA role
browserfleet find text <text> <action>               # By text content
browserfleet find label <label> <action> [value]     # By label
browserfleet find placeholder <ph> <action> [value]  # By placeholder
browserfleet find alt <text> <action>                # By alt text
browserfleet find title <text> <action>              # By title attr
browserfleet find testid <id> <action> [value]       # By data-testid
browserfleet find first <sel> <action> [value]       # First match
browserfleet find last <sel> <action> [value]        # Last match
browserfleet find nth <n> <sel> <action> [value]     # Nth match
```

**Actions:** `click`, `fill`, `check`, `hover`, `text`

**Examples:**
```bash
browserfleet find role button click --name "Submit"
browserfleet find text "Sign In" click
browserfleet find label "Email" fill "test@test.com"
browserfleet find first ".item" click
browserfleet find nth 2 "a" text
```

### Wait

```bash
browserfleet wait <selector>         # Wait for element to be visible
browserfleet wait <ms>               # Wait for time (milliseconds)
browserfleet wait --text "Welcome"   # Wait for text to appear
browserfleet wait --url "**/dash"    # Wait for URL pattern
browserfleet wait --load networkidle # Wait for load state
browserfleet wait --fn "window.ready === true"  # Wait for JS condition
```

**Load states:** `load`, `domcontentloaded`, `networkidle`

### Mouse Control

```bash
browserfleet mouse move <x> <y>      # Move mouse
browserfleet mouse down [button]     # Press button (left/right/middle)
browserfleet mouse up [button]       # Release button
browserfleet mouse wheel <dy> [dx]   # Scroll wheel
```

### Browser Settings

```bash
browserfleet set viewport <w> <h>    # Set viewport size
browserfleet set device <name>       # Emulate device ("iPhone 14")
browserfleet set geo <lat> <lng>     # Set geolocation
browserfleet set offline [on|off]    # Toggle offline mode
browserfleet set headers <json>      # Extra HTTP headers
browserfleet set credentials <u> <p> # HTTP basic auth
browserfleet set media [dark|light]  # Emulate color scheme
```

### Cookies & Storage

```bash
browserfleet cookies                 # Get all cookies
browserfleet cookies set <name> <val> # Set cookie
browserfleet cookies clear           # Clear cookies

browserfleet storage local           # Get all localStorage
browserfleet storage local <key>     # Get specific key
browserfleet storage local set <k> <v>  # Set value
browserfleet storage local clear     # Clear all

browserfleet storage session         # Same for sessionStorage
```

### Network

```bash
browserfleet network route <url>              # Intercept requests
browserfleet network route <url> --abort      # Block requests
browserfleet network route <url> --body <json>  # Mock response
browserfleet network unroute [url]            # Remove routes
browserfleet network requests                 # View tracked requests
browserfleet network requests --filter api    # Filter requests
```

### Tabs & Windows

```bash
browserfleet tab                     # List tabs
browserfleet tab new [url]           # New tab (optionally with URL)
browserfleet tab <n>                 # Switch to tab n
browserfleet tab close [n]           # Close tab
browserfleet window new              # New window
```

### Frames

```bash
browserfleet frame <sel>             # Switch to iframe
browserfleet frame main              # Back to main frame
```

### Dialogs

```bash
browserfleet dialog accept [text]    # Accept (with optional prompt text)
browserfleet dialog dismiss          # Dismiss
```

### Debug

```bash
browserfleet trace start [path]      # Start recording trace
browserfleet trace stop [path]       # Stop and save trace
browserfleet console                 # View console messages
browserfleet console --clear         # Clear console
browserfleet errors                  # View page errors
browserfleet errors --clear          # Clear errors
browserfleet highlight <sel>         # Highlight element
browserfleet state save <path>       # Save auth state
browserfleet state load <path>       # Load auth state
```

### Navigation

```bash
browserfleet back                    # Go back
browserfleet forward                 # Go forward
browserfleet reload                  # Reload page
```

### Setup

```bash
browserfleet install                 # Download Chromium browser
browserfleet install --with-deps     # Also install system deps (Linux)
```

## Sessions

Run multiple isolated browser instances:

```bash
# Different sessions
browserfleet --session agent1 open site-a.com
browserfleet --session agent2 open site-b.com

# Or via environment variable
BROWSERFLEET_SESSION=agent1 browserfleet click "#btn"

# List active sessions
browserfleet session list
# Output:
# Active sessions:
# -> default
#    agent1

# Show current session
browserfleet session
```

Each session has its own:
- Browser instance
- Cookies and storage
- Navigation history
- Authentication state

## Snapshot Options

The `snapshot` command supports filtering to reduce output size:

```bash
browserfleet snapshot                    # Full accessibility tree
browserfleet snapshot -i                 # Interactive elements only (buttons, inputs, links)
browserfleet snapshot -c                 # Compact (remove empty structural elements)
browserfleet snapshot -d 3               # Limit depth to 3 levels
browserfleet snapshot -s "#main"         # Scope to CSS selector
browserfleet snapshot -i -c -d 5         # Combine options
```

| Option | Description |
|--------|-------------|
| `-i, --interactive` | Only show interactive elements (buttons, links, inputs) |
| `-c, --compact` | Remove empty structural elements |
| `-d, --depth <n>` | Limit tree depth |
| `-s, --selector <sel>` | Scope to CSS selector |

## Options

| Option | Description |
|--------|-------------|
| `--session <name>` | Use isolated session (or `BROWSERFLEET_SESSION` env) |
| `--headers <json>` | Set HTTP headers scoped to the URL's origin |
| `--executable-path <path>` | Custom browser executable (or `BROWSERFLEET_EXECUTABLE_PATH` env) |
| `--json` | JSON output (for agents) |
| `--full, -f` | Full page screenshot |
| `--name, -n` | Locator name filter |
| `--exact` | Exact text match |
| `--headed` | Show browser window (not headless) |
| `--cdp <port>` | Connect via Chrome DevTools Protocol |
| `--debug` | Debug output |

## Selectors

### Refs (Recommended for AI)

Refs provide deterministic element selection from snapshots:

```bash
# 1. Get snapshot with refs
browserfleet snapshot
# Output:
# - heading "Example Domain" [ref=e1] [level=1]
# - button "Submit" [ref=e2]
# - textbox "Email" [ref=e3]
# - link "Learn more" [ref=e4]

# 2. Use refs to interact
browserfleet click @e2                   # Click the button
browserfleet fill @e3 "test@example.com" # Fill the textbox
browserfleet get text @e1                # Get heading text
browserfleet hover @e4                   # Hover the link
```

**Why use refs?**
- **Deterministic**: Ref points to exact element from snapshot
- **Fast**: No DOM re-query needed
- **AI-friendly**: Snapshot + ref workflow is optimal for LLMs

### CSS Selectors

```bash
browserfleet click "#id"
browserfleet click ".class"
browserfleet click "div > button"
```

### Text & XPath

```bash
browserfleet click "text=Submit"
browserfleet click "xpath=//button"
```

### Semantic Locators

```bash
browserfleet find role button click --name "Submit"
browserfleet find label "Email" fill "test@test.com"
```

## Agent Mode

Use `--json` for machine-readable output:

```bash
browserfleet snapshot --json
# Returns: {"success":true,"data":{"snapshot":"...","refs":{"e1":{"role":"heading","name":"Title"},...}}}

browserfleet get text @e1 --json
browserfleet is visible @e2 --json
```

### Optimal AI Workflow

```bash
# 1. Navigate and get snapshot
browserfleet open example.com
browserfleet snapshot -i --json   # AI parses tree and refs

# 2. AI identifies target refs from snapshot
# 3. Execute actions using refs
browserfleet click @e2
browserfleet fill @e3 "input text"

# 4. Get new snapshot if page changed
browserfleet snapshot -i --json
```

## Headed Mode

Show the browser window for debugging:

```bash
browserfleet open example.com --headed
```

This opens a visible browser window instead of running headless.

## Authenticated Sessions

Use `--headers` to set HTTP headers for a specific origin, enabling authentication without login flows:

```bash
# Headers are scoped to api.example.com only
browserfleet open api.example.com --headers '{"Authorization": "Bearer <token>"}'

# Requests to api.example.com include the auth header
browserfleet snapshot -i --json
browserfleet click @e2

# Navigate to another domain - headers are NOT sent (safe!)
browserfleet open other-site.com
```

This is useful for:
- **Skipping login flows** - Authenticate via headers instead of UI
- **Switching users** - Start new sessions with different auth tokens
- **API testing** - Access protected endpoints directly
- **Security** - Headers are scoped to the origin, not leaked to other domains

To set headers for multiple origins, use `--headers` with each `open` command:

```bash
browserfleet open api.example.com --headers '{"Authorization": "Bearer token1"}'
browserfleet open api.acme.com --headers '{"Authorization": "Bearer token2"}'
```

For global headers (all domains), use `set headers`:

```bash
browserfleet set headers '{"X-Custom-Header": "value"}'
```

## Custom Browser Executable

Use a custom browser executable instead of the bundled Chromium. This is useful for:
- **Serverless deployment**: Use lightweight Chromium builds like `@sparticuz/chromium` (~50MB vs ~684MB)
- **System browsers**: Use an existing Chrome/Chromium installation
- **Custom builds**: Use modified browser builds

### CLI Usage

```bash
# Via flag
browserfleet --executable-path /path/to/chromium open example.com

# Via environment variable
BROWSERFLEET_EXECUTABLE_PATH=/path/to/chromium browserfleet open example.com
```

### Serverless Example (Vercel/AWS Lambda)

```typescript
import chromium from '@sparticuz/chromium';
import { BrowserManager } from 'browserfleet';

export async function handler() {
  const browser = new BrowserManager();
  await browser.launch({
    executablePath: await chromium.executablePath(),
    headless: true,
  });
  // ... use browser
}
```

## CDP Mode

Connect to an existing browser via Chrome DevTools Protocol:

```bash
# Connect to Electron app
browserfleet --cdp 9222 snapshot

# Connect to Chrome with remote debugging
# (Start Chrome with: google-chrome --remote-debugging-port=9222)
browserfleet --cdp 9222 open about:blank
```

This enables control of:
- Electron apps
- Chrome/Chromium instances with remote debugging
- WebView2 applications
- Any browser exposing a CDP endpoint

## Streaming (Browser Preview)

Stream the browser viewport via WebSocket for live preview or "pair browsing" where a human can watch and interact alongside an AI agent.

### Enable Streaming

Set the `BROWSERFLEET_STREAM_PORT` environment variable:

```bash
BROWSERFLEET_STREAM_PORT=9223 browserfleet open example.com
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

### Programmatic API

For advanced use, control streaming directly via the protocol:

```typescript
import { BrowserManager } from 'browserfleet';

const browser = new BrowserManager();
await browser.launch({ headless: true });
await browser.navigate('https://example.com');

// Start screencast
await browser.startScreencast((frame) => {
  // frame.data is base64-encoded image
  // frame.metadata contains viewport info
  console.log('Frame received:', frame.metadata.deviceWidth, 'x', frame.metadata.deviceHeight);
}, {
  format: 'jpeg',
  quality: 80,
  maxWidth: 1280,
  maxHeight: 720,
});

// Inject mouse events
await browser.injectMouseEvent({
  type: 'mousePressed',
  x: 100,
  y: 200,
  button: 'left',
});

// Inject keyboard events
await browser.injectKeyboardEvent({
  type: 'keyDown',
  key: 'Enter',
  code: 'Enter',
});

// Stop when done
await browser.stopScreencast();
```

## Architecture

browserfleet uses a client-daemon architecture:

1. **Rust CLI** (fast native binary) - Parses commands, communicates with daemon
2. **Node.js Daemon** - Manages Playwright browser instance
3. **Fallback** - If native binary unavailable, uses Node.js directly

The daemon starts automatically on first command and persists between commands for fast subsequent operations.

**Browser Engine:** Uses Chromium by default. The daemon also supports Firefox and WebKit via the Playwright protocol.

## Platforms

| Platform | Binary | Fallback |
|----------|--------|----------|
| macOS ARM64 | Native Rust | Node.js |
| macOS x64 | Native Rust | Node.js |
| Linux ARM64 | Native Rust | Node.js |
| Linux x64 | Native Rust | Node.js |
| Windows x64 | Native Rust | Node.js |

## Usage with AI Agents

### Just ask the agent

The simplest approach - just tell your agent to use it:

```
Use browserfleet to test the login flow. Run browserfleet --help to see available commands.
```

The `--help` output is comprehensive and most agents can figure it out from there.

### AGENTS.md / CLAUDE.md

For more consistent results, add to your project or global instructions file:

```markdown
## Browser Automation

Use `browserfleet` for web automation. Run `browserfleet --help` for all commands.

Core workflow:
1. `browserfleet open <url>` - Navigate to page
2. `browserfleet snapshot -i` - Get interactive elements with refs (@e1, @e2)
3. `browserfleet click @e1` / `fill @e2 "text"` - Interact using refs
4. Re-snapshot after page changes
```

### Claude Code Skill

For Claude Code, a [skill](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices) provides richer context:

```bash
cp -r node_modules/browserfleet/skills/browserfleet .claude/skills/
```

Or download:

```bash
mkdir -p .claude/skills/browserfleet
curl -o .claude/skills/browserfleet/SKILL.md \
  https://raw.githubusercontent.com/nmwcode/browserfleet/main/skills/browserfleet/SKILL.md
```

## License

Apache-2.0
