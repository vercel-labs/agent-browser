---
"agent-browser": patch
---

### Bug Fixes

- **Restored WebSocket streaming** - Fixed broken WebSocket streaming in the native daemon by keeping the **StreamServer** instance alive so the broadcast channel remains open, and ensuring CDP session IDs and connection status are correctly propagated to stream clients (#826)
- **Filtered internal Chrome targets** - Fixed auto-connect discovery incorrectly attempting to attach to Chrome-internal pages (e.g. `chrome://`, `chrome-extension://`, `devtools://` URLs), which could cause unexpected connection failures (#827)
