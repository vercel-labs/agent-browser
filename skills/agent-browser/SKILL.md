---
name: agent-browser
description: Automates browser interactions for web testing, form filling, screenshots, and data extraction. Use when the user needs to navigate websites, interact with web pages, fill forms, take screenshots, test web applications, or extract information from web pages.
allowed-tools: Bash(npx agent-browser:*)
---

# Browser Automation with agent-browser

## Quick start

```bash
npx agent-browser open <url>        # Navigate to page
npx agent-browser snapshot -i       # Get interactive elements with refs
npx agent-browser click @e1         # Click element by ref
npx agent-browser fill @e2 "text"   # Fill input by ref
npx agent-browser close             # Close browser
```

## Core workflow

1. Navigate: `npx agent-browser open <url>`
2. Snapshot: `npx agent-browser snapshot -i` (returns elements with refs like `@e1`, `@e2`)
3. Interact using refs from the snapshot
4. Re-snapshot after navigation or significant DOM changes

## Commands

### Navigation

```bash
npx agent-browser open <url>      # Navigate to URL (aliases: goto, navigate)
                              # Supports: https://, http://, file://, about:, data://
                              # Auto-prepends https:// if no protocol given
npx agent-browser back            # Go back
npx agent-browser forward         # Go forward
npx agent-browser reload          # Reload page
npx agent-browser close           # Close browser (aliases: quit, exit)
npx agent-browser connect 9222    # Connect to browser via CDP port
```

### Snapshot (page analysis)

```bash
npx agent-browser snapshot            # Full accessibility tree
npx agent-browser snapshot -i         # Interactive elements only (recommended)
npx agent-browser snapshot -c         # Compact output
npx agent-browser snapshot -d 3       # Limit depth to 3
npx agent-browser snapshot -s "#main" # Scope to CSS selector
```

### Interactions (use @refs from snapshot)

```bash
npx agent-browser click @e1           # Click
npx agent-browser dblclick @e1        # Double-click
npx agent-browser focus @e1           # Focus element
npx agent-browser fill @e2 "text"     # Clear and type
npx agent-browser type @e2 "text"     # Type without clearing
npx agent-browser press Enter         # Press key (alias: key)
npx agent-browser press Control+a     # Key combination
npx agent-browser keydown Shift       # Hold key down
npx agent-browser keyup Shift         # Release key
npx agent-browser hover @e1           # Hover
npx agent-browser check @e1           # Check checkbox
npx agent-browser uncheck @e1         # Uncheck checkbox
npx agent-browser select @e1 "value"  # Select dropdown option
npx agent-browser select @e1 "a" "b"  # Select multiple options
npx agent-browser scroll down 500     # Scroll page (default: down 300px)
npx agent-browser scrollintoview @e1  # Scroll element into view (alias: scrollinto)
npx agent-browser drag @e1 @e2        # Drag and drop
npx agent-browser upload @e1 file.pdf # Upload files
```

### Get information

```bash
npx agent-browser get text @e1        # Get element text
npx agent-browser get html @e1        # Get innerHTML
npx agent-browser get value @e1       # Get input value
npx agent-browser get attr @e1 href   # Get attribute
npx agent-browser get title           # Get page title
npx agent-browser get url             # Get current URL
npx agent-browser get count ".item"   # Count matching elements
npx agent-browser get box @e1         # Get bounding box
npx agent-browser get styles @e1      # Get computed styles (font, color, bg, etc.)
```

### Check state

```bash
npx agent-browser is visible @e1      # Check if visible
npx agent-browser is enabled @e1      # Check if enabled
npx agent-browser is checked @e1      # Check if checked
```

### Screenshots & PDF

```bash
npx agent-browser screenshot          # Save to a temporary directory
npx agent-browser screenshot path.png # Save to a specific path
npx agent-browser screenshot --full   # Full page
npx agent-browser pdf output.pdf      # Save as PDF
```

### Video recording

```bash
npx agent-browser record start ./demo.webm    # Start recording (uses current URL + state)
npx agent-browser click @e1                   # Perform actions
npx agent-browser record stop                 # Stop and save video
npx agent-browser record restart ./take2.webm # Stop current + start new recording
```

Recording creates a fresh context but preserves cookies/storage from your session. If no URL is provided, it
automatically returns to your current page. For smooth demos, explore first, then start recording.

