---
"agent-browser": minor
---

Add `replay` command for interactive DOM session recording via rrweb

New commands:
- `replay start` - Inject rrweb recorder into the current page (auto re-injects on navigation)
- `replay stop [path]` - Stop recording and generate self-contained replay HTML
- `replay status` - Show event count and recording state

Unlike video recording (`record`), DOM replays are lightweight, inspectable, and produce
self-contained HTML files with play/pause, timeline scrubbing, and speed controls (1x-8x).
The export automatically extracts CSS custom properties for accurate visual replay.
