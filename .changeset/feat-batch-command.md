---
"agent-browser": minor
---

Add `batch` command for executing multiple commands from stdin in a single invocation. Accepts a JSON array of string arrays and returns results sequentially. Supports `--bail` to stop on first error and `--json` for structured output.
