import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Type text into an input, textarea, or contenteditable element.",
  inputSchema: z.object({
    clear: z
      .boolean()
      .default(true)
      .describe("Clear the field first. Set false to append to the existing value."),
    selector: z.string().describe(SELECTOR_HINT),
    text: z.string(),
  }),
  async execute({ clear, selector, text }, ctx) {
    return await runBrowser(ctx, [clear ? "fill" : "type", selector, text]);
  },
});
