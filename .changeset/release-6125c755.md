---
"agent-browser": patch
---

Added support for chrome:// and chrome-extension:// URLs in navigation and recording commands. These special browser URLs are now preserved as-is instead of having https:// incorrectly prepended.
