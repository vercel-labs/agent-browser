---
"agent-browser": patch
---

Fix all Clippy lint warnings in the Rust CLI: remove redundant import, use `.first()` instead of `.get(0)`, use `.copied()` instead of `.map(|s| *s)`, use `.contains()` instead of `.iter().any()`, use `then_some` instead of lazy `then`, and simplify redundant match guards.
