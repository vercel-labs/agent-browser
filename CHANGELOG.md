# agent-browser

## 0.7.6

### Patch Changes

- a4d0c26: Allow null values for the screenshot selector field. Previously, passing a null selector would fail validation, but now it is properly handled as an optional value.

## 0.7.5

### Patch Changes

- 8c2a6ec: Fix GitHub release workflow to handle existing releases. If a release already exists, binaries are uploaded to it instead of failing.

## 0.7.4

### Patch Changes

- 957b5e5: Fix binary permissions on install. npm doesn't preserve execute bits, so postinstall now ensures the native binary is executable.

## 0.7.3

### Patch Changes

- 161d8f5: Fix native binary distribution in npm package. Native binaries for all platforms (Linux x64/arm64, macOS x64/arm64, Windows x64) are now correctly included when publishing.

## 0.7.2

### Patch Changes

- 6afede2: Fix native binary distribution in npm package

  Native binaries for all platforms (Linux x64/arm64, macOS x64/arm64, Windows x64) are now included in the npm package. Previously, the release workflow published to npm before building binaries, causing "No binary found" errors on installation.

## 0.7.1

### Patch Changes

- Fix native binary distribution in npm package. Native binaries for all platforms (Linux x64/arm64, macOS x64/arm64, Windows x64) are now included in the npm package. Previously, the release workflow published to npm before building binaries, causing "No binary found" errors on installation.

## 0.7.0

### Minor Changes

- 316e649: ## New Features
  - **Cloud browser providers** - Connect to Browserbase or Browser Use for remote browser infrastructure via `-p` flag or `AGENT_BROWSER_PROVIDER` env var
  - **Persistent browser profiles** - Store cookies, localStorage, and login sessions across browser restarts with `--profile`
  - **Remote CDP WebSocket URLs** - Connect to remote browser services via WebSocket URL (e.g., `--cdp "wss://..."`)
  - **Download commands** - New `download` command and `wait --download` for file downloads with ref support
  - **Browser launch configuration** - New `--args`, `--user-agent`, and `--proxy-bypass` flags for fine-grained browser control
  - **Enhanced skills** - Hierarchical structure with references and templates for Claude Code

  ## Bug Fixes
  - Screenshot command now supports refs and has improved error messages
  - WebSocket URLs work in `connect` command
  - Fixed socket file location (uses `~/.agent-browser` instead of TMPDIR)
  - Windows binary path fix (.exe extension)
  - State load and path-based actions now show correct output messages

  ## Documentation
  - Added Claude Code marketplace plugin installation instructions
  - Updated skill documentation with references and templates
  - Improved error documentation
