---
name: core
description: Core agent-browser usage guide. Read this before running any agent-browser commands. Covers the snapshot-and-ref workflow, navigating pages, interacting with elements (click, fill, type, select), extracting text and data, taking screenshots, managing tabs, handling forms and auth, waiting for content, running multiple browser sessions in parallel, and troubleshooting common failures. Use when the user asks to interact with a website, fill a form, click something, extract data, take a screenshot, log into a site, test a web app, or automate any browser task.
allowed-tools: Bash(agent-browser:*), Bash(npx agent-browser:*)
---

# agent-browser core

Fast browser automation CLI for AI agents. Chrome/Chromium via CDP, no
Playwright or Puppeteer dependency. Accessibility-tree snapshots with compact
`@eN` refs let agents interact with pages in ~200-400 tokens instead of
parsing raw HTML.

Most normal web tasks (navigate, read, click, fill, extract, screenshot) are
covered here. Load a specialized skill when the task falls outside browser
web pages — see [When to load another skill](#when-to-load-another-skill).

## The core loop

```bash
agent-browser open <url>        # 1. Open a page
agent-browser snapshot -i       # 2. See what's on it (interactive elements only)
agent-browser click @e3         # 3. Act on refs from the snapshot
agent-browser snapshot -i       # 4. Re-snapshot after any page change
```

Refs (`@e1`, `@e2`, ...) are assigned fresh on every snapshot. They become
**stale the moment the page changes** — after clicks that navigate, form
submits, dynamic re-renders, dialog opens. Always re-snapshot before your
next ref interaction.

## Quickstart

```bash
# Install once
npm i -g agent-browser && agent-browser install

# Take a screenshot of a page
agent-browser open https://example.com
agent-browser screenshot home.png
agent-browser close

# Search, click a result, and capture it
agent-browser open https://duckduckgo.com
agent-browser snapshot -i                      # find the search box ref
agent-browser fill @e1 "agent-browser cli"
agent-browser press Enter
agent-browser wait --load networkidle
agent-browser snapshot -i                      # refs now reflect results
agent-browser click @e5                        # click a result
agent-browser screenshot result.png
```

The browser stays running across commands so these feel like a single
session. Use `agent-browser close` (or `close --all`) when you're done.

## Reading a page

```bash
agent-browser snapshot                    # full tree (verbose)
agent-browser snapshot -i                 # interactive elements only (preferred)
agent-browser snapshot -i -u              # include href urls on links
agent-browser snapshot -i -c              # compact (no empty structural nodes)
agent-browser snapshot -i -d 3            # cap depth at 3 levels
agent-browser snapshot -s "#main"         # scope to a CSS selector
agent-browser snapshot -i --json          # machine-readable output
```

Snapshot output looks like:

```
Page: Example - Log in
URL: https://example.com/login

@e1 [heading] "Log in"
@e2 [form]
  @e3 [input type="email"] placeholder="Email"
  @e4 [input type="password"] placeholder="Password"
  @e5 [button type="submit"] "Continue"
  @e6 [link] "Forgot password?"
```

For unstructured reading (no refs needed):

```bash
agent-browser get text @e1                # visible text of an element
agent-browser get html @e1                # innerHTML
agent-browser get attr @e1 href           # any attribute
agent-browser get value @e1               # input value
agent-browser get title                   # page title
agent-browser get url                     # current URL
agent-browser get count ".item"           # count matching elements
```

## Interacting

```bash
agent-browser click @e1                   # click
agent-browser click @e1 --new-tab         # open link in new tab instead of navigating
agent-browser dblclick @e1                # double-click
agent-browser hover @e1                   # hover
agent-browser focus @e1                   # focus (useful before keyboard input)
agent-browser fill @e2 "hello"            # clear then type
agent-browser type @e2 " world"           # type without clearing
agent-browser press Enter                 # press a key at current focus
agent-browser press Control+a             # key combination
agent-browser check @e3                   # check checkbox
agent-browser uncheck @e3                 # uncheck
agent-browser select @e4 "option-value"   # select dropdown option
agent-browser select @e4 "a" "b"          # select multiple
agent-browser upload @e5 file1.pdf        # upload file(s)
agent-browser scroll down 500             # scroll page (up/down/left/right)
agent-browser scrollintoview @e1          # scroll element into view
agent-browser drag @e1 @e2                # drag and drop
```

### When refs don't work or you don't want to snapshot

Use semantic locators:

```bash
agent-browser find role button click --name "Submit"
agent-browser find text "Sign In" click
agent-browser find text "Sign In" click --exact     # exact match only
agent-browser find label "Email" fill "user@test.com"
agent-browser find placeholder "Search" type "query"
agent-browser find testid "submit-btn" click
agent-browser find first ".card" click
agent-browser find nth 2 ".card" hover
```

Or a raw CSS selector:

```bash
agent-browser click "#submit"
agent-browser fill "input[name=email]" "user@test.com"
agent-browser click "button.primary"
```

Rule of thumb: snapshot + `@eN` refs are fastest and most reliable for
AI agents. `find role/text/label` is next best and doesn't require a prior
snapshot. Raw CSS is a fallback when the others fail.

## Waiting (read this)

Agents fail more often from bad waits than from bad selectors. Pick the
right wait for the situation:

```bash
agent-browser wait @e1                     # until an element appears
agent-browser wait 2000                    # dumb wait, milliseconds (last resort)
agent-browser wait --text "Success"        # until the text appears on the page
agent-browser wait --url "**/dashboard"    # until URL matches pattern (glob)
agent-browser wait --load networkidle      # until network idle (post-navigation)
agent-browser wait --load domcontentloaded # until DOMContentLoaded
agent-browser wait --fn "window.myApp.ready === true"  # until JS condition
```

After any page-changing action, pick one:

- Wait for a specific element you expect to appear: `wait @ref` or `wait --text "..."`.
- Wait for URL change: `wait --url "**/new-page"`.
- Wait for network idle (catch-all for SPA navigation): `wait --load networkidle`.

Avoid bare `wait 2000` except when debugging — it makes scripts slow and
flaky. Timeouts default to 25 seconds.

## Common workflows

### Log in

```bash
agent-browser open https://app.example.com/login
agent-browser snapshot -i

# Pick the email/password refs out of the snapshot, then:
agent-browser fill @e3 "user@example.com"
agent-browser fill @e4 "hunter2"
agent-browser click @e5
agent-browser wait --url "**/dashboard"
agent-browser snapshot -i
```

Credentials in shell history are a leak. For anything sensitive, use the
auth vault (see [references/authentication.md](references/authentication.md)):

```bash
agent-browser auth save my-app --url https://app.example.com/login \
  --username user@example.com --password-stdin
# (type password, Ctrl+D)

agent-browser auth login my-app    # fills + clicks, waits for form
```

### Persist session across runs

```bash
# Log in once, save cookies + localStorage
agent-browser state save ./auth.json

# Later runs start already-logged-in
agent-browser --state ./auth.json open https://app.example.com
```

Or use `--session-name` for auto-save/restore:

```bash
AGENT_BROWSER_SESSION_NAME=my-app agent-browser open https://app.example.com
# State is auto-saved and restored on subsequent runs with the same name.
```

### Extract data

```bash
# Structured snapshot (best for AI reasoning over page content)
agent-browser snapshot -i --json > page.json

# Targeted extraction with refs
agent-browser snapshot -i
agent-browser get text @e5
agent-browser get attr @e10 href

# Arbitrary shape via JavaScript
cat <<'EOF' | agent-browser eval --stdin
const rows = document.querySelectorAll("table tbody tr");
Array.from(rows).map(r => ({
  name: r.cells[0].innerText,
  price: r.cells[1].innerText,
}));
EOF
```

Prefer `eval --stdin` (heredoc) or `eval -b <base64>` for any JS with
quotes or special characters. Inline `agent-browser eval "..."` works
only for simple expressions.

### Screenshot

```bash
agent-browser screenshot                        # temp path, printed on stdout
agent-browser screenshot page.png               # specific path
agent-browser screenshot --full full.png        # full scroll height
agent-browser screenshot --annotate map.png     # numbered labels + legend keyed to snapshot refs
```

`--annotate` is designed for multimodal models: each label `[N]` maps to ref `@eN`.

### Handle multiple pages via tabs

```bash
agent-browser tab                      # list open tabs (with stable tabId)
agent-browser tab new https://docs...  # open a new tab (and switch to it)
agent-browser tab 2                    # switch to tab 2
agent-browser tab close 2              # close tab 2
```

Stable `tabId`s mean `tab 2` points at the same tab across commands even
when other tabs open or close. After switching, refs from a prior snapshot
on a different tab no longer apply — re-snapshot.

### Run multiple browsers in parallel

Each `--session <name>` is an isolated browser with its own cookies, tabs,
and refs. Useful for testing multi-user flows or parallel scraping:

```bash
agent-browser --session a open https://app.example.com
agent-browser --session b open https://app.example.com
agent-browser --session a fill @e1 "alice@test.com"
agent-browser --session b fill @e1 "bob@test.com"
```

`AGENT_BROWSER_SESSION=myapp` sets the default session for the current
shell.

### Mock network requests

```bash
agent-browser network route "**/api/users" --body '{"users":[]}'   # stub a response
agent-browser network route "**/analytics" --abort                 # block entirely
agent-browser network requests                                     # inspect what fired
agent-browser network har start                                    # record all traffic
# ... perform actions ...
agent-browser network har stop /tmp/trace.har
```

### Record a video of the workflow

```bash
agent-browser record start demo.webm
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser click @e3
agent-browser record stop
```

See [references/video-recording.md](references/video-recording.md) for
codec options, GIF export, and more.

### Iframes

Iframes are auto-inlined in the snapshot — their refs work transparently:

```bash
agent-browser snapshot -i
# @e3 [Iframe] "payment-frame"
#   @e4 [input] "Card number"
#   @e5 [button] "Pay"

agent-browser fill @e4 "4111111111111111"
agent-browser click @e5
```

To scope a snapshot to an iframe (for focus or deep nesting):

```bash
agent-browser frame @e3      # switch context to the iframe
agent-browser snapshot -i
agent-browser frame main     # back to main frame
```

### Dialogs

`alert` and `beforeunload` are auto-accepted so agents never block. For
`confirm` and `prompt`:

```bash
agent-browser dialog status          # is there a pending dialog?
agent-browser dialog accept           # accept
agent-browser dialog accept "text"    # accept with prompt input
agent-browser dialog dismiss          # cancel
```

## Diagnosing install issues

If a command fails unexpectedly (`Unknown command`, `Failed to connect`,
stale daemons, version mismatches after `upgrade`, missing Chrome, etc.)
run `doctor` before anything else:

```bash
agent-browser doctor                     # full diagnosis (env, Chrome, daemons, config, providers, network, launch test)
agent-browser doctor --offline --quick   # fast, local-only
agent-browser doctor --fix               # also run destructive repairs (reinstall Chrome, purge old state, ...)
agent-browser doctor --json              # structured output for programmatic consumption
```

`doctor` auto-cleans stale socket/pid/version sidecar files on every run.
Destructive actions require `--fix`. Exit code is `0` if all checks pass
(warnings OK), `1` if any fail.

## Troubleshooting

**"Ref not found" / "Element not found: @eN"**
Page changed since the snapshot. Run `agent-browser snapshot -i` again,
then use the new refs.

**Element exists in the DOM but not in the snapshot**
It's probably off-screen or not yet rendered. Try:

```bash
agent-browser scroll down 1000
agent-browser snapshot -i
# or
agent-browser wait --text "..."
agent-browser snapshot -i
```

**Click does nothing / overlay swallows the click**
Some modals and cookie banners block other clicks. Snapshot, find the
dismiss/close button, click it, then re-snapshot.

**Fill / type doesn't work**
Some custom input components intercept key events. Try:

```bash
agent-browser focus @e1
agent-browser keyboard inserttext "text"    # bypasses key events
# or
agent-browser keyboard type "text"          # raw keystrokes, no selector
```

**Page needs JS you can't get right in one shot**
Use `eval --stdin` with a heredoc instead of inline:

```bash
cat <<'EOF' | agent-browser eval --stdin
// Complex script with quotes, backticks, whatever
document.querySelectorAll('[data-id]').length
EOF
```

**Cross-origin iframe not accessible**
Cross-origin iframes that block accessibility tree access are silently
skipped. Use `frame "#iframe"` to switch into them explicitly if the
parent opts in, otherwise the iframe's contents aren't available via
snapshot — fall back to `eval` in the iframe's origin or use the
`--headers` flag to satisfy CORS.

**Authentication expires mid-workflow**
Use `--session-name <name>` or `state save`/`state load` so your session
survives browser restarts. See [references/session-management.md](references/session-management.md)
and [references/authentication.md](references/authentication.md).

## Global flags worth knowing

```bash
--session <name>        # isolated browser session
--json                  # JSON output (for machine parsing)
--headed                # show the window (default is headless)
--auto-connect          # connect to an already-running Chrome
--cdp <port>            # connect to a specific CDP port
--profile <name|path>   # use a Chrome profile (login state survives)
--headers <json>        # HTTP headers scoped to the URL's origin
--proxy <url>           # proxy server
--state <path>          # load saved auth state from JSON
--session-name <name>   # auto-save/restore session state by name
```

## When to load another skill

- **Electron desktop app** (VS Code, Slack desktop, Discord, Figma, etc.):
  `agent-browser skills get electron`
- **Slack workspace automation**: `agent-browser skills get slack`
- **Exploratory testing / QA / bug hunts**: `agent-browser skills get dogfood`
- **Vercel Sandbox microVMs**: `agent-browser skills get vercel-sandbox`
- **AWS Bedrock AgentCore cloud browser**: `agent-browser skills get agentcore`

## Full reference

Everything covered here plus the complete command/flag/env listing:

```bash
agent-browser skills get core --full
```

That pulls in:

- `references/commands.md` — every command, flag, alias
- `references/snapshot-refs.md` — deep dive on the snapshot + ref model
- `references/authentication.md` — auth vault, credential handling
- `references/session-management.md` — persistence, multi-session workflows
- `references/profiling.md` — Chrome DevTools tracing and profiling
- `references/video-recording.md` — video capture options
- `references/proxy-support.md` — proxy configuration
- `templates/*` — starter shell scripts for auth, capture, form automation
