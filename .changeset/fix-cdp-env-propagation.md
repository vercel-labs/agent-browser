---
"agent-browser": patch
---

fix: propagate --cdp flag to daemon for reliable CDP reconnection

When using the --cdp flag, the CDP endpoint was not being passed to the daemon process via environment variables. This caused auto-reconnection to fail after connection drops, as auto_launch() checks AGENT_BROWSER_CDP but it was never set.

This fix ensures the --cdp value is propagated to the daemon, enabling reliable CDP connection recovery.
