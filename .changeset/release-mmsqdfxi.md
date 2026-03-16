---
"agent-browser": patch
---

### Bug Fixes

- **Network idle detection for cached pages** - Fixed an issue where `poll_network_idle` could return immediately when no network events were observed (e.g. pages served from cache). The idle timer is now only satisfied after a consistent **500 ms idle period** has elapsed, preventing false-positive idle detection. The core polling logic has also been extracted into a standalone `poll_network_idle` function to improve testability (#847)
