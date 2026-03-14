---
"agent-browser": patch
---

### Bug Fixes

- Fixed **AX tree deserialization** to accept integer `nodeId` and `childIds` values for compatibility with Lightpanda, which sends numeric IDs where Chrome sends strings (#775)
- Fixed **misleading SIGPIPE comment** to accurately describe the default Rust SIGPIPE behavior and why it is reset to `SIG_DFL` (#776)
- Fixed **WebM recording output** to use the VP9 codec (`libvpx-vp9`) instead of H.264, producing valid WebM files; also adds a padding filter to ensure even frame dimensions (#779)
