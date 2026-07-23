import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description:
    "Scroll the page or a scrollable container, or scroll an element into view (pass only a selector).",
  inputSchema: z.object({
    direction: z
      .enum(["up", "down", "left", "right"])
      .optional()
      .describe("Omit to scroll the selector's element into view instead."),
    pixels: z.number().int().positive().optional().describe("Scroll distance in pixels."),
    selector: z
      .string()
      .optional()
      .describe(`Scroll container, or the element to bring into view. ${SELECTOR_HINT}`),
  }),
  async execute({ direction, pixels, selector }, ctx) {
    if (direction === undefined) {
      if (selector === undefined) {
        throw new Error("Provide a direction to scroll, or a selector to scroll into view.");
      }
      return await runBrowser(ctx, ["scrollintoview", selector]);
    }
    const args = ["scroll", direction];
    if (pixels !== undefined) args.push(String(pixels));
    if (selector !== undefined) args.push("--selector", selector);
    return await runBrowser(ctx, args);
  },
});
