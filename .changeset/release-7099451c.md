---
"agent-browser": patch
---

Allow null values for the screenshot selector field. Previously, passing a null selector would fail validation, but now it is properly handled as an optional value.
