# agent-browser

Headless browser automation CLI for AI agents. Fast Rust CLI with Node.js fallback.

## Installation

### npm (recommended)

```bash
npm install -g agent-browser
agent-browser install  # Download Chromium
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
# or manually: npx playwright install-deps chromium
```

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
agent-browser click <sel>             # Click element
agent-browser dblclick <sel>          # Double-click element
agent-browser focus <sel>             # Focus element
agent-browser type <sel> <text>       # Type into element
agent-browser fill <sel> <text>       # Clear and fill
agent-browser press <key>             # Press key (Enter, Tab, Control+a) (alias: key)
agent-browser keydown <key>           # Hold key down
agent-browser keyup <key>             # Release key
agent-browser hover <sel>             # Hover element
agent-browser select <sel> <val>      # Select dropdown option
agent-browser check <sel>             # Check checkbox
agent-browser uncheck <sel>           # Uncheck checkbox
agent-browser scroll <dir> [px]       # Scroll (up/down/left/right)
agent-browser scrollintoview <sel>    # Scroll element into view (alias: scrollinto)
agent-browser drag <src> <tgt>        # Drag and drop
agent-browser upload <sel> <files>    # Upload files
agent-browser screenshot [path]       # Take screenshot (--full for full page)
agent-browser pdf <path>              # Save as PDF
agent-browser snapshot                # Accessibility tree with refs (best for AI)
agent-browser eval <js>               # Run JavaScript
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
agent-browser get count <sel>         # Count matching elements
agent-browser get box <sel>           # Get bounding box
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

**Actions:** `click`, `fill`, `check`, `hover`, `text`

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
agent-browser wait --text "Welcome"   # Wait for text to appear
agent-browser wait --url "**/dash"    # Wait for URL pattern
agent-browser wait --load networkidle # Wait for load state
agent-browser wait --fn "window.ready === true"  # Wait for JS condition
```

**Load states:** `load`, `domcontentloaded`, `networkidle`

### Mouse Control

```bash
agent-browser mouse move <x> <y>      # Move mouse
agent-browser mouse down [button]     # Press button (left/right/middle)
agent-browser mouse up [button]       # Release button
agent-browser mouse wheel <dy> [dx]   # Scroll wheel
```

### Browser Settings

```bash
agent-browser set viewport <w> <h>    # Set viewport size
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

### Debug

```bash
agent-browser trace start [path]      # Start recording trace
agent-browser trace stop [path]       # Stop and save trace
agent-browser console                 # View console messages
agent-browser console --clear         # Clear console
agent-browser errors                  # View page errors
agent-browser errors --clear          # Clear errors
agent-browser highlight <sel>         # Highlight element
```

### State Management

```bash
agent-browser state save <path>       # Save auth state to file
agent-browser state load <path>       # Load auth state from file
agent-browser state list              # List all saved state files
agent-browser state show <file>       # Show details of a state file
agent-browser state clear [name]      # Clear state files for session name
agent-browser state clear --all       # Clear all saved state files
agent-browser state clean --older-than <days>  # Delete old state files
agent-browser state rename <old> <new>  # Rename a state file
```

#### State Encryption

Protect sensitive session data with AES-256-GCM encryption:

```bash
# Generate a secure 256-bit key
openssl rand -hex 32
# Output: a1b2c3d4e5f6...  (64 hex characters)

# Set the encryption key (add to your shell profile for persistence)
export AGENT_BROWSER_ENCRYPTION_KEY="a1b2c3d4e5f6..."

# Now all saved states are automatically encrypted
agent-browser --session-name twitter open twitter.com --headed
# After login, close browser
agent-browser close

# State file is encrypted - safe to backup/sync
agent-browser state list
# twitter-default.json (1.2KB, 2026-01-14) [encrypted]
```

#### Auto-Expiration

State files are automatically deleted after 30 days by default:

```bash
# Change expiration (in days) - set to 0 to disable
export AGENT_BROWSER_STATE_EXPIRE_DAYS=7

# Or manually clean old files
agent-browser state clean --older-than 30
```

### Navigation

```bash
agent-browser back                    # Go back
agent-browser forward                 # Go forward
agent-browser reload                  # Reload page
```

### Setup

```bash
agent-browser install                 # Download Chromium browser
agent-browser install --with-deps     # Also install system deps (Linux)
```

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

