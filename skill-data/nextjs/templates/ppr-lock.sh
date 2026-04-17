#!/usr/bin/env bash
# Set the PPR instant-navigation cookie for the current browser origin.
# The dev server uses this cookie to respond with the static shell only,
# mimicking a prefetched instant navigation.
#
# Usage:
#   ppr-lock.sh [domain]
#
# Without <domain>, reads the current page's hostname from agent-browser.

set -eu

domain=${1:-}
if [ -z "$domain" ]; then
  url=$(agent-browser get url 2>/dev/null || true)
  if [ -z "$url" ]; then
    echo "no current URL - pass a <domain> explicitly" >&2
    exit 1
  fi
  domain=$(printf '%s' "$url" | awk -F/ '{print $3}' | awk -F: '{print $1}')
fi

rand=$(od -An -N4 -tu4 /dev/urandom | tr -d ' ')
agent-browser cookies set next-instant-navigation-testing \
  "[0,\"p${rand}\"]" --domain "$domain"

echo "locked PPR on $domain"
