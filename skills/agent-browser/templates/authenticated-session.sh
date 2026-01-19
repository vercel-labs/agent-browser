#!/bin/bash
# Template: Authenticated Session Workflow
# Login once, save state, reuse for subsequent runs

set -euo pipefail

LOGIN_URL="${1:?Usage: $0 <login-url> [state-file]}"
STATE_FILE="${2:-./auth-state.json}"

# Check for required environment variables
: "${APP_USERNAME:?Set APP_USERNAME environment variable}"
: "${APP_PASSWORD:?Set APP_PASSWORD environment variable}"

echo "Authentication workflow for: $LOGIN_URL"

# Check if we have saved state
if [[ -f "$STATE_FILE" ]]; then
    echo "Loading saved authentication state..."
    agent-browser state load "$STATE_FILE"

    # Navigate to verify session is still valid
    agent-browser open "$LOGIN_URL"
    agent-browser wait --load networkidle

    CURRENT_URL=$(agent-browser get url)

    # Check if we got redirected to login (session expired)
    if [[ "$CURRENT_URL" == *"login"* ]] || [[ "$CURRENT_URL" == *"signin"* ]]; then
        echo "Session expired, performing fresh login..."
        rm -f "$STATE_FILE"
    else
        echo "Session restored successfully!"
        agent-browser snapshot -i
        exit 0
    fi
fi

# Perform fresh login
echo "Performing login..."
agent-browser open "$LOGIN_URL"
agent-browser wait --load networkidle

# Get form elements
echo "Analyzing login form..."
agent-browser snapshot -i

# Fill credentials
# Adjust refs based on your login form structure
# agent-browser fill @e1 "$APP_USERNAME"    # Email/username field
# agent-browser fill @e2 "$APP_PASSWORD"    # Password field

# Submit login
# agent-browser click @e3                    # Login button

# Wait for navigation after login
agent-browser wait --load networkidle

# Verify login succeeded
FINAL_URL=$(agent-browser get url)
if [[ "$FINAL_URL" == *"login"* ]] || [[ "$FINAL_URL" == *"signin"* ]]; then
    echo "ERROR: Login failed - still on login page"
    agent-browser screenshot /tmp/login-failed.png
    agent-browser close
    exit 1
fi

# Save authenticated state for future use
echo "Saving authentication state to: $STATE_FILE"
agent-browser state save "$STATE_FILE"

echo "Login successful!"
agent-browser snapshot -i

# Optional: Continue with authenticated actions
# agent-browser open "https://app.example.com/dashboard"
# agent-browser snapshot -i
