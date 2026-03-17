---
"agent-browser": patch
---

Fix daemon detection for PID namespace isolation (e.g. `unshare`). Use socket connectivity as the sole liveness check instead of `kill(pid, 0)`, which fails when the caller cannot see the daemon's PID.
