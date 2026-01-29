---
"agent-browser": patch
---

Fixed version synchronization to automatically update Cargo.lock alongside Cargo.toml during releases, and made the CLI binary executable. This ensures the Rust CLI version stays in sync with the npm package version.
