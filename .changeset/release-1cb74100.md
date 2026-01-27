---
"agent-browser": patch
---

Replaced shell-based CLI wrappers with a cross-platform Node.js wrapper to enable npx support on Windows. Added postinstall logic to patch npm's bin entry on global installs, allowing the native binary to be invoked directly with zero overhead. Added CI tests to verify global installation works correctly across all platforms.
