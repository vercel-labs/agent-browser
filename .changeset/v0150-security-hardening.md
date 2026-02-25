---
"agent-browser": minor
---

- Added security hardening: authentication vault, content boundary markers, domain allowlist, action policy, action confirmation, and output length limits.
- Added `--download-path` flag (and `AGENT_BROWSER_DOWNLOAD_PATH` env / `downloadPath` config key) to set a default download directory.
- Added `--selector` flag to `scroll` command for scrolling within specific container elements.
