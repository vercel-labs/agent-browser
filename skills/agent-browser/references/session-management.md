# Session Management

Run multiple isolated browser sessions concurrently with state persistence.

## Named Sessions

Use `--session` flag to isolate browser contexts:

```bash
# Session 1: Authentication flow
npx agent-browser --session auth open https://app.example.com/login

# Session 2: Public browsing (separate cookies, storage)
npx agent-browser --session public open https://example.com

# Commands are isolated by session
npx agent-browser --session auth fill @e1 "user@example.com"
npx agent-browser --session public get text body
```

## Session Isolation Properties

Each session has independent:
- Cookies
- LocalStorage / SessionStorage
- IndexedDB
- Cache
- Browsing history
- Open tabs

## Session State Persistence

### Save Session State

```bash
# Save cookies, storage, and auth state
npx agent-browser state save /path/to/auth-state.json
```

### Load Session State

```bash
# Restore saved state
npx agent-browser state load /path/to/auth-state.json

# Continue with authenticated session
npx agent-browser open https://app.example.com/dashboard
```

### State File Contents

```json
{
  "cookies": [...],
  "localStorage": {...},
  "sessionStorage": {...},
  "origins": [...]
}
```

## Common Patterns

### Authenticated Session Reuse

```bash
#!/bin/bash
# Save login state once, reuse many times

STATE_FILE="/tmp/auth-state.json"

# Check if we have saved state
if [[ -f "$STATE_FILE" ]]; then
    npx agent-browser state load "$STATE_FILE"
    npx agent-browser open https://app.example.com/dashboard
else
    # Perform login
    npx agent-browser open https://app.example.com/login
    npx agent-browser snapshot -i
    npx agent-browser fill @e1 "$USERNAME"
    npx agent-browser fill @e2 "$PASSWORD"
    npx agent-browser click @e3
    npx agent-browser wait --load networkidle

    # Save for future use
    npx agent-browser state save "$STATE_FILE"
fi
```

### Concurrent Scraping

```bash
#!/bin/bash
# Scrape multiple sites concurrently

# Start all sessions
npx agent-browser --session site1 open https://site1.com &
npx agent-browser --session site2 open https://site2.com &
npx agent-browser --session site3 open https://site3.com &
wait

# Extract from each
npx agent-browser --session site1 get text body > site1.txt
npx agent-browser --session site2 get text body > site2.txt
npx agent-browser --session site3 get text body > site3.txt

# Cleanup
npx agent-browser --session site1 close
npx agent-browser --session site2 close
npx agent-browser --session site3 close
```

### A/B Testing Sessions

```bash
# Test different user experiences
npx agent-browser --session variant-a open "https://app.com?variant=a"
npx agent-browser --session variant-b open "https://app.com?variant=b"

# Compare
npx agent-browser --session variant-a screenshot /tmp/variant-a.png
npx agent-browser --session variant-b screenshot /tmp/variant-b.png
```

## Default Session

When `--session` is omitted, commands use the default session:

```bash
# These use the same default session
npx agent-browser open https://example.com
npx agent-browser snapshot -i
npx agent-browser close  # Closes default session
```

## Session Cleanup

```bash
# Close specific session
npx agent-browser --session auth close

# List active sessions
npx agent-browser session list
```

## Best Practices

### 1. Name Sessions Semantically

```bash
# GOOD: Clear purpose
npx agent-browser --session github-auth open https://github.com
npx agent-browser --session docs-scrape open https://docs.example.com

# AVOID: Generic names
npx agent-browser --session s1 open https://github.com
```

### 2. Always Clean Up

```bash
# Close sessions when done
npx agent-browser --session auth close
npx agent-browser --session scrape close
```

### 3. Handle State Files Securely

```bash
# Don't commit state files (contain auth tokens!)
echo "*.auth-state.json" >> .gitignore

# Delete after use
rm /tmp/auth-state.json
```

### 4. Timeout Long Sessions

```bash
# Set timeout for automated scripts
timeout 60 npx agent-browser --session long-task get text body
```
