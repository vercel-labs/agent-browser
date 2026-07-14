import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    "Close the browser session and free its resources. Call when you are done with browser work; the next navigate call starts a fresh browser.",
  inputSchema: z.object({}),
  async execute(_input, ctx) {
    return await runBrowser(ctx, ["close"]);
  },
});
