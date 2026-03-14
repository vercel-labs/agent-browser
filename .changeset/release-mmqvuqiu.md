---
"agent-browser": patch
---

### Bug Fixes

- **Broadcast channel lag handling** - Fixed an issue where **broadcast channel lag** errors were incorrectly treated as stream closure, causing premature termination of event listeners in reload, response body, download, and navigation wait operations. Lagged messages are now skipped and the loop continues instead of breaking (#797)

### Improvements

- Removed unused **pnpm setup** steps from the `global-install` CI job, simplifying the workflow configuration (#798)
