import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Hover the mouse over an element (to reveal menus, tooltips, etc.).",
  inputSchema: z.object({
    selector: z.string().describe(SELECTOR_HINT),
  }),
  async execute({ selector }, ctx) {
    return await runBrowser(ctx, ["hover", selector]);
  },
});
