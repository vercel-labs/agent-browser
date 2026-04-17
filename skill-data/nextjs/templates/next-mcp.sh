#!/usr/bin/env bash
# Next.js dev-server MCP bridge.
#
# Usage:
#   next-mcp.sh <origin> <tool> [args-json]
#
# Tools: get_errors, get_logs, get_page_metadata, get_project_metadata,
#        get_routes, get_server_action_by_id
#
# Example:
#   next-mcp.sh http://localhost:3000 get_errors
#   next-mcp.sh http://localhost:3000 get_server_action_by_id '{"actionId":"abc"}'

set -eu

origin=${1:?"usage: next-mcp.sh <origin> <tool> [args-json]"}
tool=${2:?"usage: next-mcp.sh <origin> <tool> [args-json]"}
args_json=${3:-"{}"}

body=$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"%s","arguments":%s}}' \
  "$tool" "$args_json")

response=$(curl -sS --max-time 10 \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -X POST --data "$body" \
  "$origin/_next/mcp")

# SSE response: pick the first `data:` frame.
data=$(printf '%s\n' "$response" | awk '/^data: / { sub(/^data: /, ""); print; exit }')
if [ -z "$data" ]; then
  echo "no data frame in MCP response" >&2
  printf '%s\n' "$response" >&2
  exit 1
fi

# If the result carries a text field, print that (usually JSON); otherwise
# print the result object.
err=$(printf '%s' "$data" | sed -n 's/.*"error":{[^}]*"message":"\([^"]*\)".*/\1/p')
if [ -n "$err" ]; then
  echo "MCP error: $err" >&2
  exit 1
fi

# Try to extract result.content[0].text; fall back to printing the whole
# result. We lean on Node for JSON parsing because bash doesn't.
printf '%s' "$data" | node -e '
  let s = "";
  process.stdin.on("data", (c) => (s += c));
  process.stdin.on("end", () => {
    const parsed = JSON.parse(s);
    const text = parsed.result && parsed.result.content && parsed.result.content[0] && parsed.result.content[0].text;
    if (text) {
      try { console.log(JSON.stringify(JSON.parse(text), null, 2)); }
      catch { console.log(text); }
      return;
    }
    console.log(JSON.stringify(parsed.result, null, 2));
  });
'
