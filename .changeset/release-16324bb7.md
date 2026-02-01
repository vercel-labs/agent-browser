---
"agent-browser": patch
---

Improved daemon connection reliability by adding automatic retry logic for transient errors like connection resets, broken pipes, and temporary resource unavailability. The CLI now cleans up stale socket and PID files before starting a new daemon, and includes better detection of daemon responsiveness to handle race conditions during shutdown.
