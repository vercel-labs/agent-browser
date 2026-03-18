---
"agent-browser": patch
---

### Bug Fixes

- **Deduplicate text content in snapshots** - Fixed an issue where duplicate text content appeared in page snapshots (#909)
- **Native mouse drag state** - Fixed incorrect raw native mouse drag state not being properly tracked across `down`, `move`, and `up` events (#872)
- **Chrome headless launch failures** - Fixed browser launch failures caused by the `--enable-unsafe-swiftshader` flag in Chrome headless mode (#915)
- **Origin-scoped `--headers` persistence** - Restored correct persistence of origin-scoped headers set via `--headers` across navigation commands (#894)
- **Relative URLs in WebSocket domain filter** - Fixed handling of relative URLs in the WebSocket domain filter script (#624)
