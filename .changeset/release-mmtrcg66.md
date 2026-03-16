---
"agent-browser": patch
---

### New Features

- **Idle timeout for daemon auto-shutdown** - Added `--idle-timeout` CLI flag (and `AGENT_BROWSER_IDLE_TIMEOUT_MS` environment variable) to automatically shut down the daemon after a period of inactivity. Accepts human-friendly formats such as `10s`, `3m`, `1h`, or raw milliseconds (#856)
- **Cursor-interactive elements in snapshot tree** - Cursor-interactive elements are now embedded directly into the snapshot tree for richer context (#855)

### Bug Fixes

- Fixed **remote host support** in CDP discovery, enabling connection to browsers running on non-local hosts (#854)
- Fixed **CDP flag propagation** to the daemon process, ensuring reliable CDP reconnection across sessions (#857)
- Fixed **Windows auto-connect profiling** to correctly handle browser connection on Windows (#835, #840)
- Fixed **Windows transient error detection** by recognising Windows-specific socket error codes (`os error 10061` connection refused, `os error 10054` connection reset) during daemon reconnection attempts
