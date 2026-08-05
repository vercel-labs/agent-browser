import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description:
    "Attach files to a file input. Paths refer to files inside the sandbox (e.g. under /workspace).",
  inputSchema: z.object({
    files: z.array(z.string()).min(1).describe("Sandbox file paths to attach."),
    selector: z.string().describe(SELECTOR_HINT),
  }),
  async execute({ files, selector }, ctx) {
    return await runBrowser(ctx, ["upload", selector, ...files]);
  },
});
