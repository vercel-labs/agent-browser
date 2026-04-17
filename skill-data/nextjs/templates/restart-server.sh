#!/usr/bin/env bash
# Restart the Next.js dev server and wait for the new process to come up.
#
# Usage:
#   restart-server.sh <origin>
#
# Last resort when the dev server is wedged. HMR picks up code changes
# on its own; reach for this only when you have evidence of a problem
# (stale output after edits, builds that never finish, errors that don't
# clear).

set -eu

origin=${1:?"usage: restart-server.sh <origin>"}

# Capture the current executionId so we can detect the new process.
before=$(curl -sS --max-time 5 "$origin/__nextjs_server_status" \
  | node -e 'let s="";process.stdin.on("data",c=>s+=c);process.stdin.on("end",()=>{try{console.log(JSON.parse(s).executionId||"")}catch{}})')

curl -sS -X POST --max-time 5 \
  "$origin/__nextjs_restart_dev?invalidateFileSystemCache=1" > /dev/null || true

deadline=$(( $(date +%s) + 30 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  sleep 1
  after=$(curl -sS --max-time 5 "$origin/__nextjs_server_status" 2>/dev/null \
    | node -e 'let s="";process.stdin.on("data",c=>s+=c);process.stdin.on("end",()=>{try{console.log(JSON.parse(s).executionId||"")}catch{}})' \
    || true)
  if [ -n "$after" ] && [ "$after" != "$before" ]; then
    echo "restarted (new executionId: $after)"
    exit 0
  fi
done

echo "timeout waiting for restart - check the dev server manually" >&2
exit 1
