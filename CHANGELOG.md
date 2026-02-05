# agent-browser

## 0.9.1

### Patch Changes

- ae34945: Added --allow-file-access flag to enable opening and interacting with local file:// URLs (PDFs, HTML files) by passing Chromium flags that allow JavaScript access to local files. Added -C/--cursor flag for snapshots to include cursor-interactive elements like divs with onclick handlers or cursor:pointer styles, which is useful for modern web apps using custom clickable elements.

## 0.9.0

### Minor Changes

- 9d021bd: Add iOS Simulator and real device support for mobile Safari testing via Appium. New CLI commands include `device list` to show available simulators, `tap` and `swipe` for touch interactions, and the `--device` flag to specify which iOS device to use. Configure with `-p ios` provider flag or `AGENT_BROWSER_PROVIDER=ios` environment variable.

## 0.8.10

### Patch Changes

- 17dba8f: Add --stdin flag for eval command to read JavaScript from stdin, enabling heredoc usage for multiline scripts
- daeede4: Add --stdin flag for the eval command to read JavaScript from stdin, enabling heredoc usage for multiline scripts. Also fix binary permission issues on macOS/Linux when postinstall scripts don't run (e.g., with bun).

## 0.8.9

### Patch Changes

- 0dc36f2: Add --stdin flag for eval command to read JavaScript from stdin, enabling heredoc usage for multiline scripts

## 0.8.8

### Patch Changes

- 2771588: Added base64 encoding support for the eval command with -b/--base64 flag to avoid shell escaping issues when executing JavaScript. Updated documentation with AI agent setup instructions and reorganized the docs structure by consolidating agent mode content into the installation page.

## 0.8.7

### Patch Changes

- d24f753: Fixed browser launch options not being passed correctly when using persistent profiles, ensuring args, userAgent, proxy, and ignoreHTTPSErrors settings now work properly. Added pre-flight checks for socket path length limits and directory write permissions to provide clearer error messages when daemon startup fails. Improved error handling to properly exit with failure status when browser launch fails.

## 0.8.6

### Patch Changes

- d75350a: Improved daemon connection reliability by adding automatic retry logic for transient errors like connection resets, broken pipes, and temporary resource unavailability. The CLI now cleans up stale socket and PID files before starting a new daemon, and includes better detection of daemon responsiveness to handle race conditions during shutdown.

## 0.8.5

### Patch Changes

- cb2f8c3: Fixed version synchronization to automatically update Cargo.lock alongside Cargo.toml during releases, and made the CLI binary executable. This ensures the Rust CLI version stays in sync with the npm package version.

## 0.8.4

### Patch Changes

- 759302e: Fixed "Daemon not found" error when running through AI agents (e.g., Claude Code) by resolving symlinks in the executable path. Previously, npm global bin symlinks weren't being resolved correctly, causing intermittent daemon discovery failures.

## 0.8.3

### Patch Changes

- 4116a8a: Replaced shell-based CLI wrappers with a cross-platform Node.js wrapper to enable npx support on Windows. Added postinstall logic to patch npm's bin entry on global installs, allowing the native binary to be invoked directly with zero overhead. Added CI tests to verify global installation works correctly across all platforms.

## 0.8.2

### Patch Changes

- 7e6336f: Fixed the Windows CMD wrapper to use the native binary directly instead of routing through Node.js, improving startup performance and reliability. Added retry logic to the CI install command to handle transient failures during browser installation.

## 0.8.1

### Patch Changes

- 8eec634: Improved release workflow to validate binary file sizes and ensure binaries are executable after npm install. Updated documentation site with a new mobile navigation system and added v0.8.0 changelog entries. Reformatted CHANGELOG.md for better readability.

## v0.8.0

### New Features

- **Kernel cloud browser provider** - Connect to Kernel (https://kernel.sh) for remote browser infrastructure via `-p kernel` flag or `AGENT_BROWSER_PROVIDER=kernel`. Supports stealth mode, persistent profiles, and automatic profile find-or-create.
- **Ignore HTTPS certificate errors** - New `--ignore-https-errors` flag for working with self-signed certificates and development environments
- **Enhanced cookie management** - Extended `cookies set` command with `--url`, `--domain`, `--path`, `--httpOnly`, `--secure`, `--sameSite`, and `--expires` flags for setting cookies before page load

### Bug Fixes

- Fixed tab list command not recognizing new pages opened via clicks or `target="_blank"` links (#275)
- Fixed `check` command hanging indefinitely (#272)
- Fixed `set device` not applying deviceScaleFactor - HiDPI screenshots now work correctly (#270)
- Fixed state load and profile persistence not working in v0.7.6 (#268)
- Screenshots now save to temp directory when no path is provided (#247)

### Security

- Daemon and stream server now reject cross-origin connections (#274)

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
