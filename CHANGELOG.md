# agent-browser

## 0.20.3

### Patch Changes

- e365909: ### Bug Fixes

  - **Chrome launch retry** - Chrome will now retry launching up to 3 times with a 500ms delay between attempts, improving resilience against transient startup failures (#791)
  - **Remote CDP snapshot hang** - Resolved an issue where snapshots would hang indefinitely over remote CDP (WSS) connections by removing WebSocket message and frame size limits to accommodate large responses (e.g. `Accessibility.getFullAXTree`), accepting binary frames from remote proxies such as Browserless, and immediately clearing pending commands when the connection closes rather than waiting for the 30-second timeout (#792)

## 0.20.2

### Patch Changes

- 944fa01: ### New Features

  - **Linux musl (Alpine) builds** - Added pre-built binaries for **linux-musl** targeting both **x64** and **arm64** architectures, enabling native support for Alpine Linux and other musl-based distributions without requiring glibc (#784)

  ### Improvements

  - **Consecutive `--auto-connect` commands** - Added support for issuing multiple consecutive `--auto-connect` commands without requiring a full browser relaunch; external connections are now correctly identified and reused (#786)
  - **External browser disconnect behavior** - When using `--auto-connect` or `--cdp`, closing the agent session now disconnects cleanly without shutting down the user's browser process

  ### Bug Fixes

  - **Restored `refs` dict in `--json` snapshot output** - The `refs` map containing role and name metadata for referenced elements is now correctly included in JSON snapshot responses (#787)
  - Fixed e2e test assertions for `diff_snapshot` and `domain_filter` to correctly reflect expected behavior (#783)
  - Fixed Chrome temp-dir cleanup test failing on Windows (#766)

## 0.20.1

### Patch Changes

- bd05917: ### Bug Fixes

  - Fixed **AX tree deserialization** to accept integer `nodeId` and `childIds` values for compatibility with Lightpanda, which sends numeric IDs where Chrome sends strings (#775)
  - Fixed **misleading SIGPIPE comment** to accurately describe the default Rust SIGPIPE behavior and why it is reset to `SIG_DFL` (#776)
  - Fixed **WebM recording output** to use the VP9 codec (`libvpx-vp9`) instead of H.264, producing valid WebM files; also adds a padding filter to ensure even frame dimensions (#779)

## 0.20.0

### Minor Changes

- 235fa88: ### Full Native Rust

  - **100% native Rust** -- Removed the entire Node.js/Playwright daemon. The Rust native daemon is now the only implementation. No Node.js runtime or Playwright dependency required. (#754)
  - **99x smaller install** -- Install size reduced from 710 MB to 7 MB by eliminating the Node.js dependency tree.
  - **18x less memory** -- Daemon memory usage reduced from 143 MB to 8 MB.
  - **1.6x faster cold start** -- Cold start time reduced from 1002ms to 617ms.
  - **Benchmarks** -- Added benchmark suite comparing native vs Node.js daemon performance.
  - **Chromium installer hardened** -- Fixed zip path traversal vulnerability in Chrome for Testing installer.

  ### Bug Fixes

  - Fixed `--headed false` flag not being respected in CLI (#757)
  - Fixed "not found" error pattern in `to_ai_friendly_error` incorrectly catching non-element errors (#759)
  - Fixed storage local key lookup parsing and text output (#761)
  - Fixed Lightpanda engine launch with release binaries (#760)
  - Hardened Lightpanda startup timeouts (#762)

## 0.19.0

### Minor Changes

- 56bb92b: ### New Features

  - **Browserless.io provider** -- Added browserless.io as a browser provider, supported in both Node.js and native daemon paths. Connect to remote Browserless instances with `--provider browserless` or `AGENT_BROWSER_PROVIDER=browserless`. Configurable via `BROWSERLESS_API_KEY`, `BROWSERLESS_API_URL`, and `BROWSERLESS_BROWSER_TYPE` environment variables. (#502, #746)
  - **`clipboard` command** -- Read from and write to the browser clipboard. Supports `read`, `write <text>`, `copy` (simulates Ctrl+C), and `paste` (simulates Ctrl+V) operations. (#749)
  - **Screenshot output configuration** -- New global flags `--screenshot-dir`, `--screenshot-quality`, `--screenshot-format` and corresponding `AGENT_BROWSER_SCREENSHOT_DIR`, `AGENT_BROWSER_SCREENSHOT_QUALITY`, `AGENT_BROWSER_SCREENSHOT_FORMAT` environment variables for persistent screenshot settings. (#749)

  ### Bug Fixes

  - Fixed `wait --text` not working in native daemon path (#749)
  - Fixed `BrowserManager.navigate()` and package entry point (#748)
  - Fixed extensions not being loaded from `config.json` (#750)
  - Fixed scroll on page load (#747)
  - Fixed HTML retrieval by using `browser.getLocator()` for selector operations (#745)

## 0.18.0

### Minor Changes

- 942b8cd: ### New Features

  - **`inspect` command** - Opens Chrome DevTools for the active page by launching a local proxy server that forwards the DevTools frontend to the browser's CDP WebSocket. Commands continue to work while DevTools is open. Implemented in both Node.js and native paths. (#736)
  - **`get cdp-url` subcommand** - Retrieve the Chrome DevTools Protocol WebSocket URL for the active page, useful for external debugging tools. (#736)
  - **Native screenshot annotate** - The `--annotate` flag for screenshots now works in the native Rust daemon, bringing parity with the Node.js path. (#706)

  ### Improvements

  - **KERNEL_API_KEY now optional** - External credential injection no longer requires `KERNEL_API_KEY` to be set, making it easier to use Kernel with pre-configured environments. (#687)
  - **Browserbase simplified** - Removed the `BROWSERBASE_PROJECT_ID` requirement, reducing setup friction for Browserbase users. (#625)

  ### Bug Fixes

  - Fixed Browserbase API using incorrect endpoint to release sessions (#707)
  - Fixed CDP connect paths using hardcoded 10s timeout instead of `getDefaultTimeout()` (#704)
  - Fixed lone Unicode surrogates causing errors by sanitizing with `toWellFormed()` (#720)
  - Fixed CDP connection failure on IPv6-first systems (#717)
  - Fixed recordings not inheriting the current viewport settings (#718)

## 0.17.1

### Patch Changes

- 94cd888: Added support for device scale factor (retina display) in the viewport command via an optional scale parameter. Also added webview target type support for better Electron application compatibility, and the pages list now includes target type information.

## 0.17.0

### Minor Changes

- 94521e7: ### New Features

  - **Lightpanda browser engine support** - Added `--engine <name>` flag to select the browser engine (`chrome` by default, or `lightpanda`), implying `--native` mode. Configurable via `AGENT_BROWSER_ENGINE` environment variable (#646)
  - **Dialog dismiss command** - Added support for `dismiss` subcommand in dialog command parsing (#605)

  ### Improvements

  - **Daemon startup error reporting** - Daemon startup errors are now surfaced directly instead of showing an opaque timeout message (#614)
  - **CDP port discovery** - Replaced broken hand-rolled HTTP client with `reqwest` for more reliable CDP port discovery (#619)
  - **Chrome extensions** - Extensions now load correctly by forcing headed mode when extensions are present (#652)
  - **Google Translate bar suppression** - Suppressed the Google Translate bar in native headless mode to avoid interference (#649)
  - **Auth cookie persistence** - Auth cookies are now persisted on browser close in native mode (#650)

  ### Bug Fixes

  - Fixed native auth login failing due to incompatible encryption format (#648)

  ### Documentation

  - Improved snapshot usage guidance and added reproducibility check (#630)
  - Added `--engine` flag to the README options table

  ### Performance

  - Added benchmarks to the CLI codebase (#637)

## 0.16.3

### Patch Changes

- 7d2c895: Fixed an issue where the --native flag was being passed to child processes even when not explicitly specified on the command line. The flag is now only forwarded when the user explicitly provides it, consistent with how other CLI flags like --allow-file-access and --download-path are handled.

## 0.16.2

### Patch Changes

- 01ac557: Added AGENT_BROWSER_HEADED environment variable support for running the browser in headed mode, and improved temporary profile cleanup when launching Chrome directly. Also includes documentation clarification that browser extensions work in both headed and headless modes.

## 0.16.1

### Patch Changes

- c4180c8: Improved Chrome launch reliability by automatically detecting containerized environments (Docker, Podman, Kubernetes) and enabling --no-sandbox when needed. Added support for discovering Playwright-installed Chromium browsers and enhanced error messages with helpful diagnostics when Chrome fails to launch.

## 0.16.0

### Minor Changes

- 05018b3: Added experimental native Rust daemon (`--native` flag, `AGENT_BROWSER_NATIVE=1` env, or `"native": true` in config). The native daemon communicates with Chrome directly via CDP, eliminating Node.js and Playwright dependencies. Supports 150+ commands with full parity to the default Node.js daemon. Includes WebDriver backend for Safari/iOS, CDP protocol codegen, request tracking, frame context management, and comprehensive e2e and parity tests.

## 0.15.3

### Patch Changes

- 62241b5: Fixed Windows compatibility issues including proper handling of extended-length path prefixes from canonicalize(), prevention of MSYS/Git Bash path translation that could mangle arguments, and improved daemon startup reliability. Also added ARM64 Windows support in postinstall shims and expanded CI testing with a full daemon lifecycle test on Windows.

## 0.15.2

### Patch Changes

- 6aea316: Documentation site improvements and internal tooling updates including enhanced code blocks, mobile navigation, and docs chat components. CLI connection and output handling refinements. Skill creator reference documentation and scripts have been reorganized.

## 0.15.1

### Patch Changes

- 7bd8ce9: Added support for chrome:// and chrome-extension:// URLs in navigation and recording commands. These special browser URLs are now preserved as-is instead of having https:// incorrectly prepended.

## 0.15.0

### Minor Changes

- 2e38882: - Added security hardening: authentication vault, content boundary markers, domain allowlist, action policy, action confirmation, and output length limits.
  - Added `--download-path` flag (and `AGENT_BROWSER_DOWNLOAD_PATH` env / `downloadPath` config key) to set a default download directory.
  - Added `--selector` flag to `scroll` command for scrolling within specific container elements.

## 0.14.0

### Minor Changes

- b7665e5: - Added `keyboard` command for raw keyboard input -- type with real keystrokes, insert text, and press shortcuts at the currently focused element without needing a selector.
  - Added `--color-scheme` flag and `AGENT_BROWSER_COLOR_SCHEME` env var for persistent dark/light mode preference across browser sessions.
  - Fixed IPC EAGAIN errors (os error 35/11) by adding backpressure-aware socket writes, command serialization, and lowering the default Playwright timeout to 25s (configurable via `AGENT_BROWSER_DEFAULT_TIMEOUT`).
  - Fixed remote debugging (CDP) reconnection.
  - Fixed state load failing when no browser is running.
  - Fixed `--annotate` flag warning appearing when not explicitly passed via CLI.

## 0.13.0

### Minor Changes

- ebd8717: Added new diff commands for comparing snapshots, screenshots, and URLs between page states. You can now run visual pixel diffs against baseline images, compare accessibility tree snapshots with customizable depth and selectors, and diff two URLs side-by-side with optional screenshot comparison.

## 0.12.0

### Minor Changes

- 69ffad0: Add annotated screenshots with the new --annotate flag, which overlays numbered labels on interactive elements and prints a legend mapping each label to its element ref. This enables multimodal AI models to reason about visual layout while using the same @eN refs for subsequent interactions. The flag can also be set via the AGENT_BROWSER_ANNOTATE environment variable.

## 0.11.1

### Patch Changes

- c6fc7df: Added documentation for command chaining with && across README, CLI help output, docs, and skill files, explaining how to efficiently chain multiple agent-browser commands in a single shell invocation since the browser persists via a background daemon.

## 0.11.0

### Minor Changes

- 5dc40b4: Added configuration file support with automatic loading from user and project directories, new profiler commands for Chrome DevTools profiling, computed styles getter, browser extension loading, storage state management, and iOS device emulation. Expanded click command with new-tab option, improved find command with additional actions and filtering options, and enhanced CDP connection to accept WebSocket URLs. Documentation has been significantly expanded with new sections for configuration, profiling, and proxy support.

## 0.10.0

### Minor Changes

- 1112a16: Added session persistence with automatic save/restore of cookies and localStorage across browser restarts using --session-name flag, with optional AES-256-GCM encryption for saved state data. New state management commands allow listing, showing, renaming, clearing, and cleaning up old session files. Also added --new-tab option for click commands to open links in new tabs.

## 0.9.4

### Patch Changes

- 323b6cd: Fix all Clippy lint warnings in the Rust CLI: remove redundant import, use `.first()` instead of `.get(0)`, use `.copied()` instead of `.map(|s| *s)`, use `.contains()` instead of `.iter().any()`, use `then_some` instead of lazy `then`, and simplify redundant match guards.

## 0.9.3

### Patch Changes

- d03e238: Added support for custom executable path in CLI browser launch options. Documentation site received UI improvements including a new chat component with sheet-based interface and updated dependencies.

## 0.9.2

### Patch Changes

- 76d23db: Documentation site migrated to MDX for improved content authoring, added AI-powered docs chat feature, and updated README with Homebrew installation instructions for macOS users.

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