## Automatic Session Persistence

Stay logged in across browser restarts with zero configuration:

```bash
# Set session name - that's it!
agent-browser --session-name twitter open twitter.com --headed
# ... login to Twitter ...
agent-browser close  # Auto-saves authentication

# Next time: already logged in!
agent-browser --session-name twitter open twitter.com
agent-browser snapshot  # See authenticated content
```

Or use environment variable:

```bash
export AGENT_BROWSER_SESSION_NAME=twitter
agent-browser open twitter.com  # Auto-loads saved auth
```

### How It Works

When `AGENT_BROWSER_SESSION_NAME` is set (via `--session-name` flag or env var), agent-browser automatically:
1. **On Launch**: Checks for saved state in `~/.agent-browser/sessions/`
2. **If Found**: Loads cookies and storage (you stay logged in)
3. **On Close**: Saves current state for next time

**Without `--session-name`**: Browser runs in ephemeral mode - no state is saved or loaded. Each session starts fresh.

### Session Name Rules

Session names must contain only alphanumeric characters, hyphens, and underscores. This prevents path traversal attacks and ensures safe file naming:

```bash
# Valid session names
agent-browser --session-name my-project open example.com
agent-browser --session-name test_session_v2 open example.com
agent-browser --session-name MyApp123 open example.com

# Invalid (rejected with error)
agent-browser --session-name "../bad" open example.com     # path traversal
agent-browser --session-name "my session" open example.com # spaces not allowed
agent-browser --session-name "foo/bar" open example.com    # slashes not allowed
```

### Multiple Accounts

Use different session names for different accounts:

```bash
# Personal Twitter
agent-browser --session-name twitter-personal open twitter.com

# Work Twitter  
agent-browser --session-name twitter-work open twitter.com
```

### Switching Between Accounts

Switch accounts by changing the session name:

```bash
# Login to account 1
agent-browser --session-name twitter-account1 open twitter.com --headed
# ... login ...
agent-browser close

# Login to account 2
agent-browser --session-name twitter-account2 open twitter.com --headed
# ... login ...
agent-browser close

# Switch between them anytime
agent-browser --session-name twitter-account1 open twitter.com  # Account 1
agent-browser close
agent-browser --session-name twitter-account2 open twitter.com  # Account 2
```

### Combining with --session

Combine `--session-name` with `--session` for complete isolation:

```bash
# Agent 1: Personal Gmail
agent-browser --session-name gmail --session agent1 open gmail.com

# Agent 2: Work Gmail
agent-browser --session-name gmail-work --session agent2 open gmail.com
```

Each combination gets its own state file:
- `~/.agent-browser/sessions/gmail-agent1.json`
- `~/.agent-browser/sessions/gmail-work-agent2.json`

### Logging Out / Clearing Sessions

Several ways to logout or start fresh:

```bash
# Method 1: Use state management commands
agent-browser state list                          # See all saved states
agent-browser state show twitter-default.json     # View state details
agent-browser state clear twitter                 # Clear all twitter-* states
agent-browser state clear --all                   # Clear ALL saved states
agent-browser state clean --older-than 30         # Delete states older than 30 days
agent-browser state rename old-name new-name      # Rename a state file

# Method 2: Delete the state file manually
rm ~/.agent-browser/sessions/twitter-default.json

# Method 3: Clear cookies via browser
agent-browser --session-name twitter open twitter.com
agent-browser cookies clear
agent-browser close  # Saves state without cookies

# Method 4: Use a new session name (fresh state)
agent-browser --session-name twitter-fresh open twitter.com

# Method 5: Run without --session-name (ephemeral, nothing saved)
agent-browser open twitter.com  # Completely fresh, no persistence
```

### Starting Fresh (No Persistence)

To run without any session persistence, simply omit `--session-name`:

```bash
# Ephemeral mode - no state saved or loaded
agent-browser open twitter.com
agent-browser close  # Nothing saved
agent-browser open twitter.com  # Fresh browser again
```

### Security Notes

- State files contain cookies and may include authentication tokens
- Location: `~/.agent-browser/sessions/` (auto-created with 0600 permissions)
- **Never commit state files to version control**

## Snapshot Options

The `snapshot` command supports filtering to reduce output size:

