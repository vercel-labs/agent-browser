---
"agent-browser": minor
---

Added session persistence with automatic save/restore of cookies and localStorage across browser restarts using --session-name flag, with optional AES-256-GCM encryption for saved state data. New state management commands allow listing, showing, renaming, clearing, and cleaning up old session files. Also added --new-tab option for click commands to open links in new tabs.