### Wait

```bash
npx agent-browser wait @e1                     # Wait for element
npx agent-browser wait 2000                    # Wait milliseconds
npx agent-browser wait --text "Success"        # Wait for text (or -t)
npx agent-browser wait --url "**/dashboard"    # Wait for URL pattern (or -u)
npx agent-browser wait --load networkidle      # Wait for network idle (or -l)
npx agent-browser wait --fn "window.ready"     # Wait for JS condition (or -f)
```

### Mouse control

```bash
npx agent-browser mouse move 100 200      # Move mouse
npx agent-browser mouse down left         # Press button
npx agent-browser mouse up left           # Release button
npx agent-browser mouse wheel 100         # Scroll wheel
```

### Semantic locators (alternative to refs)

```bash
npx agent-browser find role button click --name "Submit"
npx agent-browser find text "Sign In" click
npx agent-browser find text "Sign In" click --exact      # Exact match only
npx agent-browser find label "Email" fill "user@test.com"
npx agent-browser find placeholder "Search" type "query"
npx agent-browser find alt "Logo" click
npx agent-browser find title "Close" click
npx agent-browser find testid "submit-btn" click
npx agent-browser find first ".item" click
npx agent-browser find last ".item" click
npx agent-browser find nth 2 "a" hover
```

### Browser settings

```bash
npx agent-browser set viewport 1920 1080          # Set viewport size
npx agent-browser set device "iPhone 14"          # Emulate device
npx agent-browser set geo 37.7749 -122.4194       # Set geolocation (alias: geolocation)
npx agent-browser set offline on                  # Toggle offline mode
npx agent-browser set headers '{"X-Key":"v"}'     # Extra HTTP headers
npx agent-browser set credentials user pass       # HTTP basic auth (alias: auth)
npx agent-browser set media dark                  # Emulate color scheme
npx agent-browser set media light reduced-motion  # Light mode + reduced motion
```

### Cookies & Storage

```bash
npx agent-browser cookies                     # Get all cookies
npx agent-browser cookies set name value      # Set cookie
npx agent-browser cookies clear               # Clear cookies
npx agent-browser storage local               # Get all localStorage
npx agent-browser storage local key           # Get specific key
npx agent-browser storage local set k v       # Set value
npx agent-browser storage local clear         # Clear all
```

### Network

```bash
npx agent-browser network route <url>              # Intercept requests
npx agent-browser network route <url> --abort      # Block requests
npx agent-browser network route <url> --body '{}'  # Mock response
npx agent-browser network unroute [url]            # Remove routes
npx agent-browser network requests                 # View tracked requests
npx agent-browser network requests --filter api    # Filter requests
```

### Tabs & Windows

```bash
npx agent-browser tab                 # List tabs
npx agent-browser tab new [url]       # New tab
npx agent-browser tab 2               # Switch to tab by index
npx agent-browser tab close           # Close current tab
npx agent-browser tab close 2         # Close tab by index
npx agent-browser window new          # New window
```

### Frames

```bash
npx agent-browser frame "#iframe"     # Switch to iframe
npx agent-browser frame main          # Back to main frame
```

### Dialogs

```bash
npx agent-browser dialog accept [text]  # Accept dialog
npx agent-browser dialog dismiss        # Dismiss dialog
```

### JavaScript

```bash
npx agent-browser eval "document.title"   # Run JavaScript
```

## Global options

```bash
npx agent-browser --session <name> ...    # Isolated browser session
npx agent-browser --json ...              # JSON output for parsing
npx agent-browser --headed ...            # Show browser window (not headless)
npx agent-browser --full ...              # Full page screenshot (-f)
npx agent-browser --cdp <port> ...        # Connect via Chrome DevTools Protocol
npx agent-browser -p <provider> ...       # Cloud browser provider (--provider)
npx agent-browser --proxy <url> ...       # Use proxy server
npx agent-browser --headers <json> ...    # HTTP headers scoped to URL's origin
npx agent-browser --executable-path <p>   # Custom browser executable
npx agent-browser --extension <path> ...  # Load browser extension (repeatable)
npx agent-browser --help                  # Show help (-h)
npx agent-browser --version               # Show version (-V)
npx agent-browser <command> --help        # Show detailed help for a command
```

