---
"agent-browser": patch
---

### Bug Fixes

- **Browserbase session creation** - Fixed 415 "Unsupported Media Type" error when using `-p browserbase` by sending an empty JSON body with the session creation request. Regression introduced in #625 which removed the JSON body along with the `projectId` field.
