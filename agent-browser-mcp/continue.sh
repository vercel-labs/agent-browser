#!/bin/bash
# Helper script to signal agent-browser to continue after a pause

CONTINUE_SIGNAL="$HOME/.agent-browser-continue"
WAITING_SIGNAL="$HOME/.agent-browser-waiting"

# Check if agent is waiting
if [ -f "$WAITING_SIGNAL" ]; then
    echo "Agent is waiting: $(cat "$WAITING_SIGNAL")"
    echo ""
fi

# Create continue signal
touch "$CONTINUE_SIGNAL"
echo "✓ Signaled agent to continue"

# Show status
if [ -f "$WAITING_SIGNAL" ]; then
    rm -f "$WAITING_SIGNAL" 2>/dev/null
fi
