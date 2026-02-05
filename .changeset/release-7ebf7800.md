---
"agent-browser": patch
---

Added --allow-file-access flag to enable opening and interacting with local file:// URLs (PDFs, HTML files) by passing Chromium flags that allow JavaScript access to local files. Added -C/--cursor flag for snapshots to include cursor-interactive elements like divs with onclick handlers or cursor:pointer styles, which is useful for modern web apps using custom clickable elements.
