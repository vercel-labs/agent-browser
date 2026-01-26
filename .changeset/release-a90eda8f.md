---
"agent-browser": patch
---

Fixed the Windows CMD wrapper to use the native binary directly instead of routing through Node.js, improving startup performance and reliability. Added retry logic to the CI install command to handle transient failures during browser installation.
