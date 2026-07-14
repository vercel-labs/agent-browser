import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Select an option in a <select> dropdown by value.",
  inputSchema: z.object({
    selector: z.string().describe(SELECTOR_HINT),
    value: z.string().describe("The option value to select."),
  }),
  async execute({ selector, value }, ctx) {
    return await runBrowser(ctx, ["select", selector, value]);
  },
});
