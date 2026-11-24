#!/usr/bin/env bash
# Build the macOS PiP helper as a universal binary.
# Writes bin/agent-browser-pip-darwin and copies it next to any local cargo build
# so `agent-browser pip` finds it from a source checkout.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/pip/macos/main.swift"
out="$root/bin/agent-browser-pip-darwin"
min_macos="12.0"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "Skipping the PiP helper: it builds on macOS only."
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for arch in arm64 x86_64; do
  swiftc -O -target "$arch-apple-macos$min_macos" "$src" -o "$work/pip-$arch"
done

mkdir -p "$root/bin"
lipo -create "$work/pip-arm64" "$work/pip-x86_64" -output "$out"
chmod +x "$out"
echo "✓ Built $out"

for profile in debug release; do
  dir="$root/cli/target/$profile"
  if [[ -x "$dir/agent-browser" ]]; then
    cp "$out" "$dir/agent-browser-pip"
    echo "✓ Copied to $dir/agent-browser-pip"
  fi
done
