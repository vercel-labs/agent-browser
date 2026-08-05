import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    "Run a JavaScript expression in the page and return its result. The result must be JSON-serializable.",
  inputSchema: z.object({
    expression: z
      .string()
      .describe('JavaScript to evaluate, e.g. "document.querySelectorAll(\'a\').length".'),
  }),
  async execute({ expression }, ctx) {
    return await runBrowser(ctx, ["eval", expression]);
  },
});
