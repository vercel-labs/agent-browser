---
"agent-browser": patch
---

### Bug Fixes

- **Daemon panic on broken stderr pipe** - Replaced all `eprintln!` calls with `writeln!(std::io::stderr(), ...)` wrapped in `let _ =` to silently discard write errors, preventing the daemon from panicking when the parent process drops the stderr pipe during Chrome launch (#802)
