---
"agent-browser": patch
---

### Bug Fixes

- **Auth login readiness** - `agent-browser auth login` now navigates with `load` and waits for login form selectors to appear before filling credentials, reducing failures on async/SPA login pages and avoiding `networkidle` hangs on pages with continuous background requests.
