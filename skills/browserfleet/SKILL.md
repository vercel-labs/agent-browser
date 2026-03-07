---
name: browserfleet
description: Automates browser interactions for web testing, form filling, screenshots, and data extraction. Use when the user needs to navigate websites, interact with web pages, fill forms, take screenshots, test web applications, or extract information from web pages.
---

# Browser Automation with browserfleet

## Quick start

```bash
browserfleet open <url>        # Navigate to page
browserfleet snapshot -i       # Get interactive elements with refs
browserfleet click @e1         # Click element by ref
browserfleet fill @e2 "text"   # Fill input by ref
browserfleet close             # Close browser
```

## Core workflow

1. Navigate: `browserfleet open <url>`
2. Snapshot: `browserfleet snapshot -i` (returns elements with refs like `@e1`, `@e2`)
3. Interact using refs from the snapshot
4. Re-snapshot after navigation or significant DOM changes

## Commands

### Navigation
```bash
browserfleet open <url>      # Navigate to URL
browserfleet back            # Go back
browserfleet forward         # Go forward  
browserfleet reload          # Reload page
browserfleet close           # Close browser
```

### Snapshot (page analysis)
```bash
browserfleet snapshot        # Full accessibility tree
browserfleet snapshot -i     # Interactive elements only (recommended)
browserfleet snapshot -c     # Compact output
browserfleet snapshot -d 3   # Limit depth to 3
```

### Interactions (use @refs from snapshot)
```bash
browserfleet click @e1           # Click
browserfleet dblclick @e1        # Double-click
browserfleet fill @e2 "text"     # Clear and type
browserfleet type @e2 "text"     # Type without clearing
browserfleet press Enter         # Press key
browserfleet press Control+a     # Key combination
browserfleet hover @e1           # Hover
browserfleet check @e1           # Check checkbox
browserfleet uncheck @e1         # Uncheck checkbox
browserfleet select @e1 "value"  # Select dropdown
browserfleet scroll down 500     # Scroll page
browserfleet scrollintoview @e1  # Scroll element into view
```

### Get information
```bash
browserfleet get text @e1        # Get element text
browserfleet get value @e1       # Get input value
browserfleet get title           # Get page title
browserfleet get url             # Get current URL
```

### Screenshots
```bash
browserfleet screenshot          # Screenshot to stdout
browserfleet screenshot path.png # Save to file
browserfleet screenshot --full   # Full page
```

### Wait
```bash
browserfleet wait @e1                     # Wait for element
browserfleet wait 2000                    # Wait milliseconds
browserfleet wait --text "Success"        # Wait for text
browserfleet wait --load networkidle      # Wait for network idle
```

### Semantic locators (alternative to refs)
```bash
browserfleet find role button click --name "Submit"
browserfleet find text "Sign In" click
browserfleet find label "Email" fill "user@test.com"
```

## Example: Form submission

```bash
browserfleet open https://example.com/form
browserfleet snapshot -i
# Output shows: textbox "Email" [ref=e1], textbox "Password" [ref=e2], button "Submit" [ref=e3]

browserfleet fill @e1 "user@example.com"
browserfleet fill @e2 "password123"
browserfleet click @e3
browserfleet wait --load networkidle
browserfleet snapshot -i  # Check result
```

## Example: Authentication with saved state

```bash
# Login once
browserfleet open https://app.example.com/login
browserfleet snapshot -i
browserfleet fill @e1 "username"
browserfleet fill @e2 "password"
browserfleet click @e3
browserfleet wait --url "**/dashboard"
browserfleet state save auth.json

# Later sessions: load saved state
browserfleet state load auth.json
browserfleet open https://app.example.com/dashboard
```

## Sessions (parallel browsers)

```bash
browserfleet --session test1 open site-a.com
browserfleet --session test2 open site-b.com
browserfleet session list
```

## JSON output (for parsing)

Add `--json` for machine-readable output:
```bash
browserfleet snapshot -i --json
browserfleet get text @e1 --json
```

## Debugging

```bash
browserfleet open example.com --headed  # Show browser window
browserfleet console                    # View console messages
browserfleet errors                     # View page errors
```
