#!/bin/bash
# Signal agent-browser to continue after a pause

CONTINUE_SIGNAL="$HOME/.agent-browser-continue"
WAITING_SIGNAL="$HOME/.agent-browser-waiting"

# Check if agent is waiting
if [ -f "$WAITING_SIGNAL" ]; then
    echo "Agent was waiting: $(cat "$WAITING_SIGNAL")"
fi

# Create continue signal
touch "$CONTINUE_SIGNAL"
echo "✓ Signaled agent to continue"

# Clean up
rm -f "$WAITING_SIGNAL" 2>/dev/null
