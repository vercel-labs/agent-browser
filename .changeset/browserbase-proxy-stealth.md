---
"agent-browser": patch
---

### New Features

- **Browserbase proxy support** - Enable Browserbase's residential proxy via `BROWSERBASE_PROXY=1` environment variable to route traffic through residential IPs instead of datacenter IPs
- **Browserbase advanced stealth** - Enable advanced stealth mode via `BROWSERBASE_ADVANCED_STEALTH=1` and configure OS fingerprint via `BROWSERBASE_OS` (windows, mac, linux, mobile, tablet)
- **Image blocking for providers** - Block image loading to save proxy bandwidth via `BROWSERBASE_BLOCK_IMAGES=1` environment variable, using CDP Fetch interception to abort image requests (note: not recommended on sites with anti-bot protection)

### Bug Fixes

- **Browserbase session creation** - Fixed 415 "Unsupported Media Type" error when using `-p browserbase` by sending a JSON body with the session creation request (regression from #625)
