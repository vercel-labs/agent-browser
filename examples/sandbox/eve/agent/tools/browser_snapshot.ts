import { runAgentBrowser } from "@agent-browser/sandbox/eve";
import { defineTool } from "eve/tools";
import { z } from "zod";

export default defineTool({
  description: "Open a URL in a sandboxed browser and return an accessibility snapshot.",
  inputSchema: z.object({
    url: z.string().url(),
  }),
  async execute({ url }, ctx) {
    await runAgentBrowser(ctx, ["open", url]);
    try {
      const snapshot = await runAgentBrowser(ctx, ["snapshot", "-i", "-c"], { json: false });
      return { snapshot: snapshot.stdout };
    } finally {
      await runAgentBrowser(ctx, ["close"], { json: false }).catch(() => {});
    }
  },
});
