---
"agent-browser": patch
---

### Bug Fixes

- Fixed **video duration** being reported incorrectly when using real-time ffmpeg encoding for screen recording (#812)
- Removed obsolete **`BrowserManager` TypeScript API** references that no longer reflect the current CLI-based usage model (#821)

### Documentation

- Updated README to replace outdated **`BrowserManager` programmatic API** examples with the current CLI-based approach using `execSync` and `agent-browser` commands (#821)
- Removed the **Programmatic API** section covering `BrowserManager` screencast and input injection methods, which are no longer part of the public API (#821)
