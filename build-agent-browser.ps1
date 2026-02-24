#!/usr/bin/env pwsh
# Build script for agent-browser native binary
# Usage: .\build-agent-browser.ps1

$ErrorActionPreference = "Stop"

Write-Host "Building agent-browser native binary..." -ForegroundColor Cyan
Write-Host ""

# Step 1: Sync version
Write-Host "[1/3] Syncing version..." -ForegroundColor Yellow
pnpm run version:sync

# Step 2: Build Rust binary
Write-Host "[2/3] Building Rust binary..." -ForegroundColor Yellow
cargo build --release --manifest-path cli/Cargo.toml

if ($LASTEXITCODE -ne 0) {
    Write-Error "Cargo build failed!"
}

# Step 3: Copy binary to bin directory
Write-Host "[3/3] Copying binary to bin directory..." -ForegroundColor Yellow
node scripts/copy-native.js

Write-Host ""
Write-Host "Build complete!" -ForegroundColor Green
Write-Host ""
Write-Host "To test the fix:" -ForegroundColor Cyan
Write-Host "  cd bin; .\agent-browser.exe connect 9222" -ForegroundColor White
