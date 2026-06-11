#!/bin/bash
set -euo pipefail

# Download release binaries and generate SHA256SUMS.
# Usage: ./scripts/generate-checksums.sh [version]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT_FILE="$PROJECT_ROOT/SHA256SUMS"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

GITHUB_REPO="vercel-labs/agent-browser"

if [ "${1:-}" ]; then
  VERSION="${1#v}"
else
  VERSION="$(
    curl -fsSL "https://api.github.com/repos/$GITHUB_REPO/releases/latest" \
      | node -e "let data=''; process.stdin.on('data', c => data += c); process.stdin.on('end', () => console.log(JSON.parse(data).tag_name.replace(/^v/, '')));"
  )"
fi

BASE_URL="https://github.com/$GITHUB_REPO/releases/download/v$VERSION"

BINARIES=(
  "agent-browser-linux-x64"
  "agent-browser-linux-arm64"
  "agent-browser-win32-x64.exe"
  "agent-browser-darwin-x64"
  "agent-browser-darwin-arm64"
  "agent-browser-linux-musl-x64"
  "agent-browser-linux-musl-arm64"
)

echo "Generating checksums for v$VERSION"
rm -f "$OUTPUT_FILE"

for binary in "${BINARIES[@]}"; do
  echo "Downloading $binary"
  curl -fsSL "$BASE_URL/$binary" -o "$TMP_DIR/$binary"

  if command -v sha256sum >/dev/null 2>&1; then
    hash="$(sha256sum "$TMP_DIR/$binary" | awk '{print $1}')"
  else
    hash="$(shasum -a 256 "$TMP_DIR/$binary" | awk '{print $1}')"
  fi

  printf "%s  %s\n" "$hash" "$binary" >> "$OUTPUT_FILE"
done

echo "Wrote $OUTPUT_FILE"
