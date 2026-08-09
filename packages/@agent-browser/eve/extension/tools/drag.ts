import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description: "Drag an element and drop it onto another element.",
  inputSchema: z.object({
    source: z.string().describe(`Element to drag. ${SELECTOR_HINT}`),
    target: z.string().describe(`Element to drop onto. ${SELECTOR_HINT}`),
  }),
  async execute({ source, target }, ctx) {
    return await runBrowser(ctx, ["drag", source, target]);
  },
});
