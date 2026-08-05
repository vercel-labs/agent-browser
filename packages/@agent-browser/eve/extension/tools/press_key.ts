import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    'Press a key or key combination in the browser, e.g. "Enter", "Tab", "Escape", "Control+a".',
  inputSchema: z.object({
    key: z.string().describe('Key name or combination like "Enter" or "Control+a".'),
  }),
  async execute({ key }, ctx) {
    return await runBrowser(ctx, ["press", key]);
  },
});
