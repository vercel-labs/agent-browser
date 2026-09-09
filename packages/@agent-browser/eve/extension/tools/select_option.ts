import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Select an option in a native select or supported ARIA combobox/listbox by exact value, label, or normalized accessible text.",
  inputSchema: z.object({
    selector: z.string().describe(SELECTOR_HINT),
    value: z.string().describe("The exact option value, label, or normalized accessible text to select."),
  }),
  async execute({ selector, value }, ctx) {
    return await runBrowser(ctx, ["select", selector, value]);
  },
});
