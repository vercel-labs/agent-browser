---
"agent-browser": minor
---

feat: configurable screencast resolution via environment variables

Add `AGENT_BROWSER_STREAM_MAX_WIDTH`, `AGENT_BROWSER_STREAM_MAX_HEIGHT`, `AGENT_BROWSER_STREAM_QUALITY`, and `AGENT_BROWSER_STREAM_FORMAT` environment variables to configure the CDP `Page.startScreencast` parameters.

Previously, the screencast was hardcoded to 1280×720 which causes significant quality loss for portrait mobile viewports on HiDPI displays. The `maxHeight: 720` cap forces CDP to downscale a portrait frame (e.g., 393×852 on iPhone 15) to ~333×720, losing resolution that cannot be recovered by the consumer.

With these new env vars, consumers can match the stream resolution to their target display (e.g., `AGENT_BROWSER_STREAM_MAX_HEIGHT=2560` for 3× Retina mobile).
