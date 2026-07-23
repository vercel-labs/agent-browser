# Session Management

Multiple isolated browser sessions with state persistence and concurrent browsing.

**Related**: [authentication.md](authentication.md) for login patterns, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Named Sessions](#named-sessions)
- [Session Isolation Properties](#session-isolation-properties)
- [Session State Persistence](#session-state-persistence)
- [Common Patterns](#common-patterns)
- [Default Session](#default-session)
- [Session Cleanup](#session-cleanup)
- [Best Practices](#best-practices)

## Named Sessions

Use `--session` to isolate browser contexts. Agent skills should derive one stable id and reuse it on every command:

```bash
SESSION="$(agent-browser session id --scope worktree --prefix my-skill)"
agent-browser --session "$SESSION" --restore open https://app.example.com/login
```

`--scope worktree` uses the Git worktree root when available, then the Git root, then the canonical current directory. This is the recommended default for agents because worktrees are commonly used for parallel agent runs.

```bash
# Session 1: Authentication flow
agent-browser --session auth open https://app.example.com/login

# Session 2: Public browsing (separate cookies, storage)
agent-browser --session public open https://example.com

# Commands are isolated by session
agent-browser --session auth fill @e1 "user@example.com"
agent-browser --session public get text body
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

### Automatic Restore

```bash
# Bare --restore uses the current --session as the persistence key
SESSION="$(agent-browser session id --scope worktree --prefix next-dev-loop)"
agent-browser --session "$SESSION" --restore open https://app.example.com/dashboard
```

When `--restore` or another restore key is configured, state is loaded before navigation and saved on close, daemon shutdown, idle timeout, and compatible relaunch. It is also saved periodically while the browser is open (after commands settle, at most once per `AGENT_BROWSER_AUTOSAVE_INTERVAL_MS`, default 30000; set to `0` to save only on close), so a browser window the user closes by hand still leaves a recent save behind. A session ID by itself only isolates the daemon and does not enable persistence; without a restore key, shutdown discards transient browser state and open tabs. Idle sessions with configured persistence keep saving on the same interval, capturing changes the page makes on its own such as token refreshes. The daemon exits after one hour without commands or dashboard input by default; `--idle-timeout <time>` or `AGENT_BROWSER_IDLE_TIMEOUT_MS` tunes this, and `0` disables it. Headed, Safari/iOS WebDriver, and user-attached browsers are exempt from the default timeout; provider-owned cloud browsers are not. The default save policy is `--restore-save auto`, which skips auto-save if restore failed or validation failed; `never` disables periodic autosave too.

```bash
agent-browser --session "$SESSION" --restore --restore-check-url "**/dashboard" open https://app.example.com/dashboard
agent-browser --session "$SESSION" --restore --restore-check-text Dashboard open https://app.example.com/dashboard
agent-browser --session "$SESSION" --restore --restore-check-fn "!!localStorage.getItem('session')" open https://app.example.com/dashboard
```

Use `agent-browser session info --json` for diagnostics:

```bash
agent-browser --session "$SESSION" session info --json
```

### Manual State Files

Use `state save`, `state load`, and `--state <path>` when you need an explicit portable JSON file. Do not make agents construct paths under `~/.agent-browser/sessions/`; prefer `--restore` for reusable agent sessions.

## Common Patterns

### Authenticated Session Reuse

```bash
#!/bin/bash
SESSION="$(agent-browser session id --scope worktree --prefix app)"
agent-browser --session "$SESSION" --restore open https://app.example.com/dashboard
```

### Concurrent Scraping

```bash
#!/bin/bash
# Scrape multiple sites concurrently

# Start all sessions
agent-browser --session site1 open https://site1.com &
agent-browser --session site2 open https://site2.com &
agent-browser --session site3 open https://site3.com &
wait

# Extract from each
agent-browser --session site1 get text body > site1.txt
agent-browser --session site2 get text body > site2.txt
agent-browser --session site3 get text body > site3.txt

# Cleanup
agent-browser --session site1 close
agent-browser --session site2 close
agent-browser --session site3 close
```

### A/B Testing Sessions

```bash
# Test different user experiences
agent-browser --session variant-a open "https://app.com?variant=a"
agent-browser --session variant-b open "https://app.com?variant=b"

# Compare
agent-browser --session variant-a screenshot /tmp/variant-a.png
agent-browser --session variant-b screenshot /tmp/variant-b.png
```

## Default Session

When `--session` is omitted, commands use the default session:

```bash
# These use the same default session
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser close  # Closes default session
```

## Session Cleanup

```bash
# Close specific session
agent-browser --session auth close

# List active sessions
agent-browser session list
```

## Best Practices

### 1. Name Sessions Semantically

```bash
# GOOD: Clear purpose
agent-browser --session github-auth open https://github.com
agent-browser --session docs-scrape open https://docs.example.com

# AVOID: Generic names
agent-browser --session s1 open https://github.com
```

### 2. Always Clean Up

```bash
# Close sessions when done
agent-browser --session auth close
agent-browser --session scrape close
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
timeout 60 agent-browser --session long-task get text body
```
