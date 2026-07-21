import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Click (or double-click) an element on the current page.",
  inputSchema: z.object({
    doubleClick: z.boolean().default(false),
    newTab: z.boolean().default(false).describe("Open the click target in a new tab."),
    selector: z.string().describe(SELECTOR_HINT),
  }),
  async execute({ doubleClick, newTab, selector }, ctx) {
    const args = [doubleClick ? "dblclick" : "click", selector];
    if (newTab) args.push("--new-tab");
    return await runBrowser(ctx, args);
  },
});
