---
"agent-browser": patch
---

### Bug Fixes

- **Stale accessibility tree reference fallback** - Fixed an issue where interacting with an element whose **`backend_node_id`** had become stale (e.g. after the DOM was replaced) would fail with a `Could not compute box model` CDP error. Element resolution now re-queries the accessibility tree using role/name lookup to obtain a fresh node ID before retrying the operation (#806)
