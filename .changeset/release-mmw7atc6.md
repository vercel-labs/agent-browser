---
"agent-browser": patch
---

### New Features

- **HAR 1.2 network capture** - Added commands to capture and export network traffic in HAR 1.2 format, including accurate request/response timing, headers, body sizes, and resource types sourced from Chrome DevTools Protocol events (#864)
- **Built-in `upgrade` command** - Added `agent-browser upgrade` to self-update the CLI; automatically detects your installation method (npm, Homebrew, or Cargo) and runs the appropriate update command (#898)

### Documentation

- Added `upgrade` command to the README command reference and installation guide
- Added a dedicated **Updating** section to the README with usage instructions for `agent-browser upgrade`
