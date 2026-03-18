#!/bin/bash
set -e

# Build agent-browser for all platforms using Docker
# Usage: ./scripts/build-all-platforms.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="$PROJECT_ROOT/bin"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}Building agent-browser for all platforms...${NC}"
echo ""

# Ensure output directory exists
mkdir -p "$OUTPUT_DIR"

# Build the Docker image if needed
echo -e "${YELLOW}Building Docker cross-compilation image...${NC}"
docker build -t agent-browser-builder -f "$PROJECT_ROOT/docker/Dockerfile.build" "$PROJECT_ROOT"

# Function to build for a target
build_target() {
    local target=$1
    local output_name=$2
    
    echo -e "${YELLOW}Building for ${target}...${NC}"
    
    docker run --rm \
        -v "$PROJECT_ROOT/cli:/build" \
        -v "$OUTPUT_DIR:/output" \
        agent-browser-builder \
        -c "cargo zigbuild --release --target ${target} && cp /build/target/${target}/release/agent-browser* /output/${output_name} && chmod +x /output/${output_name} 2>/dev/null || true"
    
    if [ -f "$OUTPUT_DIR/$output_name" ]; then
        echo -e "${GREEN}✓ Built ${output_name}${NC}"
    else
        echo -e "${RED}✗ Failed to build ${output_name}${NC}"
        return 1
    fi
}

# Build for each platform
# Linux x64
build_target "x86_64-unknown-linux-gnu" "agent-browser-linux-x64"

# Linux ARM64
build_target "aarch64-unknown-linux-gnu" "agent-browser-linux-arm64"

# Windows x64
build_target "x86_64-pc-windows-gnu" "agent-browser-win32-x64.exe"

# macOS x64 (via zig for cross-compilation)
build_target "x86_64-apple-darwin" "agent-browser-darwin-x64"

# macOS ARM64 (via zig for cross-compilation)
build_target "aarch64-apple-darwin" "agent-browser-darwin-arm64"

# Linux musl x64 (Alpine)
build_target "x86_64-unknown-linux-musl" "agent-browser-linux-musl-x64"

# Linux musl ARM64 (Alpine)
build_target "aarch64-unknown-linux-musl" "agent-browser-linux-musl-arm64"

echo ""
echo -e "${GREEN}Build complete!${NC}"
echo ""
echo "Binaries are in: $OUTPUT_DIR"
ls -la "$OUTPUT_DIR"/agent-browser-*
