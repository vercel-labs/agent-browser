#!/bin/bash
# Template: Network Mocking Workflow
# Mock API responses for testing UI states, error handling, and edge cases

set -euo pipefail

URL="${1:?Usage: $0 <app-url>}"
SCREENSHOT_DIR="${2:-./mock-screenshots}"

mkdir -p "$SCREENSHOT_DIR"

echo "Starting network mocking workflow for: $URL"

# ============================================
# Example 1: Mock Empty State
# ============================================
echo "Testing empty state..."

# Mock API to return empty data
agent-browser network route "https://api.example.com/items" \
    --body '{"items": [], "total": 0}'

agent-browser open "$URL"
agent-browser wait --load networkidle
agent-browser screenshot "$SCREENSHOT_DIR/empty-state.png"

# Clear route for next test
agent-browser network unroute

# ============================================
# Example 2: Mock Error Response
# ============================================
echo "Testing error handling..."

# Mock 500 server error
agent-browser network route "https://api.example.com/items" \
    --body '{"error": "Internal server error"}' --status 500

agent-browser reload
agent-browser wait --load networkidle
agent-browser snapshot -i
agent-browser screenshot "$SCREENSHOT_DIR/error-state.png"

# Verify error UI is shown
# agent-browser find text "Something went wrong" is visible

agent-browser network unroute

# ============================================
# Example 3: Mock Loading State (block request)
# ============================================
echo "Testing loading state..."

# Block API to keep UI in loading state
agent-browser network route "https://api.example.com/items" --abort

agent-browser reload
agent-browser wait 1000  # Brief wait to see loading state
agent-browser screenshot "$SCREENSHOT_DIR/loading-state.png"

agent-browser network unroute

# ============================================
# Example 4: Mock Success with Data
# ============================================
echo "Testing populated state..."

# Mock successful response with sample data
agent-browser network route "https://api.example.com/items" \
    --body '{
        "items": [
            {"id": 1, "name": "Test Item 1", "status": "active"},
            {"id": 2, "name": "Test Item 2", "status": "pending"},
            {"id": 3, "name": "Test Item 3", "status": "completed"}
        ],
        "total": 3
    }'

agent-browser reload
agent-browser wait --load networkidle
agent-browser snapshot -i
agent-browser screenshot "$SCREENSHOT_DIR/populated-state.png"

agent-browser network unroute

# ============================================
# Example 5: Block Analytics/Tracking
# ============================================
echo "Blocking analytics for clean testing..."

agent-browser network route "**/google-analytics.com/**" --abort
agent-browser network route "**/facebook.com/tr**" --abort
agent-browser network route "**/hotjar.com/**" --abort
agent-browser network route "**/segment.com/**" --abort

# Continue with automation (analytics won't interfere)
agent-browser reload
agent-browser wait --load networkidle

# ============================================
# Example 6: View Tracked Requests
# ============================================
echo "Viewing network requests..."
agent-browser network requests

# Cleanup
agent-browser network unroute
agent-browser close

echo "Network mocking workflow complete"
echo "Screenshots saved to: $SCREENSHOT_DIR"
ls -la "$SCREENSHOT_DIR"
