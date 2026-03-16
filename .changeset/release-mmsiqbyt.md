---
"agent-browser": patch
---

### Bug Fixes

- Fixed **`snapshot -C`** and **`screenshot --annotate`** hanging when connected over WSS (WebSocket Secure) due to sequential CDP round-trips per interactive element (#842)

### Performance

- **`snapshot -C` (cursor-interactive mode)** now batches CDP calls instead of issuing N×2 sequential round-trips per cursor-interactive element, preventing timeouts on high-latency WSS connections (#842)
- **`screenshot --annotate`** now batches element queries, reducing completion time from potentially 20–40s (e.g. 50+ buttons over WSS) to within expected bounds (#842)
