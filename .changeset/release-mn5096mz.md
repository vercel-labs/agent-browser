---
"agent-browser": patch
---

### New Features

- **Dialog status command** - Added `dialog status` command to check whether a JavaScript dialog is currently open (#999)
- **Dialog warning field** - Command responses now include a `warning` field when a JavaScript dialog is pending, indicating the dialog type and message (#999)

### Improvements

- **Standard proxy environment variables** - The proxy setting now automatically falls back to standard environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and their lowercase variants), with `NO_PROXY`/`no_proxy` respected for bypass rules (#1000)
- **Font packages for `--with-deps`** - Installing with `--with-deps` now includes CJK and emoji font packages on Linux (Debian, RPM, and yum-based distros) to prevent missing glyphs when rendering international content (#1002)

### Bug Fixes

- Fixed `state show` always failing with "Missing 'path' parameter" due to a mismatched JSON field name (`filename` → `path`) (#994)
- Fixed `console` command returning only `Done` due to a JSON field name mismatch in the response (#986)
- Fixed browser-domain CDP events being dropped during downloads due to a `sessionId` mismatch (#998)
- Fixed proxy authentication by handling credentials via the CDP `Fetch.authRequired` event rather than passing them inline (#1000)
