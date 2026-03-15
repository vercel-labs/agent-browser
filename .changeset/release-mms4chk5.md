---
"agent-browser": patch
---

### Bug Fixes

- **Material Design checkbox/radio parity** - Restored Playwright-parity behavior for `check`/`uncheck` actions on Material Design controls. These components hide the native `<input>` off-screen and use overlay elements that intercept coordinate-based clicks; the actions now detect this pattern and fall back to a JS `.click()` to correctly toggle state. Also improves `ischecked` to handle nested hidden inputs and ARIA-only checkboxes (#837)
- **Punctuation handling in `type` command** - Fixed incorrect virtual key (VK) codes being used for punctuation characters (e.g. `.`, `@`) in the `type` action, which previously caused those characters to be dropped or mistyped (#836)
