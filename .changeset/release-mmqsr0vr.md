---
"agent-browser": patch
---

### Bug Fixes

- **Chrome launch retry** - Chrome will now retry launching up to 3 times with a 500ms delay between attempts, improving resilience against transient startup failures (#791)
- **Remote CDP snapshot hang** - Resolved an issue where snapshots would hang indefinitely over remote CDP (WSS) connections by removing WebSocket message and frame size limits to accommodate large responses (e.g. `Accessibility.getFullAXTree`), accepting binary frames from remote proxies such as Browserless, and immediately clearing pending commands when the connection closes rather than waiting for the 30-second timeout (#792)
