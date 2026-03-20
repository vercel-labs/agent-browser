---
"agent-browser": patch
---

### Bug Fixes

- **Auth login readiness** - `agent-browser auth login` now waits for `networkidle` before filling credentials, reducing failures on async/SPA login pages where form fields appear after the initial `load` event.
