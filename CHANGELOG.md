# agent-browser

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
