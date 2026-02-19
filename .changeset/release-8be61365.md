---
"agent-browser": minor
---

Add annotated screenshots with the new --annotate flag, which overlays numbered labels on interactive elements and prints a legend mapping each label to its element ref. This enables multimodal AI models to reason about visual layout while using the same @eN refs for subsequent interactions. The flag can also be set via the AGENT_BROWSER_ANNOTATE environment variable.
