---
"agent-browser": patch
---

### New Features

- **Linux musl (Alpine) builds** - Added pre-built binaries for **linux-musl** targeting both **x64** and **arm64** architectures, enabling native support for Alpine Linux and other musl-based distributions without requiring glibc (#784)

### Improvements

- **Consecutive `--auto-connect` commands** - Added support for issuing multiple consecutive `--auto-connect` commands without requiring a full browser relaunch; external connections are now correctly identified and reused (#786)
- **External browser disconnect behavior** - When using `--auto-connect` or `--cdp`, closing the agent session now disconnects cleanly without shutting down the user's browser process

### Bug Fixes

- **Restored `refs` dict in `--json` snapshot output** - The `refs` map containing role and name metadata for referenced elements is now correctly included in JSON snapshot responses (#787)
- Fixed e2e test assertions for `diff_snapshot` and `domain_filter` to correctly reflect expected behavior (#783)
- Fixed Chrome temp-dir cleanup test failing on Windows (#766)