### Proxy support

```bash
npx agent-browser --proxy http://proxy.com:8080 open example.com
npx agent-browser --proxy http://user:pass@proxy.com:8080 open example.com
npx agent-browser --proxy socks5://proxy.com:1080 open example.com
```

## Environment variables

```bash
AGENT_BROWSER_SESSION="mysession"            # Default session name
AGENT_BROWSER_EXECUTABLE_PATH="/path/chrome" # Custom browser path
AGENT_BROWSER_EXTENSIONS="/ext1,/ext2"       # Comma-separated extension paths
AGENT_BROWSER_PROVIDER="your-cloud-browser-provider"  # Cloud browser provider (select browseruse or browserbase)
AGENT_BROWSER_STREAM_PORT="9223"             # WebSocket streaming port
AGENT_BROWSER_HOME="/path/to/agent-browser"  # Custom install location (for daemon.js)
```

## Example: Form submission

```bash
npx agent-browser open https://example.com/form
npx agent-browser snapshot -i
# Output shows: textbox "Email" [ref=e1], textbox "Password" [ref=e2], button "Submit" [ref=e3]

npx agent-browser fill @e1 "user@example.com"
npx agent-browser fill @e2 "password123"
npx agent-browser click @e3
npx agent-browser wait --load networkidle
npx agent-browser snapshot -i  # Check result
```

## Example: Authentication with saved state

```bash
# Login once
npx agent-browser open https://app.example.com/login
npx agent-browser snapshot -i
npx agent-browser fill @e1 "username"
npx agent-browser fill @e2 "password"
npx agent-browser click @e3
npx agent-browser wait --url "**/dashboard"
npx agent-browser state save auth.json

# Later sessions: load saved state
npx agent-browser state load auth.json
npx agent-browser open https://app.example.com/dashboard
```

## Sessions (parallel browsers)

```bash
npx agent-browser --session test1 open site-a.com
npx agent-browser --session test2 open site-b.com
npx agent-browser session list
```

## JSON output (for parsing)

Add `--json` for machine-readable output:

```bash
npx agent-browser snapshot -i --json
npx agent-browser get text @e1 --json
```

## Debugging

```bash
npx agent-browser --headed open example.com   # Show browser window
npx agent-browser --cdp 9222 snapshot         # Connect via CDP port
npx agent-browser connect 9222                # Alternative: connect command
npx agent-browser console                     # View console messages
npx agent-browser console --clear             # Clear console
npx agent-browser errors                      # View page errors
npx agent-browser errors --clear              # Clear errors
npx agent-browser highlight @e1               # Highlight element
npx agent-browser trace start                 # Start recording trace
npx agent-browser trace stop trace.zip        # Stop and save trace
npx agent-browser record start ./debug.webm   # Record video from current page
npx agent-browser record stop                 # Save recording
```

## Deep-dive documentation

For detailed patterns and best practices, see:

| Reference | Description |
|-----------|-------------|
| [references/snapshot-refs.md](references/snapshot-refs.md) | Ref lifecycle, invalidation rules, troubleshooting |
| [references/session-management.md](references/session-management.md) | Parallel sessions, state persistence, concurrent scraping |
| [references/authentication.md](references/authentication.md) | Login flows, OAuth, 2FA handling, state reuse |
| [references/video-recording.md](references/video-recording.md) | Recording workflows for debugging and documentation |
| [references/proxy-support.md](references/proxy-support.md) | Proxy configuration, geo-testing, rotating proxies |

## Ready-to-use templates

Executable workflow scripts for common patterns:

| Template | Description |
|----------|-------------|
| [templates/form-automation.sh](templates/form-automation.sh) | Form filling with validation |
| [templates/authenticated-session.sh](templates/authenticated-session.sh) | Login once, reuse state |
| [templates/capture-workflow.sh](templates/capture-workflow.sh) | Content extraction with screenshots |

Usage:
```bash
./templates/form-automation.sh https://example.com/form
./templates/authenticated-session.sh https://app.example.com/login
./templates/capture-workflow.sh https://example.com ./output
```

## HTTPS Certificate Errors

For sites with self-signed or invalid certificates:
```bash
npx agent-browser open https://localhost:8443 --ignore-https-errors
```
