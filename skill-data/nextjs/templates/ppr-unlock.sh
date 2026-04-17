#!/usr/bin/env bash
# Clear the PPR instant-navigation cookie and (optionally) capture the
# unlocked suspense boundary report for comparison with the locked one.
#
# Usage:
#   ppr-unlock.sh                 # clear the cookie only
#   ppr-unlock.sh --report        # clear + run `react suspense`

set -eu

agent-browser cookies clear
echo "unlocked PPR"

case "${1:-}" in
  --report)
    # Give the page time to re-render with dynamic content after the
    # cookie clears, then snapshot the boundaries.
    sleep 1
    agent-browser react suspense
    ;;
esac
