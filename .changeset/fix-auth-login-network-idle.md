---
"agent-browser": patch
---

### Bug Fixes

- **Auth login readiness** - `agent-browser auth login` now navigates with `load`, waits for usable login form selectors, and uses staged username detection (targeted email/username selectors first, then broad text-input fallback). This reduces SPA timing failures, avoids false matches on unrelated text fields, and prevents `networkidle` hangs on pages with continuous background requests.
