---
"agent-browser": patch
---

### Bug Fixes

- **Appium v3 iOS capabilities** - Added `appium:` vendor prefix to iOS capabilities (e.g., `appium:automationName`, `appium:deviceName`, `appium:platformVersion`) to comply with the Appium v3 WebDriver protocol requirements (#810)
- **Snapshot `--selector` scoping** - Fixed `snapshot --selector` so that the output is properly scoped to the matched element's subtree rather than returning the full accessibility tree. The selector now resolves the target DOM node's backend IDs and filters the accessibility tree to only include nodes within that subtree (#825)
