import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Check or uncheck a checkbox or radio input.",
  inputSchema: z.object({
    checked: z.boolean(),
    selector: z.string().describe(SELECTOR_HINT),
  }),
  async execute({ checked, selector }, ctx) {
    return await runBrowser(ctx, [checked ? "check" : "uncheck", selector]);
  },
});
