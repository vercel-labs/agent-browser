#!/usr/bin/env bash
# Resolve a bundled location (file:line:col) to its original source via
# Next.js's dev-server endpoint `/__nextjs_original-stack-frames`.
#
# Usage:
#   sourcemap-resolve.sh <origin> <file> <line> <col>
#
# Example:
#   sourcemap-resolve.sh http://localhost:3000 /_next/static/chunks/app.js 1234 56
#
# Returns the original file:line:col if resolved, or the input unchanged
# when the endpoint returns no mapping (common for Next internals and
# production builds).

set -eu

origin=${1:?"usage: sourcemap-resolve.sh <origin> <file> <line> <col>"}
file=${2:?"usage: sourcemap-resolve.sh <origin> <file> <line> <col>"}
line=${3:?"usage: sourcemap-resolve.sh <origin> <file> <line> <col>"}
col=${4:?"usage: sourcemap-resolve.sh <origin> <file> <line> <col>"}

# Next's endpoint treats "/_next/..." as an app path - pass the path only,
# not the origin prefix.
path="$file"
case "$file" in
  "$origin"*)
    path="${file#"$origin"}"
    ;;
esac

body=$(printf '{"frames":[{"file":"%s","methodName":"","arguments":[],"line1":%s,"column1":%s}],"isServer":false,"isEdgeServer":false,"isAppDirectory":true}' \
  "$path" "$line" "$col")

response=$(curl -sS --max-time 5 \
  -H "Content-Type: application/json" \
  -X POST --data "$body" \
  "$origin/__nextjs_original-stack-frames" 2>/dev/null || true)

if [ -z "$response" ]; then
  echo "${file}:${line}:${col}"
  exit 0
fi

printf '%s' "$response" | node -e '
  let s = "";
  process.stdin.on("data", (c) => (s += c));
  process.stdin.on("end", () => {
    const arr = JSON.parse(s);
    const frame = arr && arr[0] && arr[0].status === "fulfilled"
      ? arr[0].value.originalStackFrame
      : null;
    if (!frame) {
      console.log(process.argv[1] + ":" + process.argv[2] + ":" + process.argv[3]);
      return;
    }
    console.log(frame.file + ":" + frame.line1 + ":" + frame.column1);
  });
' "$path" "$line" "$col"
