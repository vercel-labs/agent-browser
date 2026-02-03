---
"agent-browser": patch
---

Add --stdin flag for the eval command to read JavaScript from stdin, enabling heredoc usage for multiline scripts. Also fix binary permission issues on macOS/Linux when postinstall scripts don't run (e.g., with bun).
