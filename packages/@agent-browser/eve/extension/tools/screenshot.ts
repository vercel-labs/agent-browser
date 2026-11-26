import { catalogTool } from "../lib/catalog";
import { inlineScreenshot, type ScreenshotOutput } from "../lib/screenshot";

export default catalogTool<ScreenshotOutput>("screenshot", {
  output: inlineScreenshot,
  // The image goes to channels/UIs via the tool output. When it is shown to
  // the user automatically, the model needs neither the base64 nor the sandbox
  // path — surfacing the path invites replies like "saved it at /workspace/…".
  toModelOutput(output) {
    const { imageDataUrl, ...rest } = output;
    if (imageDataUrl === undefined) {
      return { type: "json", value: rest };
    }
    const { path: _path, ...visible } = rest;
    return {
      type: "json",
      value: { ...visible, screenshot: "Captured and already displayed to the user." },
    };
  },
});
