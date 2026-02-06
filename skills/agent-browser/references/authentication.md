# Authentication Patterns

Patterns for handling login flows, session persistence, and authenticated browsing.

## Basic Login Flow

```bash
# Navigate to login page
npx agent-browser open https://app.example.com/login
npx agent-browser wait --load networkidle

# Get form elements
npx agent-browser snapshot -i
# Output: @e1 [input type="email"], @e2 [input type="password"], @e3 [button] "Sign In"

# Fill credentials
npx agent-browser fill @e1 "user@example.com"
npx agent-browser fill @e2 "password123"

# Submit
npx agent-browser click @e3
npx agent-browser wait --load networkidle

# Verify login succeeded
npx agent-browser get url  # Should be dashboard, not login
```

## Saving Authentication State

After logging in, save state for reuse:

```bash
# Login first (see above)
npx agent-browser open https://app.example.com/login
npx agent-browser snapshot -i
npx agent-browser fill @e1 "user@example.com"
npx agent-browser fill @e2 "password123"
npx agent-browser click @e3
npx agent-browser wait --url "**/dashboard"

# Save authenticated state
npx agent-browser state save ./auth-state.json
```

## Restoring Authentication

Skip login by loading saved state:

```bash
# Load saved auth state
npx agent-browser state load ./auth-state.json

# Navigate directly to protected page
npx agent-browser open https://app.example.com/dashboard

# Verify authenticated
npx agent-browser snapshot -i
```

## OAuth / SSO Flows

For OAuth redirects:

```bash
# Start OAuth flow
npx agent-browser open https://app.example.com/auth/google

# Handle redirects automatically
npx agent-browser wait --url "**/accounts.google.com**"
npx agent-browser snapshot -i

# Fill Google credentials
npx agent-browser fill @e1 "user@gmail.com"
npx agent-browser click @e2  # Next button
npx agent-browser wait 2000
npx agent-browser snapshot -i
npx agent-browser fill @e3 "password"
npx agent-browser click @e4  # Sign in

# Wait for redirect back
npx agent-browser wait --url "**/app.example.com**"
npx agent-browser state save ./oauth-state.json
```

## Two-Factor Authentication

Handle 2FA with manual intervention:

```bash
# Login with credentials
npx agent-browser open https://app.example.com/login --headed  # Show browser
npx agent-browser snapshot -i
npx agent-browser fill @e1 "user@example.com"
npx agent-browser fill @e2 "password123"
npx agent-browser click @e3

# Wait for user to complete 2FA manually
echo "Complete 2FA in the browser window..."
npx agent-browser wait --url "**/dashboard" --timeout 120000

# Save state after 2FA
npx agent-browser state save ./2fa-state.json
```

## HTTP Basic Auth

For sites using HTTP Basic Authentication:

```bash
# Set credentials before navigation
npx agent-browser set credentials username password

# Navigate to protected resource
npx agent-browser open https://protected.example.com/api
```

## Cookie-Based Auth

Manually set authentication cookies:

```bash
# Set auth cookie
npx agent-browser cookies set session_token "abc123xyz"

# Navigate to protected page
npx agent-browser open https://app.example.com/dashboard
```

## Token Refresh Handling

For sessions with expiring tokens:

```bash
#!/bin/bash
# Wrapper that handles token refresh

STATE_FILE="./auth-state.json"

# Try loading existing state
if [[ -f "$STATE_FILE" ]]; then
    npx agent-browser state load "$STATE_FILE"
    npx agent-browser open https://app.example.com/dashboard

    # Check if session is still valid
    URL=$(npx agent-browser get url)
    if [[ "$URL" == *"/login"* ]]; then
        echo "Session expired, re-authenticating..."
        # Perform fresh login
        npx agent-browser snapshot -i
        npx agent-browser fill @e1 "$USERNAME"
        npx agent-browser fill @e2 "$PASSWORD"
        npx agent-browser click @e3
        npx agent-browser wait --url "**/dashboard"
        npx agent-browser state save "$STATE_FILE"
    fi
else
    # First-time login
    npx agent-browser open https://app.example.com/login
    # ... login flow ...
fi
```

## Security Best Practices

1. **Never commit state files** - They contain session tokens
   ```bash
   echo "*.auth-state.json" >> .gitignore
   ```

2. **Use environment variables for credentials**
   ```bash
   npx agent-browser fill @e1 "$APP_USERNAME"
   npx agent-browser fill @e2 "$APP_PASSWORD"
   ```

3. **Clean up after automation**
   ```bash
   npx agent-browser cookies clear
   rm -f ./auth-state.json
   ```

4. **Use short-lived sessions for CI/CD**
   ```bash
   # Don't persist state in CI
   npx agent-browser open https://app.example.com/login
   # ... login and perform actions ...
   npx agent-browser close  # Session ends, nothing persisted
   ```
