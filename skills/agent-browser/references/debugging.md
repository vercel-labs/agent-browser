# Debugging Guide

Tools and techniques for troubleshooting browser automation issues.

## Quick Diagnostics

```bash
# Show browser window (see what's happening)
agent-browser --headed open https://example.com

# View console logs
agent-browser console

# View page errors
agent-browser errors

# Highlight element to verify selection
agent-browser highlight @e1
```

## Debugging Commands

### Console Logs

```bash
# View all console messages
agent-browser console

# Clear console
agent-browser console --clear
```

Console output includes:
- `console.log()` messages
- `console.warn()` warnings
- `console.error()` errors
- Unhandled promise rejections

### Page Errors

```bash
# View JavaScript errors
agent-browser errors

# Clear errors
agent-browser errors --clear
```

### Element Highlighting

```bash
# Visually highlight an element
agent-browser highlight @e1

# Useful for verifying you have the right element
agent-browser snapshot -i
agent-browser highlight @e5  # Is this the button I think it is?
```

### Trace Recording

```bash
# Start recording all browser activity
agent-browser trace start

# Perform actions
agent-browser open https://example.com
agent-browser click @e1
agent-browser fill @e2 "test"

# Stop and save trace
agent-browser trace stop ./trace.zip
```

Trace files can be viewed at [trace.playwright.dev](https://trace.playwright.dev).

### Video Recording

```bash
# Record video of browser actions
agent-browser record start ./debug-session.webm
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser click @e1
agent-browser record stop
```

## Headed Mode

Run with visible browser window:

```bash
# See exactly what the browser sees
agent-browser --headed open https://example.com
agent-browser --headed snapshot -i
agent-browser --headed click @e1
```

Useful for:
- Seeing page state during automation
- Handling CAPTCHAs manually
- Debugging complex interactions
- Understanding timing issues

## CDP Connection

Connect to an existing browser for debugging:

```bash
# Launch Chrome with debugging port
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --remote-debugging-port=9222

# Connect agent-browser
agent-browser --cdp 9222 snapshot -i
agent-browser --cdp 9222 click @e1
```

Or use the connect command:

```bash
agent-browser connect 9222
agent-browser snapshot -i
```

## Common Issues

### Element Not Found

**Symptoms:**
```
Error: Element @e5 not found
```

**Causes & Solutions:**

1. **Refs changed after navigation**
   ```bash
   # Re-snapshot to get fresh refs
   agent-browser snapshot -i
   ```

2. **Element not yet loaded**
   ```bash
   # Wait for element
   agent-browser wait @e5
   agent-browser click @e5

   # Or wait for network
   agent-browser wait --load networkidle
   agent-browser snapshot -i
   ```

3. **Element in iframe**
   ```bash
   # Switch to iframe first
   agent-browser frame "#iframe-id"
   agent-browser snapshot -i
   agent-browser click @e1
   agent-browser frame main  # Switch back
   ```

4. **Element hidden/not interactive**
   ```bash
   # Scroll element into view
   agent-browser scrollintoview @e5
   agent-browser click @e5
   ```

### Click Not Working

**Symptoms:** Click succeeds but nothing happens

**Solutions:**

1. **Element obscured by overlay**
   ```bash
   # Check for modals/overlays
   agent-browser snapshot -i
   # Look for overlay elements, close them first
   agent-browser click @overlay-close
   ```

2. **Need to wait for JS handlers**
   ```bash
   agent-browser wait 500  # Brief delay for JS
   agent-browser click @e1
   ```

3. **Wrong element**
   ```bash
   # Verify with highlight
   agent-browser highlight @e1
   # Or use headed mode
   agent-browser --headed click @e1
   ```

### Form Not Submitting

**Symptoms:** Fill works but submit doesn't

**Solutions:**

1. **Form validation failing**
   ```bash
   # Check for validation errors
   agent-browser snapshot -i
   # Look for error messages
   ```

2. **Need Enter key instead of click**
   ```bash
   agent-browser fill @e1 "value"
   agent-browser press Enter
   ```

3. **Multiple submit buttons**
   ```bash
   # Use more specific selector
   agent-browser find role button click --name "Submit Form"
   ```

### Page Not Loading

**Symptoms:** Timeout or blank page

**Solutions:**

1. **Check URL**
   ```bash
   agent-browser get url
   ```

2. **Check for redirects**
   ```bash
   agent-browser wait --url "**/expected-path"
   ```

3. **Check network**
   ```bash
   agent-browser network requests
   ```

4. **Use longer timeout**
   ```bash
   agent-browser wait --load networkidle --timeout 30000
   ```

### Stale References

**Symptoms:** Refs worked before but now fail

**Solution:** Always re-snapshot after:
- Page navigation
- Form submission
- Modal open/close
- Dynamic content load
- Tab switch

```bash
agent-browser click @submit-button
agent-browser wait --load networkidle
agent-browser snapshot -i  # ALWAYS re-snapshot
agent-browser click @e1    # Now use new refs
```

## Debugging Workflow

### Step 1: Reproduce in Headed Mode

```bash
agent-browser --headed open https://example.com
agent-browser --headed snapshot -i
# Follow your automation steps, watch what happens
```

### Step 2: Check Console and Errors

```bash
agent-browser console
agent-browser errors
```

### Step 3: Record a Trace

```bash
agent-browser trace start
# Run failing automation
agent-browser trace stop ./debug-trace.zip
# Open at trace.playwright.dev
```

### Step 4: Isolate the Problem

```bash
# Test each step individually
agent-browser open https://example.com
agent-browser wait --load networkidle
agent-browser snapshot -i
# Stop here - does snapshot look right?

agent-browser click @e1
# Did click work? Check state
agent-browser get url
agent-browser snapshot -i
```

## Environment Debugging

### Check Browser Version

```bash
agent-browser --version
```

### Check Browser Path

```bash
echo $AGENT_BROWSER_EXECUTABLE_PATH
agent-browser open about:version
agent-browser get text body
```

### Check Session State

```bash
agent-browser session
agent-browser session list
```

### Debug Logs

```bash
# Enable debug output
agent-browser --debug open https://example.com
```

## Performance Debugging

### Slow Page Load

```bash
# Check what's loading
agent-browser network requests

# Block heavy resources
agent-browser network route "**/*.mp4" --abort
agent-browser network route "**/analytics**" --abort
```

### Memory Issues

```bash
# Close unused sessions
agent-browser session list
agent-browser --session old-session close

# Clear storage
agent-browser cookies clear
agent-browser storage local clear
```

## CI/CD Debugging

### Capture Evidence

```bash
#!/bin/bash
set -e

# Always capture screenshot on failure
cleanup() {
    agent-browser screenshot ./failure-screenshot.png || true
    agent-browser console > ./console-logs.txt || true
    agent-browser errors > ./error-logs.txt || true
}
trap cleanup EXIT

# Run automation
agent-browser open https://example.com
# ... rest of automation
```

### Artifacts to Save

- Screenshots at key steps
- Console logs
- Error logs
- Trace files for complex failures
- Video recordings for visual verification
