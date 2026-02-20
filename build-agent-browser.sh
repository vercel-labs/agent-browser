#!/bin/bash
# Build script for agent-browser native binary
# Usage: ./build-agent-browser.sh

set -e

echo "Building agent-browser native binary..."
echo ""

# Step 1: Sync version
echo "[1/3] Syncing version..."
pnpm run version:sync

# Step 2: Build Rust binary
echo "[2/3] Building Rust binary..."
cargo build --release --manifest-path cli/Cargo.toml

# Step 3: Copy binary to bin directory
echo "[3/3] Copying binary to bin directory..."
node scripts/copy-native.js

echo ""
echo "Build complete!"
echo ""
echo "To test the fix:"
echo "  cd bin; ./agent-browser connect 9222"
