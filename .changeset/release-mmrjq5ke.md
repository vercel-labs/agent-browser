---
"agent-browser": patch
---

### New Features

- **Brave Browser support** - Added auto-discovery of Brave Browser for CDP connections on macOS, Linux, and Windows. The agent will now automatically detect and connect to Brave alongside Chrome, Chromium, and Canary installations (#817)

### Improvements

- **Postinstall message** - The post-install message now detects existing Chrome installations on the system. If a compatible browser is found, it confirms the path and notes it will be used automatically instead of prompting an install. If no browser is detected, the warning is clearer and mentions that installation can be skipped when using `--cdp`, `--provider`, `--engine`, or `--executable-path` (#815)
