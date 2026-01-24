# Persistent Browser Profiles

Store cookies, localStorage, and login sessions across browser restarts using the `--profile` flag.

## Basic Usage

```bash
# First session: login and build up state
agent-browser --profile ~/.myapp open https://app.example.com/login
agent-browser --profile ~/.myapp snapshot -i
agent-browser --profile ~/.myapp fill @e1 "username"
agent-browser --profile ~/.myapp fill @e2 "password"
agent-browser --profile ~/.myapp click @e3
agent-browser --profile ~/.myapp close

# Later session: already logged in
agent-browser --profile ~/.myapp open https://app.example.com/dashboard
# No login needed - cookies and session persist
```

## Environment Variable

Set a default profile path:

```bash
export AGENT_BROWSER_PROFILE="~/.myapp-browser"
agent-browser open https://app.example.com  # Uses profile automatically
```

## Profile vs Session

| Feature | `--session` | `--profile` |
|---------|-------------|-------------|
| Isolation | In-memory, lost on close | Persisted to disk |
| Cookies | Session only | Persist across restarts |
| localStorage | Session only | Persist across restarts |
| Use case | Parallel testing | Long-lived auth state |

## Common Patterns

### Login Once, Reuse Forever

```bash
#!/bin/bash
PROFILE="$HOME/.app-profile"

# Check if we need to login
agent-browser --profile "$PROFILE" open https://app.example.com
URL=$(agent-browser --profile "$PROFILE" get url)

if [[ "$URL" == *"/login"* ]]; then
    echo "Not logged in, performing login..."
    agent-browser --profile "$PROFILE" snapshot -i
    agent-browser --profile "$PROFILE" fill @e1 "$USERNAME"
    agent-browser --profile "$PROFILE" fill @e2 "$PASSWORD"
    agent-browser --profile "$PROFILE" click @e3
    agent-browser --profile "$PROFILE" wait --url "**/dashboard"
fi

# Now authenticated - continue with automation
agent-browser --profile "$PROFILE" snapshot -i
```

### Multiple Accounts

```bash
# Different profile per account
agent-browser --profile ~/.app-user1 open https://app.example.com
agent-browser --profile ~/.app-user2 open https://app.example.com
agent-browser --profile ~/.app-admin open https://app.example.com/admin
```

### Development vs Production

```bash
# Separate profiles for different environments
agent-browser --profile ~/.app-dev open https://dev.example.com
agent-browser --profile ~/.app-staging open https://staging.example.com
agent-browser --profile ~/.app-prod open https://app.example.com
```

## What Gets Persisted

- Cookies (including HttpOnly cookies)
- localStorage
- sessionStorage (restored on next launch)
- IndexedDB
- Service Worker registrations
- Cache Storage

## Profile Location

The profile is stored at the specified path:

```bash
~/.myapp/
├── Default/
│   ├── Cookies
│   ├── Local Storage/
│   ├── Session Storage/
│   └── ...
└── ...
```

## Clearing Profile State

```bash
# Clear specific data
agent-browser --profile ~/.myapp cookies clear
agent-browser --profile ~/.myapp storage local clear

# Or delete the entire profile
rm -rf ~/.myapp
```

## Combining with Sessions

You can use both `--profile` and `--session` together:

```bash
# Named session within a profile
agent-browser --profile ~/.myapp --session test1 open https://example.com
agent-browser --profile ~/.myapp --session test2 open https://example.com
```

This creates isolated sessions that still share the same persistent profile data.

## Best Practices

1. **Use descriptive profile paths** - `~/.myapp-browser` instead of `~/profile1`
2. **One profile per application/account** - Don't mix different apps in one profile
3. **Gitignore profiles** - Add profile paths to `.gitignore` to avoid committing auth state
4. **Rotate profiles periodically** - Delete and recreate if sessions become stale
5. **Use environment variables in CI** - Set `AGENT_BROWSER_PROFILE` for consistent automation
