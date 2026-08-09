import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    "Navigate the sandboxed browser: open a URL, or go back/forward/reload. Launches the browser on first use. After navigating, call the snapshot tool to see the page.",
  inputSchema: z.object({
    action: z
      .enum(["goto", "back", "forward", "reload"])
      .default("goto")
      .describe('"goto" opens the given URL; the others act on history.'),
    url: z.string().optional().describe('Required when action is "goto".'),
  }),
  async execute({ action, url }, ctx) {
    if (action === "goto") {
      if (url === undefined) {
        throw new Error('The "goto" action requires a url.');
      }
      return await runBrowser(ctx, ["open", url]);
    }
    return await runBrowser(ctx, [action]);
  },
});