```bash
agent-browser snapshot                    # Full accessibility tree
agent-browser snapshot -i                 # Interactive elements only (buttons, inputs, links)
agent-browser snapshot -c                 # Compact (remove empty structural elements)
agent-browser snapshot -d 3               # Limit depth to 3 levels
agent-browser snapshot -s "#main"         # Scope to CSS selector
agent-browser snapshot -i -c -d 5         # Combine options
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
| `--session <name>` | Use isolated session (or `AGENT_BROWSER_SESSION` env) |
| `--session-name <name>` | Auto-save/load auth state (or `AGENT_BROWSER_SESSION_NAME` env) |
| `--headers <json>` | Set HTTP headers scoped to the URL's origin |
| `--executable-path <path>` | Custom browser executable (or `AGENT_BROWSER_EXECUTABLE_PATH` env) |
| `--json` | JSON output (for agents) |
| `--full, -f` | Full page screenshot |
| `--name, -n` | Locator name filter |
| `--exact` | Exact text match |
| `--headed` | Show browser window (not headless) |
| `--cdp <port>` | Connect via Chrome DevTools Protocol |
| `--debug` | Debug output |

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGENT_BROWSER_SESSION` | Session name for browser isolation | `default` |
| `AGENT_BROWSER_SESSION_NAME` | Session name for state persistence | - |
| `AGENT_BROWSER_EXECUTABLE_PATH` | Custom browser binary path | - |
| `AGENT_BROWSER_ENCRYPTION_KEY` | 64-char hex key for AES-256-GCM encryption | - |
| `AGENT_BROWSER_STATE_EXPIRE_DAYS` | Auto-delete states older than N days | `30` |
| `AGENT_BROWSER_DEBUG` | Enable debug output (`1` to enable) | - |

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

## Headed Mode

Show the browser window for debugging:

```bash
agent-browser open example.com --headed
```

This opens a visible browser window instead of running headless.

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

### Serverless Example (Vercel/AWS Lambda)

```typescript
import chromium from '@sparticuz/chromium';
import { BrowserManager } from 'agent-browser';

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
agent-browser --cdp 9222 snapshot

# Connect to Chrome with remote debugging
# (Start Chrome with: google-chrome --remote-debugging-port=9222)
agent-browser --cdp 9222 open about:blank
```

This enables control of:
- Electron apps
- Chrome/Chromium instances with remote debugging
- WebView2 applications
- Any browser exposing a CDP endpoint

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

### Programmatic API

For advanced use, control streaming directly via the protocol:

```typescript
import { BrowserManager } from 'agent-browser';

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

agent-browser uses a client-daemon architecture:

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
Use agent-browser to test the login flow. Run agent-browser --help to see available commands.
```

The `--help` output is comprehensive and most agents can figure it out from there.

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

Session persistence (stay logged in):
- Use `--session-name <name>` to auto-save/load authentication
- State files: `~/.agent-browser/sessions/{name}-default.json`
- Without `--session-name`: ephemeral mode (fresh browser each time)
```

### Session Persistence for Agents

AI agents can maintain authenticated sessions across runs:

```bash
# First run: Login and save
agent-browser --session-name twitter open twitter.com --headed
# ... agent performs login ...
agent-browser close  # Auto-saves to ~/.agent-browser/sessions/twitter-default.json

# Subsequent runs: Already authenticated
agent-browser --session-name twitter open twitter.com
agent-browser snapshot -i  # See authenticated content
```

**For multi-account agents:**

```bash
# Agent managing multiple accounts
agent-browser --session-name twitter-bot1 open twitter.com  # Account 1
agent-browser --session-name twitter-bot2 open twitter.com  # Account 2
```

**Ephemeral mode (no persistence):**

```bash
# Run without --session-name for fresh browser each time
agent-browser open twitter.com
```

### Claude Code Skill

For Claude Code, a [skill](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices) provides richer context:

```bash
cp -r node_modules/agent-browser/skills/agent-browser .claude/skills/
```

Or download:

```bash
mkdir -p .claude/skills/agent-browser
curl -o .claude/skills/agent-browser/SKILL.md \
  https://raw.githubusercontent.com/vercel-labs/agent-browser/main/skills/agent-browser/SKILL.md
```

## License

Apache-2.0
